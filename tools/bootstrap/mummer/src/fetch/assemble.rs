use std::{
   collections::BTreeMap,
   fs::{
      create_dir_all,
      read,
   },
   io,
   os::unix::fs::symlink,
   path::Path,
   process::Command,
};

use mummer::{
   attrs,
   files::write_file,
   process::Checked as _,
   store_path::StorePath,
   tree::parallel_with,
   unpack::Bsdtar,
};
use serde::Deserialize;

use crate::{
   bootstrap::stage::cores,
   fetch::drv::{
      Entry,
      Kind,
   },
};

#[derive(Deserialize)]
pub struct Attrs {
   pub outputs: BTreeMap<String, String>,
   pub peanut:  StorePath,
   pub layout:  Vec<Entry>,
}

impl Attrs {
   pub fn out(&self) -> io::Result<&str> {
      self
         .outputs
         .get("out")
         .map(String::as_str)
         .ok_or_else(|| io::Error::other("no out output"))
   }
}

fn place(entry: &Entry, out: &str, tar: &Bsdtar) -> io::Result<()> {
   let target = format!("{out}/{}", entry.to);
   let dst = Path::new(&target);
   match entry.kind {
      Kind::Link { ref link } => {
         create_dir_all(dst.parent().unwrap_or(dst))?;
         symlink(link, dst)
      },
      Kind::Unpack { ref unpack } => {
         create_dir_all(dst)?;
         tar.extract(Path::new(unpack), dst, &[
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
   let attrs = attrs::load::<Attrs>()?;
   let out = attrs.out()?;
   let tar = Bsdtar::new(attrs.peanut.join("bin/bsdtar"));
   parallel_with(&attrs.layout, cores(), |entry| place(entry, out, &tar))?;
   Command::new(attrs.peanut.join("bin/chmod"))
      .args(["-R", "u+w,go-w,a-st", out])
      .checked()
}
