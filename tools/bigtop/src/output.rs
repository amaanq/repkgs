//! Whole package outputs kept in mummerd, keyed on everything that decides
//! them except the mummer and plumbing binaries, so a change to how builds
//! are cached does not rebuild every package.

use std::{
   env::{
      current_exe,
      var,
   },
   fs::{
      read,
      read_to_string,
      remove_dir_all,
   },
   io,
   path::Path,
};

use memchr::memmem;
use mummer::{
   directory::{
      get_tree,
      put_tree,
   },
   process::socket,
   store::STORE_DIR,
   tree::note,
};
use serde::Serialize;
use tightrope::{
   client::Client,
   key::Key,
};

use crate::{
   attrs::{
      Attrs,
      build_top,
   },
   probe::toolchain,
};

#[derive(Serialize)]
struct Record<'inputs> {
   attrs:      &'inputs str,
   toolchains: Vec<String>,
   bigtop:     String,
}

/// An output's path and the stand-in for its hash while stored, the same
/// length so binaries keep their layout, with an `e`, which no store hash has.
struct Output {
   name:  &'static str,
   path:  String,
   token: String,
}

fn outputs(attrs: &Attrs) -> Vec<Output> {
   [
      ("out", Some(attrs.outputs.out.clone())),
      ("debug", attrs.outputs.debug.clone()),
      ("tree", attrs.outputs.tree.clone()),
   ]
   .into_iter()
   .enumerate()
   .filter_map(|(index, (name, path))| {
      Some(Output {
         name,
         path: path?,
         token: format!("{}{index}", "e".repeat(31)),
      })
   })
   .collect()
}

fn hash_part(path: &str) -> &str {
   path
      .strip_prefix(STORE_DIR)
      .and_then(|rest| rest.strip_prefix('/'))
      .and_then(|rest| rest.get(..32))
      .unwrap_or_default()
}

fn swap(mut data: Vec<u8>, pairs: &[(String, String)]) -> Vec<u8> {
   for pair in pairs.iter().filter(|pair| pair.0.len() == pair.1.len()) {
      let found = memmem::find_iter(&data, pair.0.as_bytes()).collect::<Vec<usize>>();
      for position in found {
         data[position..position + pair.1.len()].copy_from_slice(pair.1.as_bytes());
      }
   }
   data
}

fn swap_text(text: String, pairs: &[(String, String)]) -> String {
   pairs
      .iter()
      .fold(text, |current, pair| current.replace(&pair.0, &pair.1))
}

fn key(attrs: &Attrs, outputs: &[Output]) -> io::Result<String> {
   let file = var("NIX_ATTRS_JSON_FILE")
      .map_err(|_unset| io::Error::other("NIX_ATTRS_JSON_FILE is not set"))?;
   let mut text = read_to_string(file)?;
   for output in outputs {
      text = text.replace(&output.path, &format!("<{}>", output.name));
   }
   for helper in &attrs.plumbing {
      text = text.replace(helper.as_str(), "<plumbing>");
   }
   let top = build_top()?;
   let mut toolchains = Vec::new();
   for root in &attrs.build_dependencies {
      if Path::new(root).join("etc/mummer.json").exists() {
         toolchains.push(toolchain(root, &top)?);
         text = text.replace(root.as_str(), "<cc>");
      }
   }
   let record = Record {
      attrs: &text,
      toolchains,
      bigtop: blake3::hash(&read(current_exe()?)?).to_hex().to_string(),
   };
   Ok(blake3::hash(&serde_json::to_vec(&record)?)
      .to_hex()
      .to_string())
}

fn entry(attrs: &Attrs, digest: &str, output: &Output) -> io::Result<Key> {
   format!("output/{}/{digest}/{}", attrs.spec()?.name, output.name).parse()
}

/// The outputs a build with these inputs stored, false when any is missing,
/// with nothing left behind.
pub fn restore(attrs: &Attrs) -> io::Result<bool> {
   let Ok(mut client) = Client::connect(&socket()) else {
      return Ok(false);
   };
   let outputs = outputs(attrs);
   let digest = key(attrs, &outputs)?;
   let pairs = outputs
      .iter()
      .map(|output| (output.token.clone(), hash_part(&output.path).to_owned()))
      .collect::<Vec<_>>();
   for (index, output) in outputs.iter().enumerate() {
      let restored = get_tree(
         &mut client,
         &entry(attrs, &digest, output)?,
         Path::new(&output.path),
         |data| swap(data, &pairs),
         |target| swap_text(target, &pairs),
      );
      if !matches!(restored, Ok(true)) {
         for earlier in &outputs[..index] {
            remove_dir_all(&earlier.path)?;
         }
         if let Err(error) = restored {
            note("output-cache", &format!("restore failed with {error}"));
         }
         return Ok(false);
      }
   }
   note("output-cache", "restored");
   Ok(true)
}

pub fn store(attrs: &Attrs) -> io::Result<()> {
   let Ok(mut client) = Client::connect(&socket()) else {
      return Ok(());
   };
   let outputs = outputs(attrs);
   let digest = key(attrs, &outputs)?;
   let pairs = outputs
      .iter()
      .map(|output| (hash_part(&output.path).to_owned(), output.token.clone()))
      .collect::<Vec<_>>();
   for output in &outputs {
      if !Path::new(&output.path).exists() {
         continue;
      }
      put_tree(
         &mut client,
         &entry(attrs, &digest, output)?,
         Path::new(&output.path),
         |data| swap(data, &pairs),
         |target| swap_text(target, &pairs),
      )?;
   }
   Ok(())
}
