//! Test steps that passed, kept in mummerd under the files they read, so a
//! rebuild whose tree and inputs reproduce a passing run skips the step. A
//! traced run learns what the step read, and the request key holds the rest,
//! the step, the package's options, the environment and bigtop's code.

use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   env::{
      current_exe,
      var,
      vars,
   },
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
      symlink_metadata,
   },
   io::{
      self,
      ErrorKind,
   },
   os::unix::ffi::OsStrExt as _,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
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
      SHARED,
      fingerprint,
   },
   probe::inherited,
   trace::{
      self,
      Accesses,
      Mark,
      Outcome,
   },
};

const MANIFESTS: usize = 8;

/// Kernel views and devices, never files a build wrote.
const PSEUDO: [&str; 3] = ["/proc", "/sys", "/dev"];

#[derive(Serialize)]
struct Request<'step> {
   step:        &'step str,
   kind:        &'step Kind,
   spec:        &'step Value,
   environment: BTreeMap<String, String>,
   shared:      &'static str,
   module:      Option<&'static str>,
}

/// A path as stored, its bytes when they are not UTF-8, as the names test
/// suites make to exercise encodings are.
#[derive(Clone, Deserialize, Serialize)]
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

#[derive(Deserialize, Serialize)]
struct Input {
   path:     Spelling,
   identity: InputId,
}

#[derive(Deserialize, Serialize)]
struct Listing {
   path:  Spelling,
   names: String,
}

#[derive(Deserialize, Serialize)]
struct Manifest {
   inputs:      Vec<Input>,
   absent:      Vec<Spelling>,
   directories: Vec<Spelling>,
   listings:    Vec<Listing>,
}

enum Miss {
   NewKey,
   InputChanged(Spelling),
   Appeared(Spelling),
   NotDirectory(Spelling),
   ListingChanged(Spelling),
}

impl Display for Miss {
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      match *self {
         Self::NewKey => f.write_str("new-key"),
         Self::InputChanged(ref path) => write!(f, "inputs-changed:{path}"),
         Self::Appeared(ref path) => write!(f, "appeared:{path}"),
         Self::NotDirectory(ref path) => write!(f, "not-a-directory:{path}"),
         Self::ListingChanged(ref path) => write!(f, "listing-changed:{path}"),
      }
   }
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
   let request = Request {
      step:        &step.name,
      kind:        &step.kind,
      spec:        &ctx.spec,
      environment: vars().filter(|entry| inherited(&entry.0)).collect(),
      shared:      SHARED,
      module:      fingerprinted,
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
   let bytes = read(path)?;
   if path.starts_with(STORE_DIR) {
      return Ok(InputId::Content(Identity::of(&bytes)));
   }
   Ok(store.content_id("", bytes))
}

