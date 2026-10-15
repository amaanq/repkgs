use std::{
   collections::{
      BTreeMap,
      HashSet,
   },
   env::var,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   io,
   mem::take,
   path::{
      Path,
      PathBuf,
   },
   sync::LazyLock,
};

use blake3::Hasher;
use mummer::{
   attrs,
   files::utf8,
   store::{
      InputId,
      STORE_DIR,
      Store,
      field,
      is_store_path,
      mask_hashes,
   },
};
use serde::{
   Deserialize,
   Serialize,
};
use tightrope::{
   client::Client,
   digest::Digest,
   key::{
      Key,
      Namespace,
   },
};

const MANIFESTS: usize = 8;
/// How many a list holds at most, when the cache keeps manifests past
/// `MANIFESTS` because they were read recently.
const LISTED: usize = MANIFESTS * 4;

#[derive(Deserialize, Serialize, PartialEq, Eq)]
struct Input {
   path:     String,
   identity: InputId,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
struct Environment {
   name:  String,
   value: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Manifest {
   inputs:      Vec<Input>,
   environment: Vec<Environment>,
   absent:      Vec<String>,
   /// The output of the build that stored it, its hash masked, which result
   /// keys leave out so builds of other packages still share results.
   #[serde(default, skip_serializing_if = "Option::is_none")]
   owner:       Option<String>,
   /// The same output unmasked.
   #[serde(default, skip_serializing_if = "Option::is_none")]
   build:       Option<String>,
}

#[derive(Deserialize)]
struct Outputs {
   outputs: BTreeMap<String, String>,
}

static OUTPUT: LazyLock<Option<String>> =
   LazyLock::new(|| attrs::load::<Outputs>().ok()?.outputs.remove("out"));

/// What a result key covers, the manifest without its owner.
#[derive(Serialize)]
struct Keyed<'manifest> {
   inputs:      &'manifest [Input],
   environment: &'manifest [Environment],
   absent:      &'manifest [String],
}

impl Manifest {
   pub fn build(
      cache: &mut Client,
      store: &mut Store,
      paths: &[String],
      variables: &[String],
      absent_paths: &[String],
   ) -> io::Result<Self> {
      prefetch(cache, store, paths)?;
      let inputs = paths
         .iter()
         .map(|path| {
            Ok(Input {
               path:     store.key(path),
               identity: store
                  .input_id(path)
                  .map_err(|error| io::Error::new(error.kind(), format!("{path}: {error}")))?,
            })
         })
         .collect::<io::Result<_>>()?;
      let environment = variables
         .iter()
         .map(|name| {
            Environment {
               name:  name.to_owned(),
               value: var(name).ok(),
            }
         })
         .collect();
      let mut seen = HashSet::new();
      let absent = absent_paths
         .iter()
         .filter(|path| !path.is_empty() && !is_store_path(path) && !Path::new(path).exists())
         .map(|path| store.key(path))
         .filter(|path| seen.insert(path.clone()))
         .collect();

      Ok(Self {
         inputs,
         environment,
         absent,
         owner: OUTPUT.as_deref().map(mask_hashes),
         build: OUTPUT.clone(),
      })
   }

   /// Library files a link would have taken, kept in store directories too,
   /// since a dependency rebuilt with one moves to a path this build's roots
   /// resolve the masked one to.
   #[must_use]
   pub fn with_absent_libraries(mut self, store: &Store, paths: &[String]) -> Self {
      for path in paths.iter().filter(|path| !Path::new(path).exists()) {
         let masked = store.key(path);
         if !self.absent.contains(&masked) {
            self.absent.push(masked);
         }
      }
      self
   }

   pub fn result_key(&self, request: &Key) -> io::Result<Digest> {
      let mut hasher = Hasher::new();
      field(&mut hasher, request.as_str());
      field(
         &mut hasher,
         serde_json::to_vec(&Keyed {
            inputs:      &self.inputs,
            environment: &self.environment,
            absent:      &self.absent,
         })?,
      );
      Ok(Digest::from(hasher.finalize()))
   }

   /// Whether this is a later build of `older`'s package with only its sources changed.
   fn supersedes(&self, older: &Self) -> bool {
      self.owner.is_some()
         && self.owner == older.owner
         && self.build != older.build
         && self.environment == older.environment
         && self.store_inputs() == older.store_inputs()
   }

   fn store_inputs(&self) -> Vec<(&str, &InputId)> {
      let mut found = self
         .inputs
         .iter()
         .filter(|input| input.path.starts_with(STORE_DIR))
         .map(|input| (input.path.as_str(), &input.identity))
         .collect::<Vec<_>>();
      found.sort_unstable_by_key(|entry| entry.0);
      found
   }

