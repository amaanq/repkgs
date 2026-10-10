use std::{
   collections::HashSet,
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
};

use blake3::Hasher;
use mummer::store::{
   InputId,
   Store,
   field,
   is_store_path,
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

#[derive(Deserialize, Serialize)]
struct Input {
   path:     String,
   identity: InputId,
}

#[derive(Deserialize, Serialize)]
struct Environment {
   name:  String,
   value: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Manifest {
   inputs:      Vec<Input>,
   environment: Vec<Environment>,
   absent:      Vec<String>,
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
      field(&mut hasher, serde_json::to_vec(self)?);
      Ok(Digest::from(hasher.finalize()))
   }

   /// llvm and llvm22 precompiles share request keys, so several sets stay.
   pub fn publish(self, cache: &mut Client, request: &Key) -> io::Result<()> {
      let key = Key::of(Namespace::Manifest, request);
      let result = self.result_key(request)?;
      let known = cache
         .get(&key)?
         .and_then(|bytes| serde_json::from_slice::<Vec<Self>>(&bytes).ok())
         .unwrap_or_default();
      let mut manifests = vec![self];
      for manifest in known {
         if manifest.result_key(request)? != result {
            manifests.push(manifest);
         }
      }
      manifests.truncate(MANIFESTS);
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
               return Err(Miss::InputsChanged(path.clone()));
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

   let mut stamped = Vec::new();
   let mut keys = Vec::new();
   for path in paths {
      if let Some(stamp) = store.stamp_key(path) {
         stamped.push(path);
         keys.push(Key::digest(Namespace::Stamp, &stamp));
      }
   }
   let found = cache.get_many(&keys)?;
   for ((path, key), cached) in stamped.into_iter().zip(&keys).zip(found) {
      if let Some(identity) = cached
         .and_then(|bytes| String::from_utf8(bytes).ok())
         .and_then(|text| text.parse().ok())
      {
         store.remember(path.to_owned(), identity);
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
