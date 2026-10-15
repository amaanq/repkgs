//! Test steps that passed, kept in mummerd under the files they read, so a
//! rebuild whose tree and inputs reproduce a passing run skips the step. A
//! traced run learns what the step read, and the request key holds the rest,
//! the step, the package's options, the environment and bigtop's code.

use std::{
   collections::{
      BTreeMap,
      BTreeSet,
      HashMap,
      hash_map::Entry as Slot,
   },
   env,
   ffi::OsStr,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   fs::{
      metadata,
      read,
      read_dir,
      read_to_string,
      symlink_metadata,
      write,
   },
   io::{
      self,
      ErrorKind,
   },
   mem::{
      Discriminant,
      discriminant,
   },
   ops::Not,
   os::unix::ffi::OsStrExt as _,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      ExitStatus,
   },
   str::FromStr,
};

use mummer::{
   store::{
      InputId,
      STORE_DIR,
      Store,
      is_store_path,
      mask_hashes,
   },
   tree::note,
};
use ringmasterd::Escape;
use serde::{
   Deserialize,
   Serialize,
};
use serde_json::Value;
use tightrope::{
   client::Client,
   identity::Identity,
   key::Key,
};

use crate::{
   attrs::{
      Ctx,
      Kind,
      Step,
      build_top,
   },
   output::{
      RUNNER,
      fingerprint,
   },
   probe::inherited,
   ring::Session,
   trace::{
      self,
      Accesses,
      Mark,
      Outcome,
      Readers,
      Tests,
   },
};

const MANIFESTS: usize = 8;
/// Request keys of one step kept apart, other environments or options.
const REQUESTS: usize = 4;
/// How many a list holds at most, when the cache keeps entries past the
/// limit because they were read recently.
const SPARE: usize = 4;

/// Kernel views and devices, never files a build wrote.
const PSEUDO: [&str; 3] = ["/proc", "/sys", "/dev"];

#[derive(Serialize)]
struct Request<'step> {
   step:        &'step str,
   kind:        &'step Kind,
   spec:        &'step Value,
   environment: BTreeMap<String, String>,
   runner:      &'static str,
   module:      Option<&'static str>,
   registry:    Option<String>,
}

/// A path as stored, its bytes when they are not UTF-8, as the names test
/// suites make to exercise encodings are.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(untagged)]
enum Spelling {
   Text(String),
   Raw(Vec<u8>),
}

impl Spelling {
   fn of(store: &Store, path: &Path) -> Self {
      path.to_str().map_or_else(
         || Self::Raw(path.as_os_str().as_bytes().to_vec()),
         |text| Self::Text(spelled(store, text)),
      )
   }

   fn resolve(&self, store: &Store) -> Option<PathBuf> {
      match *self {
         Self::Text(ref masked) => store.resolve(masked).map(PathBuf::from),
         Self::Raw(ref bytes) => Some(PathBuf::from(OsStr::from_bytes(bytes))),
      }
   }
}

impl Display for Spelling {
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      match *self {
         Self::Text(ref text) => f.write_str(text),
         Self::Raw(ref bytes) => write!(f, "{}", Path::new(OsStr::from_bytes(bytes)).display()),
      }
   }
}

/// What a path held when the step reached it.
#[derive(Clone, Deserialize, Serialize)]
enum Fact {
   File(InputId),
   /// Something other than a directory, whatever it holds, for a path the
   /// harness only looked up.
   Present,
   Absent,
   Directory,
   /// The entry names, as `names` digests them.
   Listing(String),
   /// A file the build made that the step read and then replaced before the
   /// tracer could keep what it held, so no tree matches it.
   Replaced,
}

/// A path the step reached and who reached it, tests by their index in
/// `Manifest::units`.
#[derive(Clone, Deserialize, Serialize)]
struct Entry {
   path:       Spelling,
   fact:       Fact,
   /// Reached by those tests alone, nothing outside a test reached it.
   #[serde(default, skip_serializing_if = "<&bool as Not>::not")]
   only_tests: bool,
   #[serde(default, skip_serializing_if = "Vec::is_empty")]
   units:      Vec<u32>,
}

