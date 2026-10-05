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
   path::Path,
};

use blake3::Hasher;
use mummer::store::{
   Store,
   field,
   is_store_path,
};
use serde::{
   Deserialize,
   Serialize,
};
use tightrope::client::Client;

#[derive(Deserialize, Serialize)]
struct Input {
   path:     String,
   identity: String,
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

   pub fn result_key(&self, request: &str) -> io::Result<String> {
      let mut hasher = Hasher::new();
      field(&mut hasher, request);
      field(&mut hasher, serde_json::to_vec(self)?);
      Ok(hasher.finalize().to_hex()[..32].to_owned())
   }

   pub fn publish(&self, cache: &mut Client, request: &str) -> io::Result<()> {
      cache.put(&format!("m/{request}"), &serde_json::to_vec(self)?)
   }
}

pub enum Miss {
   NewKey,
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

pub fn lookup(cache: &mut Client, store: &mut Store, request: &str) -> Result<String, Miss> {
   let Some(bytes) = cache.get(&format!("m/{request}"))? else {
      return Err(Miss::NewKey);
   };
   let manifest = serde_json::from_slice::<Manifest>(&bytes).map_err(io::Error::from)?;
   let mut paths = Vec::with_capacity(manifest.inputs.len());
   for input in &manifest.inputs {
      let Some(path) = store.resolve(&input.path) else {
         return Err(Miss::InputsChanged(input.path.clone()));
      };
      paths.push(path);
   }
   prefetch(cache, store, &paths)?;
   for (input, path) in manifest.inputs.iter().zip(paths) {
      match store.input_id(&path) {
         Ok(identity) if identity == input.identity => {},
         Ok(_) => return Err(Miss::InputsChanged(path)),
         Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(Miss::InputsChanged(path));
         },
         Err(error) => return Err(Miss::Failed(error)),
      }
   }
   for absent in &manifest.absent {
      if store
         .resolve(absent)
         .is_some_and(|path| Path::new(&path).exists())
      {
         return Err(Miss::Appeared(absent.clone()));
      }
   }

   if let Some(entry) = manifest
      .environment
      .iter()
      .find(|entry| var(&entry.name).ok() != entry.value)
   {
      return Err(Miss::EnvironmentChanged(entry.name.clone()));
   }
   Ok(manifest.result_key(request)?)
}

pub fn prefetch(cache: &mut Client, store: &mut Store, paths: &[String]) -> io::Result<()> {
   let requested = paths
      .iter()
      .filter(|path| store.daemon_may_identify(path))
      .cloned()
      .collect::<Vec<String>>();
   for (path, identity) in requested.iter().zip(cache.identities(&requested)?) {
      if let Some(value) = identity {
         store.remember(path.to_owned(), value.to_string());
      }
   }

   let mut stamped = Vec::new();
   let mut keys = Vec::new();
   for path in paths {
      if let Some(key) = store.stamp_key(path) {
         stamped.push(path);
         keys.push(key);
      }
   }
   let found = cache.get_many(&keys)?;
   for ((path, key), cached) in stamped.into_iter().zip(&keys).zip(found) {
      if let Some(identity) = cached.and_then(|bytes| String::from_utf8(bytes).ok()) {
         store.remember(path.to_owned(), identity);
         continue;
      }
      let Ok(identity) = store.input_id(path) else {
         continue;
      };
      cache.put(key, identity.as_bytes())?;
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
