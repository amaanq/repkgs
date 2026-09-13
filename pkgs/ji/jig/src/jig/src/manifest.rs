use std::{
   env::var,
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

pub fn lookup(cache: &mut Client, store: &mut Store, request: &str) -> io::Result<Option<String>> {
   let Some(bytes) = cache.get(&format!("m/{request}"))? else {
      return Ok(None);
   };
   let manifest = serde_json::from_slice::<Manifest>(&bytes)?;
   let Some(paths) = manifest
      .inputs
      .iter()
      .map(|input| store.resolve(&input.path))
      .collect::<Option<Vec<_>>>()
   else {
      return Ok(None);
   };
   prefetch(cache, store, &paths)?;
   for (input, path) in manifest.inputs.iter().zip(paths) {
      match store.input_id(&path) {
         Ok(identity) if identity == input.identity => {},
         Ok(_) => return Ok(None),
         Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
         Err(error) => return Err(error),
      }
   }
   if manifest
      .environment
      .iter()
      .any(|entry| var(&entry.name).ok() != entry.value)
   {
      return Ok(None);
   }
   manifest.result_key(request).map(Some)
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