#[derive(Deserialize, Serialize)]
struct Manifest {
   /// The tests the run passed, none for a step not divided into tests.
   #[serde(default, skip_serializing_if = "Vec::is_empty")]
   units:   Vec<String>,
   entries: Vec<Entry>,
   /// The step replaces files it read, so its runs must be traced by
   /// seccomp, which sees each call before it happens.
   #[serde(default, skip_serializing_if = "<&bool as Not>::not")]
   seccomp: bool,
}

enum Miss {
   NewKey,
   InputChanged(Spelling),
   Vanished(Spelling),
   Appeared(Spelling),
   NotDirectory(Spelling),
   ListingChanged(Spelling),
   Replaced(Spelling),
}

impl Miss {
   fn of(entry: &Entry) -> Self {
      let path = entry.path.clone();
      match entry.fact {
         Fact::File(_) => Self::InputChanged(path),
         Fact::Present => Self::Vanished(path),
         Fact::Absent => Self::Appeared(path),
         Fact::Directory => Self::NotDirectory(path),
         Fact::Listing(_) => Self::ListingChanged(path),
         Fact::Replaced => Self::Replaced(path),
      }
   }
}

impl Display for Miss {
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      match *self {
         Self::NewKey => f.write_str("new-key"),
         Self::InputChanged(ref path) => write!(f, "inputs-changed:{path}"),
         Self::Vanished(ref path) => write!(f, "vanished:{path}"),
         Self::Appeared(ref path) => write!(f, "appeared:{path}"),
         Self::NotDirectory(ref path) => write!(f, "not-a-directory:{path}"),
         Self::ListingChanged(ref path) => write!(f, "listing-changed:{path}"),
         Self::Replaced(ref path) => write!(f, "replaced:{path}"),
      }
   }
}

/// What a stored pass says of the tree as it is now.
enum Verdict {
   Hit,
   /// Only these tests read something that changed.
   Stale(BTreeSet<String>),
   Miss(Miss),
}

enum Found {
   Hit,
   Stale(Manifest, BTreeSet<String>),
   Miss(Miss),
}

/// The environment variable naming the file a test phase reads the tests to
/// run from, one per line, when only some need to. A file because a large
/// selection overflows exec's 128 KiB limit on one environment string.
const SELECTED: &str = "BIGTOP_TESTS";
const SELECTED_FILE: &str = ".bigtop-tests";

/// The tests a test phase should limit itself to, or none to run them all.
pub fn selected<T>() -> io::Result<Option<Vec<T>>>
where
   T: FromStr<Err: Display>,
{
   let Ok(file) = env::var(SELECTED) else {
      return Ok(None);
   };
   read_to_string(&file)
      .map_err(|error| io::Error::other(format!("{file}: {error}")))?
      .lines()
      .map(|line| {
         line
            .parse()
            .map_err(|error| io::Error::other(format!("{line}: {error}")))
      })
      .collect::<io::Result<_>>()
      .map(Some)
}

pub enum Ran {
   Skipped,
   Passed,
}

fn request(ctx: &Ctx, step: &Step, module: Option<&'static str>) -> io::Result<Key> {
   let fingerprinted = module
      .map(|path| {
         fingerprint(path.rsplit("::").next().unwrap_or(path))
            .ok_or_else(|| io::Error::other(format!("bigtop has no fingerprint for {path}")))
      })
      .transpose()?;
   // a new release changes what the tests read, which the reads catch
   let mut spec = ctx.spec.clone();
   if let Some(fields) = spec.as_object_mut() {
      fields.remove("version");
   }
   // the registry serves from bigtop, outside the traced reads
   let served = spec
      .pointer("/tests/registry")
      .and_then(Value::as_str)
      .map(|deps| read(Path::new(deps).join("package-lock.json")))
      .transpose()?
      .map(|lock| blake3::hash(&lock).to_hex().to_string());
   let request = Request {
      step:        &step.name,
      kind:        &step.kind,
      spec:        &spec,
      environment: env::vars().filter(|entry| inherited(&entry.0)).collect(),
      runner:      RUNNER,
      module:      fingerprinted,
      registry:    served,
   };
   let text = mask_hashes(&serde_json::to_string(&request)?);
   format!(
      "test/{}/{}",
      ctx.spec()?.name,
      blake3::hash(text.as_bytes()).to_hex()
   )
   .parse()
}

