use std::{
   env::var,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   io,
   mem::take,
};

use blake3::Hasher;
use serde::{
   Deserialize,
   Serialize,
};
use wire::client::Client;

use crate::store::{
   Store,
   field,
};

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
}

impl Manifest {
   pub fn build(
      cache: &mut Client,
      store: &mut Store,
      paths: &[String],
      variables: &[String],
   ) -> io::Result<Self> {
      prefetch(cache, store, paths)?;
      let inputs = paths
         .iter()
         .map(|path| {
            Ok(Input {
               path:     store.key(path),
               identity: store.input_id(path)?,
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
      Ok(Self {
         inputs,
         environment,
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
   ObjectGone,
   Failed(io::Error),
}

impl Display for Miss {
   #[expect(
      clippy::renamed_function_params,
      reason = "Project names have at least two characters"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> FormatResult {
      match *self {
         Self::NewKey => formatter.write_str("new-key"),
         Self::InputsChanged(ref path) => write!(formatter, "inputs-changed:{path}"),
         Self::EnvironmentChanged(ref name) => write!(formatter, "env-changed:{name}"),
         Self::ObjectGone => formatter.write_str("object-gone"),
         Self::Failed(_) => formatter.write_str("replay-failed"),
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