   /// llvm and llvm22 precompiles share request keys, so several sets stay.
   /// The sets this one supersedes are dropped from the cache with their
   /// results, and past `MANIFESTS` the least recently read go unless the
   /// cache saw them read lately.
   pub fn publish(self, cache: &mut Client, request: &Key) -> io::Result<()> {
      let key = Key::of(Namespace::Manifest, request);
      let result = self.result_key(request)?;
      let known = cache
         .get(&key)?
         .and_then(|bytes| serde_json::from_slice::<Vec<Self>>(&bytes).ok())
         .unwrap_or_default();
      let mut kept = Vec::new();
      let mut superseded = Vec::new();
      for manifest in known {
         let digest = manifest.result_key(request)?;
         if digest == result {
            continue;
         }
         let record = Key::digest(Namespace::Record, &digest);
         if self.supersedes(&manifest) {
            superseded.push((manifest, record));
         } else {
            kept.push((manifest, record));
         }
      }
      let replaced = superseded
         .iter()
         .map(|entry| entry.1.clone())
         .collect::<Vec<_>>();
      for (entry, dropped) in superseded.into_iter().zip(cache.forget(&replaced)?) {
         if !dropped {
            kept.push(entry);
         }
      }
      if kept.len() >= MANIFESTS {
         let listed = kept.iter().map(|entry| entry.1.clone()).collect::<Vec<_>>();
         let ages = cache.ages(&listed)?;
         let mut ranked = kept
            .into_iter()
            .zip(ages)
            .filter_map(|(entry, age)| Some((age?, entry)))
            .collect::<Vec<_>>();
         ranked.sort_by_key(|pair| pair.0);
         let overflow = ranked.split_off(ranked.len().min(MANIFESTS - 1));
         kept = ranked.into_iter().map(|pair| pair.1).collect();
         let unlisted = overflow
            .iter()
            .map(|pair| pair.1.1.clone())
            .collect::<Vec<_>>();
         for ((_, entry), dropped) in overflow.into_iter().zip(cache.expire(&unlisted)?) {
            if !dropped {
               kept.push(entry);
            }
         }
      }
      let mut manifests = vec![self];
      manifests.extend(kept.into_iter().map(|entry| entry.0));
      manifests.truncate(LISTED);
      cache.put(&key, &serde_json::to_vec(&manifests)?)
   }

   fn matches(&self, cache: &mut Client, store: &mut Store) -> Result<(), Miss> {
      let mut paths = Vec::with_capacity(self.inputs.len());
      for input in &self.inputs {
         let Some(path) = store.resolve(&input.path) else {
            return Err(Miss::InputsChanged(input.path.clone()));
         };
         paths.push(path);
      }
      prefetch(cache, store, &paths)?;
      for (input, path) in self.inputs.iter().zip(&paths) {
         match store.input_id(path) {
            Ok(identity) if identity == input.identity => {},
            Ok(_) => return Err(Miss::InputsChanged(path.clone())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
               let elsewhere = Store::scratch_candidates(path).iter().any(|candidate| {
                  store
                     .input_id(candidate)
                     .is_ok_and(|identity| identity == input.identity)
               });
               if !elsewhere {
                  return Err(Miss::InputsChanged(path.clone()));
               }
            },
            Err(error) => return Err(Miss::Failed(error)),
         }
      }
      let absent = self
         .absent
         .iter()
         .filter_map(|absent| Some((absent, store.resolve(absent)?)))
         .collect::<Vec<_>>();
      for &(masked, ref path) in &absent {
         let found = Path::new(path);
         if !found.exists() {
            continue;
         }
         // a search dir that appeared changes nothing unless it holds a name
         // an include was spelled as, the tail of an input or of a failed
         // lookup elsewhere
         let looked_up = paths.iter().chain(absent.iter().map(|pair| &pair.1));
         let shadows = !found.is_dir()
            || looked_up.into_iter().any(|seen| {
               let parts = Path::new(seen).components().collect::<Vec<_>>();
               (1..parts.len()).any(|skip| {
                  found
                     .join(parts[skip..].iter().collect::<PathBuf>())
                     .exists()
               })
            });
         if shadows {
            return Err(Miss::Appeared(masked.clone()));
         }
      }

      if let Some(entry) = self
         .environment
         .iter()
         .find(|entry| var(&entry.name).ok() != entry.value)
      {
         return Err(Miss::EnvironmentChanged(entry.name.clone()));
      }
      Ok(())
   }
}

pub enum Miss {
   NewKey,
   /// A manifest in a shape this mummer does not read, replaced on store.
   Unreadable,
   InputsChanged(String),
   EnvironmentChanged(String),
   Appeared(String),
   ObjectGone,
   RunpathRoom,
   Failed(io::Error),
}

impl Display for Miss {
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      match *self {
         Self::NewKey => f.write_str("new-key"),
         Self::Unreadable => f.write_str("manifest-unreadable"),
         Self::InputsChanged(ref path) => write!(f, "inputs-changed:{path}"),
         Self::EnvironmentChanged(ref name) => write!(f, "env-changed:{name}"),
         Self::Appeared(ref path) => write!(f, "appeared:{path}"),
         Self::ObjectGone => f.write_str("object-gone"),
         Self::RunpathRoom => f.write_str("runpath-room"),
         Self::Failed(_) => f.write_str("replay-failed"),
      }
   }
}