/// A path as stored, its store hash masked when this build's roots give the
/// path back, and as it is otherwise, a store path then naming its contents.
fn spelled(store: &Store, path: &str) -> String {
   let masked = store.key(path);
   if store.resolve(&masked).as_deref() == Some(path) {
      masked
   } else {
      path.to_owned()
   }
}

/// A file's identity as `Store::input_id` gives it, for a path that is not
/// UTF-8 and so is neither masked nor known to the daemon.
fn raw_identity(store: &Store, path: &Path) -> io::Result<InputId> {
   Ok(held(store, path, read(path)?))
}

/// What `Store::input_id` makes of `path` holding `bytes`.
fn held(store: &Store, path: &Path, bytes: Vec<u8>) -> InputId {
   match path.to_str() {
      Some(text) => store.content_id(text, bytes),
      None if path.starts_with(STORE_DIR) => InputId::Content(Identity::of(&bytes)),
      None => store.content_id("", bytes),
   }
}

/// A file's identity now.
fn identity(store: &Store, path: &Path) -> io::Result<InputId> {
   path
      .to_str()
      .map_or_else(|| raw_identity(store, path), |text| store.input_id(text))
}

fn entry(store: &Store, units: &[String], path: &Path, fact: Fact, readers: &Readers) -> Entry {
   Entry {
      path: Spelling::of(store, path),
      fact,
      only_tests: !readers.shared,
      units: readers
         .units
         .iter()
         .filter_map(|name| units.binary_search(name).ok())
         .filter_map(|at| u32::try_from(at).ok())
         .collect(),
   }
}

/// The tree under `root` before a step runs, each directory's listing as
/// `names` gives it and every other path. Entries the step deletes or
/// replaces leave no trace to rebuild a listing from afterwards, and a test
/// can backdate what it makes past the mark, as GNU make's do.
struct Snapshot {
   root:     PathBuf,
   listings: BTreeMap<PathBuf, String>,
   files:    BTreeSet<PathBuf>,
}

impl Snapshot {
   fn take(root: PathBuf) -> io::Result<Self> {
      let mut listings = BTreeMap::new();
      let mut files = BTreeSet::new();
      let mut pending = vec![root.clone()];
      while let Some(directory) = pending.pop() {
         for listed in read_dir(&directory)? {
            let entry = listed?;
            if entry.file_type()?.is_dir() {
               pending.push(entry.path());
            } else {
               files.insert(entry.path());
            }
         }
         let digest = names(&directory, &|_entry| true)?;
         listings.insert(directory, digest);
      }
      Ok(Self {
         root,
         listings,
         files,
      })
   }

   /// Whether `path` predates the step. Anything outside the tree does.
   fn had(&self, path: &Path) -> bool {
      !path.starts_with(&self.root) || self.files.contains(path) || self.listings.contains_key(path)
   }

   /// Whether the step made `path`, which it now holds.
   fn made(&self, path: &Path, mark: Mark) -> bool {
      symlink_metadata(path).is_ok_and(|meta| mark.written(&meta) || !self.had(path))
   }
}