/// The listing of every directory under `top` as `names` gives it, taken
/// before a step runs, since entries it deletes or replaces leave no trace
/// to rebuild the listing from afterwards.
fn snapshot(top: &Path) -> io::Result<BTreeMap<PathBuf, String>> {
   let mut listings = BTreeMap::new();
   let mut pending = vec![top.to_path_buf()];
   while let Some(directory) = pending.pop() {
      for listed in read_dir(&directory)? {
         let entry = listed?;
         if entry.file_type()?.is_dir() {
            pending.push(entry.path());
         }
      }
      let digest = names(&directory, &|_entry| true)?;
      listings.insert(directory, digest);
   }
   Ok(listings)
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
   /// What the step read and the build had made before it ran, or why the
   /// accesses cannot stand for that.
   fn build(
      accesses: Accesses,
      mut before: BTreeMap<PathBuf, String>,
      mark: &Mark,
      client: &mut Client,
      store: &mut Store,
   ) -> io::Result<Result<Self, String>> {
      if let Some(why) = accesses.escaped {
         return Ok(Err(why.to_owned()));
      }
      let top = build_top()?;
      let mut own = vec![
         current_exe()?,
         Path::new(&top).join("ctx.json"),
         Path::new(&top).join(".bigtop-modules"),
      ];
      own.extend(
         ["NIX_ATTRS_JSON_FILE", "NIX_ATTRS_SH_FILE"]
            .into_iter()
            .filter_map(|name| var(name).ok().map(PathBuf::from)),
      );
      let ignored = |path: &Path| {
         own.iter().any(|mine| mine == path) || PSEUDO.iter().any(|root| path.starts_with(root))
      };
      let written = |path: &Path| symlink_metadata(path).is_ok_and(|meta| mark.written(&meta));
      let mut files = Vec::new();
      let mut absent = Vec::new();
      let mut directories = Vec::new();
      for path in accesses.paths {
         if ignored(&path) || written(&path) {
            continue;
         }
         match metadata(&path) {
            Ok(meta) if mark.written(&meta) => {},
            Ok(meta) if meta.is_dir() => directories.push(Spelling::of(store, &path)),
            Ok(meta) if meta.is_file() => files.push(path),
            Err(error) if error.kind() == ErrorKind::NotFound => {
               absent.push(Spelling::of(store, &path));
            },
            Ok(_) | Err(_) => {},
         }
      }
      let texts = files
         .iter()
         .filter_map(|path| path.to_str().map(str::to_owned))
         .collect::<Vec<_>>();
      prefetch(client, store, &texts)?;
      let inputs = files
         .iter()
         .map(|path| {
            let spelling = Spelling::of(store, path);
            let identity = match (path.to_str(), spelling.clone()) {
               (Some(text), Spelling::Text(spelled)) if is_store_path(text) && spelled == text => {
                  InputId::Path(spelled)
               },
               (Some(text), _) => store.input_id(text)?,
               (None, _) => raw_identity(store, path)?,
            };
            Ok(Input {
               path: spelling,
               identity,
            })
         })
         .collect::<io::Result<_>>()?;
      let mut listings = Vec::new();
      for path in accesses.listed {
         if ignored(&path) {
            continue;
         }
         let names = if path.starts_with(&top) {
            let Some(found) = before.remove(&path) else {
               continue;
            };
            found
         } else {
            names(&path, &|_entry| true)?
         };
         listings.push(Listing {
            path: Spelling::of(store, &path),
            names,
         });
      }
      Ok(Ok(Self {
         inputs,
         absent,
         directories,
         listings,
      }))
   }

   fn matches(&self, client: &mut Client, store: &mut Store) -> io::Result<Result<(), Miss>> {
      let mut paths = Vec::with_capacity(self.inputs.len());
      for input in &self.inputs {
         let Some(path) = input.path.resolve(store) else {
            return Ok(Err(Miss::InputChanged(input.path.clone())));
         };
         paths.push(path);
      }
      let texts = paths
         .iter()
         .filter_map(|path| path.to_str().map(str::to_owned))
         .collect::<Vec<_>>();
      prefetch(client, store, &texts)?;
      for (input, path) in self.inputs.iter().zip(&paths) {
         if let InputId::Path(ref exact) = input.identity {
            if !Path::new(exact).exists() {
               return Ok(Err(Miss::InputChanged(input.path.clone())));
            }
            continue;
         }
         let identity = path
            .to_str()
            .map_or_else(|| raw_identity(store, path), |text| store.input_id(text));
         match identity {
            Ok(found) if found == input.identity => {},
            Ok(_) => return Ok(Err(Miss::InputChanged(input.path.clone()))),
            Err(error) if error.kind() == ErrorKind::NotFound => {
               return Ok(Err(Miss::InputChanged(input.path.clone())));
            },
            Err(error) => return Err(error),
         }
      }
      for spelling in &self.absent {
         if spelling.resolve(store).is_some_and(|path| path.exists()) {
            return Ok(Err(Miss::Appeared(spelling.clone())));
         }
      }
      for spelling in &self.directories {
         if !spelling.resolve(store).is_some_and(|path| path.is_dir()) {
            return Ok(Err(Miss::NotDirectory(spelling.clone())));
         }
      }
      for listing in &self.listings {
         let Some(path) = listing.path.resolve(store) else {
            return Ok(Err(Miss::ListingChanged(listing.path.clone())));
         };
         if names(&path, &|_entry| true)? != listing.names {
            return Ok(Err(Miss::ListingChanged(listing.path.clone())));
         }
      }
      Ok(Ok(()))
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

fn lookup(client: &mut Client, store: &mut Store, key: &Key) -> io::Result<Result<(), Miss>> {
   let mut first = None;
   for manifest in stored(client, key)? {
      match manifest.matches(client, store)? {
         Ok(()) => return Ok(Ok(())),
         Err(miss) => {
            first.get_or_insert(miss);
         },
      }
   }
   Ok(Err(first.unwrap_or(Miss::NewKey)))
}

fn publish(client: &mut Client, key: &Key, manifest: Manifest) -> io::Result<()> {
   let mut manifests = vec![manifest];
   manifests.extend(stored(client, key)?);
   manifests.truncate(MANIFESTS);
   client.put(key, &serde_json::to_vec(&manifests)?)
}

/// A test step skipped when a stored pass matches, otherwise run traced from
/// `command` and its pass stored. `module` is the build system's, for a step
/// that dispatches into one. Cache trouble is noted, never a failure.
pub fn run(
   mut client: Client,
   ctx: &Ctx,
   step: &Step,
   module: Option<&'static str>,
   command: &dyn Fn() -> io::Result<Command>,
) -> io::Result<Ran> {
   let key = request(ctx, step, module)?;
   let mut store = Store::new();
   match lookup(&mut client, &mut store, &key) {
      Ok(Ok(())) => {
         note("test-cache", "passed before, skipped");
         return Ok(Ran::Skipped);
      },
      Ok(Err(miss)) => note("test-cache", &format!("miss, {miss}")),
      Err(error) => note("test-cache", &format!("lookup failed with {error}")),
   }
   let before = snapshot(Path::new(&build_top()?))?;
   let mark = Mark::now();
   // a signal landing before the trace server takes a call still turns it
   // into EINTR, even for an open of a regular file, so a traced failure
   // may be the tracer's
   let (status, traced) = match trace::run(command)? {
      Outcome::Traced(status, accesses) if status.success() || !accesses.interruptible => {
         (status, Some(accesses))
      },
      Outcome::Traced(status, _) => {
         note(
            "test-cache",
            &format!("traced run failed with {status}, rerunning untraced"),
         );
         (command()?.status()?, None)
      },
      Outcome::Untraced(status, why) => {
         note("test-cache", &format!("untraced, {why}"));
         (status, None)
      },
   };
   if !status.success() {
      return Err(io::Error::other(format!(
         "phase {} failed, {status}",
         step.name
      )));
   }
   let Some(accesses) = traced else {
      return Ok(Ran::Passed);
   };
   match Manifest::build(accesses, before, &mark, &mut client, &mut store) {
      Ok(Ok(manifest)) => {
         let counts = format!(
            "{} inputs, {} absent, {} listings",
            manifest.inputs.len(),
            manifest.absent.len(),
            manifest.listings.len()
         );
         match publish(&mut client, &key, manifest) {
            Ok(()) => note("test-cache", &format!("stored, {counts}")),
            Err(error) => note("test-cache", &format!("store failed with {error}")),
         }
      },
      Ok(Err(why)) => note("test-cache", &format!("not stored, {why}")),
      Err(error) => note("test-cache", &format!("not stored, {error}")),
   }
   Ok(Ran::Passed)
}