impl From<io::Error> for Miss {
   fn from(error: io::Error) -> Self {
      Self::Failed(error)
   }
}

pub fn lookup(cache: &mut Client, store: &mut Store, request: &Key) -> Result<Digest, Miss> {
   let Some(bytes) = cache.get(&Key::of(Namespace::Manifest, request))? else {
      return Err(Miss::NewKey);
   };
   let Ok(manifests) = serde_json::from_slice::<Vec<Manifest>>(&bytes) else {
      return Err(Miss::Unreadable);
   };
   let mut newest = None;
   for manifest in &manifests {
      match manifest.matches(cache, store) {
         Ok(()) => return Ok(manifest.result_key(request)?),
         Err(Miss::Failed(error)) => return Err(Miss::Failed(error)),
         Err(miss) => {
            newest.get_or_insert(miss);
         },
      }
   }
   Err(newest.unwrap_or(Miss::NewKey))
}

pub fn prefetch(cache: &mut Client, store: &mut Store, paths: &[String]) -> io::Result<()> {
   let requested = paths
      .iter()
      .filter(|path| store.daemon_may_identify(path))
      .cloned()
      .collect::<Vec<String>>();
   for (path, identity) in requested.iter().zip(cache.identities(&requested)?) {
      if let Some(value) = identity {
         store.remember(path.to_owned(), InputId::Content(value));
      }
   }

   // a file changed in the last two seconds has a memo only when
   // `remember_written` left one, and gets none from here
   let mut stamped = Vec::new();
   let mut keys = Vec::new();
   for path in paths {
      let settled = store.stamp_key(path);
      if let Some(stamp) = settled.or_else(|| store.written_stamp_key(path)) {
         stamped.push((path, settled.is_some()));
         keys.push(Key::digest(Namespace::Stamp, &stamp));
      }
   }
   let found = cache.get_many(&keys)?;
   for (((path, settled), key), cached) in stamped.into_iter().zip(&keys).zip(found) {
      if let Some(identity) = cached
         .and_then(|bytes| String::from_utf8(bytes).ok())
         .and_then(|text| text.parse().ok())
      {
         store.remember(path.to_owned(), identity);
         continue;
      }
      if !settled {
         continue;
      }
      let Ok(identity) = store.input_id(path) else {
         continue;
      };
      cache.put(key, identity.to_string().as_bytes())?;
      store.remember(path.to_owned(), identity);
   }
   Ok(())
}

/// The identity of a file this process just wrote, which `stamp_key` refuses
/// for two seconds, so every compile reading it then would hash it again.
pub fn remember_written(
   cache: &mut Client,
   store: &Store,
   path: &Path,
   bytes: Vec<u8>,
) -> io::Result<()> {
   let text = utf8(path)?;
   let Some(stamp) = store.written_stamp_key(text) else {
      return Ok(());
   };
   let identity = store.content_id(text, bytes);
   cache.put(
      &Key::digest(Namespace::Stamp, &stamp),
      identity.to_string().as_bytes(),
   )
}

pub fn parse_depfile(text: &str) -> Vec<String> {
   let mut paths = Vec::new();
   let mut token = String::new();
   let mut dependencies = false;
   let mut characters = text.chars().peekable();
   while let Some(character) = characters.next() {
      match character {
         '\\' if characters.peek() == Some(&'\n') => {
            characters.next();
         },
         '\\'
            if characters
               .peek()
               .is_some_and(|next| matches!(next, ' ' | '\t' | '\\' | '#' | ':')) =>
         {
            let Some(escaped) = characters.next() else {
               return paths;
            };
            token.push(escaped);
         },
         ':' if !dependencies => {
            dependencies = true;
            token.clear();
         },
         ' ' | '\t' | '\n' | '\r' => {
            if dependencies && !token.is_empty() {
               paths.push(take(&mut token));
            } else {
               token.clear();
            }
            if character == '\n' && dependencies {
               break;
            }
         },
         _ => token.push(character),
      }
   }
   if dependencies && !token.is_empty() {
      paths.push(token);
   }
   paths
}