/// The entry names of a directory as one digest, those `kept` admits, and
/// none when it is gone, as one the step made and removed is.
fn names(directory: &Path, kept: &dyn Fn(&Path) -> bool) -> io::Result<String> {
   let mut hasher = blake3::Hasher::new();
   let entries = match read_dir(directory) {
      Ok(entries) => entries,
      Err(error) if error.kind() == ErrorKind::NotFound => {
         return Ok(hasher.finalize().to_hex().to_string());
      },
      Err(error) => return Err(error),
   };
   let mut sorted = BTreeSet::new();
   for entry in entries {
      let path = entry?.path();
      if kept(&path)
         && let Some(name) = path.file_name()
      {
         sorted.insert(name.to_owned());
      }
   }
   for name in sorted {
      hasher.update(name.as_encoded_bytes());
      hasher.update(b"\0");
   }
   Ok(hasher.finalize().to_hex().to_string())
}

impl Manifest {
   /// The files bigtop itself reads around a step.
   fn own(top: &str) -> io::Result<Vec<PathBuf>> {
      let mut own = vec![
         env::current_exe()?,
         Path::new(top).join("ctx.json"),
         Path::new(top).join(".bigtop-modules"),
         Path::new(top).join("phase-env.json"),
         Path::new(top).join(SELECTED_FILE),
      ];
      own.extend(
         ["NIX_ATTRS_JSON_FILE", "NIX_ATTRS_SH_FILE"]
            .into_iter()
            .filter_map(|name| env::var(name).ok().map(PathBuf::from)),
      );
      Ok(own)
   }

   /// What the step read and the build had made before it ran, or why the
   /// accesses cannot stand for that.
   fn build(
      Traced {
         accesses,
         mut before,
         mark,
         tests,
      }: Traced,
      client: &mut Client,
      store: &mut Store,
   ) -> io::Result<Result<Self, String>> {
      if let Some(why) = accesses.escaped {
         return Ok(Err(why.to_owned()));
      }
      if accesses
         .deep
         .iter()
         .any(|directory| before.had(Path::new(directory)))
      {
         return Ok(Err(Escape::Unresolved.reason().to_owned()));
      }
      let top = build_top()?;
      let own = Self::own(&top)?;
      let ignored = |path: &Path| {
         own.iter().any(|mine| mine == path)
            || PSEUDO.iter().any(|root| path.starts_with(root))
            || tests.is_some_and(|harness| harness.state(path))
      };
      let written = |path: &Path| before.made(path, mark);
      let mut paths = accesses.paths;
      let listed = accesses.listed;
      let mut prior = accesses.prior;
      prior.retain(|path, _bytes| before.files.contains(Path::new(path)));
      let mut replaced = accesses.replaced;
      replaced.retain(|path| before.files.contains(Path::new(path)) && !ignored(Path::new(path)));
      let forced = !replaced.is_empty() || !prior.is_empty();
      let units = paths
         .values()
         .chain(listed.values())
         .flat_map(|readers| readers.units.iter().cloned())
         .collect::<BTreeSet<_>>()
         .into_iter()
         .collect::<Vec<_>>();
      let mut entries = Vec::new();
      for spelled in accesses.looked_up {
         if units.is_empty() {
            paths.entry(spelled).or_default().shared = true;
            continue;
         }
         let path = PathBuf::from(spelled);
         if paths.contains_key(path.as_os_str()) || ignored(&path) || written(&path) {
            continue;
         }
         if let Some(fact) = Self::presence(&path, &mark) {
            entries.push(entry(store, &units, &path, fact, &Readers::outside()));
         }
      }
      let mut files = Vec::new();
      for (spelled, readers) in paths {
         let path = PathBuf::from(spelled);
         if ignored(&path) {
            continue;
         }
         if replaced.remove(path.as_os_str()) {
            entries.push(entry(store, &units, &path, Fact::Replaced, &readers));
            continue;
         }
         if let Some(bytes) = prior.remove(path.as_os_str()) {
            let fact = Fact::File(held(store, &path, bytes));
            entries.push(entry(store, &units, &path, fact, &readers));
            continue;
         }
         if written(&path) {
            continue;
         }
         match metadata(&path) {
            Ok(meta) if mark.written(&meta) => {},
            Ok(meta) if meta.is_dir() => {
               entries.push(entry(store, &units, &path, Fact::Directory, &readers));
            },
            Ok(meta) if meta.is_file() => files.push((path, readers)),
            Err(error) if error.kind() == ErrorKind::NotFound => {
               entries.push(entry(store, &units, &path, Fact::Absent, &readers));
            },
            Ok(_) | Err(_) => {},
         }
      }
      entries.extend(Self::contents(client, store, &units, files)?);
      for (spelled, readers) in listed {
         let path = PathBuf::from(spelled);
         if ignored(&path) {
            continue;
         }
         let names = if path.starts_with(&top) {
            let Some(found) = before.listings.remove(&path) else {
               continue;
            };
            found
         } else {
            names(&path, &|_entry| true)?
         };
         entries.push(entry(store, &units, &path, Fact::Listing(names), &readers));
      }
      Ok(Ok(Self {
         units,
         entries,
         seccomp: forced,
      }))
   }

