use std::{
   fs::read,
   io::{
      self,
      Write as _,
   },
   path::Path,
   process::Command,
};

use jig::{
   elf::{
      RUNPATH_SLACK,
      has_segment,
      is_elf,
      runpath,
      segments,
   },
   image::Image,
   tree::{
      Entry,
      note,
      parallel,
      walk,
      writable,
   },
};

use crate::exports::lib_dirs;

const INTERP_PADDING: usize = 12;

pub fn run(
   directory: &Path,
   interp: &Path,
   stub: Option<&Path>,
   deps: &[String],
) -> io::Result<()> {
   let dirs = lib_dirs(interp, deps)?;
   let padded = format!(
      "{}/{}{}",
      interp.parent().unwrap_or(interp).display(),
      "./".repeat(INTERP_PADDING),
      interp.file_name().unwrap_or_default().to_string_lossy()
   );
   let mut files = Vec::new();
   for sub in ["bin", "lib", "libexec"] {
      let root = directory.join(sub);
      if root.is_dir() {
         files.extend(walk(&root)?.into_iter().filter(|entry| entry.kind == 'f'));
      }
   }
   let mut done: Vec<String> = parallel(&files, |entry| {
      implant(entry, directory, &dirs, &padded, stub)
   })?
   .into_iter()
   .flatten()
   .collect();
   done.sort();
   for rel in done {
      note("implant", &rel);
   }
   Ok(())
}

fn implant(
   entry: &Entry,
   directory: &Path,
   dirs: &[String],
   interp: &str,
   stub: Option<&Path>,
) -> io::Result<Option<String>> {
   let image = Image {
      bytes: read(&entry.path)?,
   };
   if !is_elf(&image) {
      return Ok(None);
   }
   let segments = segments(&image)?;
   if !has_segment(&segments, 2) {
      return Ok(None);
   }
   let existing = runpath(&image, &segments)?.unwrap_or_default();
   let mut wanted: Vec<&str> = Vec::new();
   for dir in dirs
      .iter()
      .map(String::as_str)
      .chain(existing.split(':'))
      .filter(|dir| keeps(dir))
   {
      if !wanted.contains(&dir) {
         wanted.push(dir);
      }
   }
   let path = format!(
      "{}:/{}",
      wanted.join(":"),
      "_".repeat(wanted.len() * RUNPATH_SLACK - 1)
   );
   writable(Path::new(&entry.path))?;
   let mut args = Vec::<&str>::new();
   let blob = stub.map(|found| found.to_string_lossy().into_owned());
   if has_segment(&segments, 3) {
      if let Some(found) = blob.as_deref() {
         args.extend(["--set-entry-stub", found]);
      }
      args.extend(["--set-interpreter", interp]);
   }
   args.extend(["--set-rpath", &path, &entry.path]);
   drop(writeln!(
      io::stderr().lock(),
      "+ formatelf {}",
      args.join(" ")
   ));
   if !Command::new("formatelf").args(&args).status()?.success() {
      return Err(io::Error::other(format!(
         "formatelf failed on {}",
         entry.rel
      )));
   }
   Ok(Some(
      Path::new(&entry.path)
         .strip_prefix(directory)
         .map_err(io::Error::other)?
         .to_string_lossy()
         .into_owned(),
   ))
}

fn keeps(dir: &str) -> bool {
   let mut chars = dir.chars();
   matches!(chars.next(), Some('/' | '$')) && chars.next().is_some_and(|next| next != '_')
}
