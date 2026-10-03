use std::{
   collections::BTreeMap,
   env::var,
   fs::{
      create_dir_all,
      read,
      read_to_string,
   },
   io,
   os::unix::fs::symlink,
   path::Path,
};

use serde::Deserialize;
use shim::{
   files::write_file,
   tree::parallel_with,
};

use crate::{
   bootstrap::stage::{
      cores,
      run,
   },
   fetch::drv::{
      Entry,
      Kind,
   },
};

#[derive(Deserialize)]
pub struct Attrs {
   pub outputs: BTreeMap<String, String>,
   pub seed:    String,
   pub layout:  Vec<Entry>,
}

impl Attrs {
   pub fn load() -> io::Result<Self> {
      let path = var("NIX_ATTRS_JSON_FILE")
         .map_err(|_unset| io::Error::other("NIX_ATTRS_JSON_FILE is not set"))?;
      Ok(serde_json::from_str(&read_to_string(path)?)?)
   }

   pub fn out(&self) -> io::Result<&str> {
      self
         .outputs
         .get("out")
         .map(String::as_str)
         .ok_or_else(|| io::Error::other("no out output"))
   }
}

fn place(entry: &Entry, out: &str, bin: &str) -> io::Result<()> {
   let target = format!("{out}/{}", entry.to);
   let dst = Path::new(&target);
   match entry.kind {
      Kind::Link { ref link } => {
         create_dir_all(dst.parent().unwrap_or(dst))?;
         symlink(link, dst)
      },
      Kind::Unpack { ref unpack } => {
         create_dir_all(dst)?;
         run(&format!("{bin}/bsdtar"), &[
            "-xf",
            unpack,
            "-C",
            &target,
            "--strip-components",
            "1",
            "--no-same-owner",
            "--no-same-permissions",
         ])
      },
      Kind::Write { ref write } => {
         create_dir_all(dst.parent().unwrap_or(dst))?;
         write_file(dst, write.as_bytes(), None)
      },
      Kind::Copy {
         ref copy,
         ref append,
      } => {
         create_dir_all(dst.parent().unwrap_or(dst))?;
         let mut bytes = read(copy)?;
         bytes.extend_from_slice(append.as_bytes());
         write_file(dst, &bytes, None)
      },
   }
}

/// lay the collecting derivation's `layout` out under $out, then drop the
/// write bits the daemon rejects.
pub fn run_mode() -> io::Result<()> {
   let attrs = Attrs::load()?;
   let out = attrs.out()?;
   let bin = format!("{}/bin", attrs.seed);
   parallel_with(&attrs.layout, cores(), |entry| place(entry, out, &bin))?;
   run(&format!("{bin}/chmod"), &["-R", "u+w,go-w,a-st", out])
}