   /// The entries of files the step read, their identities asked of the
   /// daemon together.
   fn contents(
      client: &mut Client,
      store: &mut Store,
      units: &[String],
      files: Vec<(PathBuf, Readers)>,
   ) -> io::Result<Vec<Entry>> {
      let texts = files
         .iter()
         .filter_map(|pair| pair.0.to_str().map(str::to_owned))
         .collect::<Vec<_>>();
      prefetch(client, store, &texts)?;
      let mut entries = Vec::new();
      for (path, readers) in files {
         let spelling = Spelling::of(store, &path);
         let fact = match (path.to_str(), spelling) {
            (Some(text), Spelling::Text(spelled)) if is_store_path(text) && spelled == text => {
               Fact::File(InputId::Path(spelled))
            },
            _ => Fact::File(identity(store, &path)?),
         };
         entries.push(entry(store, units, &path, fact, &readers));
      }
      Ok(entries)
   }

   /// Whether a path the harness only looked up exists and as what, none
   /// when the run made it.
   fn presence(path: &Path, mark: &Mark) -> Option<Fact> {
      match metadata(path) {
         Ok(meta) if mark.written(&meta) => None,
         Ok(meta) if meta.is_dir() => Some(Fact::Directory),
         Ok(_) => Some(Fact::Present),
         Err(error) if error.kind() == ErrorKind::NotFound => Some(Fact::Absent),
         Err(_) => None,
      }
   }

   /// Whether `path` still holds `fact`.
   fn holds(store: &Store, fact: &Fact, resolved: Option<&Path>) -> io::Result<bool> {
      let Some(path) = resolved else {
         return Ok(matches!(*fact, Fact::Absent));
      };
      Ok(match *fact {
         Fact::File(InputId::Path(ref exact)) => Path::new(exact).exists(),
         Fact::File(ref recorded) => {
            match identity(store, path) {
               Ok(found) => found == *recorded,
               Err(error) if error.kind() == ErrorKind::NotFound => false,
               Err(error) => return Err(error),
            }
         },
         Fact::Present => path.exists() && !path.is_dir(),
         Fact::Absent => !path.exists(),
         Fact::Directory => path.is_dir(),
         Fact::Listing(ref digest) => names(path, &|_entry| true)? == *digest,
         Fact::Replaced => false,
      })
   }

   fn matches(&self, client: &mut Client, store: &mut Store) -> io::Result<Verdict> {
      let resolved = self
         .entries
         .iter()
         .map(|entry| entry.path.resolve(store))
         .collect::<Vec<_>>();
      let texts = self
         .entries
         .iter()
         .zip(&resolved)
         .filter(|pair| matches!(pair.0.fact, Fact::File(_)))
         .filter_map(|pair| pair.1.as_ref()?.to_str().map(str::to_owned))
         .collect::<Vec<_>>();
      prefetch(client, store, &texts)?;
      let mut stale = BTreeSet::new();
      for (entry, path) in self.entries.iter().zip(&resolved) {
         if Self::holds(store, &entry.fact, path.as_deref())? {
            continue;
         }
         if !entry.only_tests || self.units.is_empty() {
            return Ok(Verdict::Miss(Miss::of(entry)));
         }
         stale.extend(
            entry
               .units
               .iter()
               .filter_map(|&at| self.units.get(usize::try_from(at).ok()?).cloned()),
         );
      }
      Ok(if stale.is_empty() {
         Verdict::Hit
      } else {
         Verdict::Stale(stale)
      })
   }

   /// This pass with the tests `newer` ran taken from `newer`, what the rest
   /// read kept as it was.
   fn merge(&self, newer: &Self) -> Self {
      let rerun = newer.units.iter().collect::<BTreeSet<_>>();
      let units = self
         .units
         .iter()
         .filter(|name| !rerun.contains(name))
         .chain(&newer.units)
         .cloned()
         .collect::<BTreeSet<_>>()
         .into_iter()
         .collect::<Vec<_>>();
      let mut merged = HashMap::<(Spelling, Discriminant<Fact>), Entry>::new();
      for (manifest, renewed) in [(self, false), (newer, true)] {
         for found in &manifest.entries {
            let mut moved = found
               .units
               .iter()
               .filter_map(|&at| manifest.units.get(usize::try_from(at).ok()?))
               .filter(|name| renewed || !rerun.contains(name))
               .filter_map(|name| units.binary_search(name).ok())
               .filter_map(|at| u32::try_from(at).ok())
               .collect::<Vec<_>>();
            if found.only_tests && moved.is_empty() {
               continue;
            }
            match merged.entry((found.path.clone(), discriminant(&found.fact))) {
               Slot::Vacant(slot) => {
                  slot.insert(Entry {
                     path:       found.path.clone(),
                     fact:       found.fact.clone(),
                     only_tests: found.only_tests,
                     units:      moved,
                  });
               },
               Slot::Occupied(mut slot) => {
                  let kept = slot.get_mut();
                  kept.only_tests &= found.only_tests;
                  kept.units.append(&mut moved);
                  kept.units.sort_unstable();
                  kept.units.dedup();
                  if renewed {
                     kept.fact = found.fact.clone();
                  }
               },
            }
         }
      }
      let mut entries = merged.into_values().collect::<Vec<_>>();
      entries.sort_by(|left, right| left.path.cmp(&right.path));
      Self {
         units,
         entries,
         seccomp: self.seccomp || newer.seccomp,
      }
   }
}

/// Store files' identities from the daemon, which already hashed them.
fn prefetch(client: &mut Client, store: &mut Store, paths: &[String]) -> io::Result<()> {
   let asked = paths
      .iter()
      .filter(|path| store.daemon_may_identify(path))
      .cloned()
      .collect::<Vec<String>>();
   for (path, identity) in asked.iter().zip(client.identities(&asked)?) {
      if let Some(value) = identity {
         store.remember(path.clone(), InputId::Content(value));
      }
   }
   Ok(())
}

fn stored(client: &mut Client, key: &Key) -> io::Result<Vec<Manifest>> {
   Ok(client
      .get(key)?
      .and_then(|bytes| serde_json::from_slice::<Vec<Manifest>>(&bytes).ok())
      .unwrap_or_default())
}

/// The best stored pass, and whether any says the step needs seccomp.
fn lookup(client: &mut Client, store: &mut Store, key: &Key) -> io::Result<(Found, bool)> {
   let manifests = stored(client, key)?;
   let seccomp = manifests.iter().any(|manifest| manifest.seccomp);
   let mut first = None;
   let mut best: Option<(Manifest, BTreeSet<String>)> = None;
   for manifest in manifests {
      match manifest.matches(client, store)? {
         Verdict::Hit => return Ok((Found::Hit, seccomp)),
         Verdict::Stale(names) => {
            if best.as_ref().is_none_or(|pair| names.len() < pair.1.len()) {
               best = Some((manifest, names));
            }
         },
         Verdict::Miss(miss) => {
            first.get_or_insert(miss);
         },
      }
   }
   let found = match best {
      Some((manifest, names)) => Found::Stale(manifest, names),
      None => Found::Miss(first.unwrap_or(Miss::NewKey)),
   };
   Ok((found, seccomp))
}

fn publish(client: &mut Client, key: &Key, manifest: Manifest) -> io::Result<()> {
   let mut manifests = vec![manifest];
   manifests.extend(stored(client, key)?);
   manifests.truncate(MANIFESTS);
   client.put(key, &serde_json::to_vec(&manifests)?)
}

/// The step's recent request keys, newest first. A request key changes with
/// the environment, options or bigtop, never with sources alone, so past
/// `REQUESTS` the least recently read go unless read lately.
fn lineage(client: &mut Client, ctx: &Ctx, step: &Step, key: &Key) -> io::Result<()> {
   let listing = format!("tests/{}/{}", ctx.spec()?.name, step.name).parse::<Key>()?;
   let known = client
      .get(&listing)?
      .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok())
      .unwrap_or_default()
      .into_iter()
      .filter(|other| other != key.as_str())
      .map(|other| other.parse::<Key>())
      .collect::<io::Result<Vec<Key>>>()?;
   let mut kept = Vec::new();
   if known.len() >= REQUESTS {
      let mut ranked = known
         .iter()
         .cloned()
         .zip(client.ages(&known)?)
         .filter_map(|(other, age)| Some((age?, other)))
         .collect::<Vec<_>>();
      ranked.sort_by_key(|pair| pair.0);
      let overflow = ranked
         .split_off(ranked.len().min(REQUESTS - 1))
         .into_iter()
         .map(|pair| pair.1)
         .collect::<Vec<_>>();
      kept.extend(ranked.into_iter().map(|pair| pair.1));
      for (other, dropped) in overflow.iter().zip(client.expire(&overflow)?) {
         if !dropped {
            kept.push(other.clone());
         }
      }
   } else {
      kept = known;
   }
   let mut keys = vec![String::from(key.clone())];
   keys.extend(kept.into_iter().map(String::from));
   keys.truncate(REQUESTS * SPARE);
   client.put(&listing, &serde_json::to_vec(&keys)?)
}

struct Traced {
   accesses: Accesses,
   before:   Snapshot,
   mark:     Mark,
   tests:    Option<&'static dyn Tests>,
}

/// One run of the step from `command`, limited to the `selected` tests when
/// given, traced by ringmasterd when it is mapped in and the step does not
/// need `seccomp`, otherwise by seccomp when the filter installs.
fn attempt(
   command: &dyn Fn() -> io::Result<Command>,
   tests: Option<&'static dyn Tests>,
   selected: Option<&BTreeSet<String>>,
   seccomp: bool,
) -> io::Result<(ExitStatus, Option<Traced>)> {
   let make = || {
      let mut built = command()?;
      if let Some(names) = selected {
         let file = Path::new(&build_top()?).join(SELECTED_FILE);
         let list = names
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n");
         write(&file, list)?;
         built.env(SELECTED, file);
      }
      Ok(built)
   };
   let before = Snapshot::take(PathBuf::from(build_top()?))?;
   let mark = Mark::now();
   let session = if seccomp {
      None
   } else {
      match Session::open() {
         Ok(opened) => Some(opened),
         Err(error) if error.kind() == ErrorKind::NotFound => None,
         Err(error) => {
            note("test-cache", &format!("ringmasterd unusable, {error}"));
            None
         },
      }
   };
   let outcome = match session {
      Some(opened) => opened.run(&make, tests, mark)?,
      None => trace::run(&make, tests, mark)?,
   };
   // a signal landing before the trace server takes a call still turns it
   // into EINTR, even for an open of a regular file, so a traced failure
   // may be the tracer's
   Ok(match outcome {
      Outcome::Traced(status, accesses)
         if status.success() || !(accesses.interruptible || accesses.refused) =>
      {
         let traced = Traced {
            accesses,
            before,
            mark,
            tests,
         };
         (status, Some(traced))
      },
      Outcome::Traced(status, _) => {
         if seccomp && let Ok(opened) = Session::open() {
            note(
               "test-cache",
               &format!("seccomp run failed with {status}, rerunning under ringmasterd"),
            );
            return Ok(match opened.run(&make, tests, mark)? {
               Outcome::Traced(ran, accesses) => {
                  let traced = Traced {
                     accesses,
                     before,
                     mark,
                     tests,
                  };
                  (ran, Some(traced))
               },
               Outcome::Untraced(ran, why) => {
                  note("test-cache", &format!("untraced, {why}"));
                  (ran, None)
               },
            });
         }
         note(
            "test-cache",
            &format!("traced run failed with {status}, rerunning untraced"),
         );
         (make()?.status()?, None)
      },
      Outcome::Untraced(status, why) => {
         note("test-cache", &format!("untraced, {why}"));
         (status, None)
      },
   })
}

/// Stores a passing run's reads under `key`, merged into the pass it reran
/// some tests of when there is one. Cache trouble is noted, never a failure.
fn keep(
   client: &mut Client,
   store: &mut Store,
   (ctx, step, key): (&Ctx, &Step, &Key),
   traced: Option<Traced>,
   older: Option<Manifest>,
) {
   let Some(run) = traced else {
      return;
   };
   match Manifest::build(run, client, store) {
      Ok(Ok(built)) => {
         let manifest = match older {
            Some(previous) => previous.merge(&built),
            None => built,
         };
         let counts = format!(
            "{} paths, {} tests",
            manifest.entries.len(),
            manifest.units.len()
         );
         match publish(client, key, manifest).and_then(|()| lineage(client, ctx, step, key)) {
            Ok(()) => note("test-cache", &format!("stored, {counts}")),
            Err(error) => note("test-cache", &format!("store failed with {error}")),
         }
      },
      Ok(Err(why)) => note("test-cache", &format!("not stored, {why}")),
      Err(error) => note("test-cache", &format!("not stored, {error}")),
   }
}

/// A test step skipped when a stored pass matches, otherwise run traced from
/// `command` and its pass stored. `module` is the build system's, for a step
/// that dispatches into one, and `tests` how its harness divides the step,
/// so that a pass whose changed reads belong to some tests reruns only those.
pub fn run(
   mut client: Client,
   ctx: &Ctx,
   step: &Step,
   module: Option<&'static str>,
   tests: Option<&'static dyn Tests>,
   command: &dyn Fn() -> io::Result<Command>,
) -> io::Result<Ran> {
   let key = request(ctx, step, module)?;
   let mut store = Store::new();
   let (found, seccomp) = lookup(&mut client, &mut store, &key).unwrap_or_else(|error| {
      note("test-cache", &format!("lookup failed with {error}"));
      (Found::Miss(Miss::NewKey), false)
   });
   match found {
      Found::Hit => {
         note("test-cache", "passed before, skipped");
         return Ok(Ran::Skipped);
      },
      Found::Stale(manifest, names) => {
         note(
            "test-cache",
            &format!(
               "{} of {} tests read changed files, running those",
               names.len(),
               manifest.units.len()
            ),
         );
         let (status, traced) = attempt(command, tests, Some(&names), seccomp)?;
         if status.success() {
            keep(
               &mut client,
               &mut store,
               (ctx, step, &key),
               traced,
               Some(manifest),
            );
            return Ok(Ran::Passed);
         }
         note(
            "test-cache",
            &format!("those failed with {status}, running every test"),
         );
      },
      Found::Miss(miss) => note("test-cache", &format!("miss, {miss}")),
   }
   let (status, traced) = attempt(command, tests, None, seccomp)?;
   if !status.success() {
      return Err(io::Error::other(format!(
         "phase {} failed, {status}",
         step.name
      )));
   }
   keep(&mut client, &mut store, (ctx, step, &key), traced, None);
   Ok(Ran::Passed)
}
