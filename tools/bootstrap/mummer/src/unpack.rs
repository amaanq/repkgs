//! An archive into `dest` the way sources are stored, shared by the
//! fixed-output fetcher and spieler's prefetch so their hashes agree.

use std::{
   fs::{
      Permissions,
      create_dir_all,
      read_dir,
      remove_dir_all,
      remove_file,
      rename,
      set_permissions,
      symlink_metadata,
   },
   io,
   os::unix::fs::PermissionsExt as _,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use crate::{
   files::sorted_entries,
   process::Checked as _,
};

/// bsdtar, the one archiver every unpacking here goes through.
pub struct Bsdtar(PathBuf);

impl Bsdtar {
   #[inline]
   pub fn new<Program>(program: Program) -> Self
   where
      Program: Into<PathBuf>,
   {
      Self(program.into())
   }

   /// `archive` extracted into `into`, `options` after.
   #[inline]
   pub fn extract(&self, archive: &Path, into: &Path, options: &[&str]) -> io::Result<()> {
      Command::new(&self.0)
         .arg("-xf")
         .arg(archive)
         .arg("-C")
         .arg(into)
         .args(options)
         .checked()
   }

   /// The member names of `archive`, one per line.
   #[inline]
   pub fn list(&self, archive: &Path) -> io::Result<String> {
      String::from_utf8(
         Command::new(&self.0)
            .arg("-tf")
            .arg(archive)
            .checked_output()?,
      )
      .map_err(io::Error::other)
   }

   /// One member of `archive` as text.
   #[inline]
   pub fn read(&self, archive: &Path, member: &str) -> io::Result<String> {
      String::from_utf8(
         Command::new(&self.0)
            .arg("-xOf")
            .arg(archive)
            .arg(member)
            .checked_output()?,
      )
      .map_err(io::Error::other)
   }
}

/// u+w and no setuid, setgid or sticky bits, recursively.
fn writable_tree(path: &Path) -> io::Result<()> {
   let meta = symlink_metadata(path)?;
   if meta.is_symlink() {
      return Ok(());
   }
   let mode = meta.permissions().mode();
   set_permissions(path, Permissions::from_mode((mode | 0o200) & !0o7000))?;
   if meta.is_dir() {
      for child in read_dir(path)? {
         writable_tree(&child?.path())?;
      }
   }
   Ok(())
}

/// Unpacks `archive` into `dest`.
///
/// If everything sits under one top-level directory (tarballs, github zips)
/// that directory becomes `dest`, otherwise (flat zips such as deno's
/// releases) the entries land in `dest` as they are. A .deb unpacks to its
/// data.tar member's tree.
#[inline]
pub fn unpack(bsdtar: &str, archive: &Path, dest: &Path) -> io::Result<()> {
   let tar = Bsdtar::new(bsdtar);
   let stage = PathBuf::from(format!("{}.unpack", dest.to_string_lossy()));
   create_dir_all(&stage)?;
   let options = ["--no-same-owner", "--no-same-permissions"];
   tar.extract(archive, &stage, &options)?;
   let top = sorted_entries(&stage)?;
   let data: Vec<&PathBuf> = top
      .iter()
      .filter(|path| {
         path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
               name.strip_prefix("data.tar.").is_some_and(|ext| {
                  !ext.is_empty()
                     && ext
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
               })
            })
      })
      .collect();
   if let &[member] = data.as_slice()
      && stage.join("debian-binary").exists()
   {
      tar.extract(member, &stage, &options)?;
      remove_file(member)?;
      remove_file(stage.join("debian-binary"))?;
      for path in sorted_entries(&stage)? {
         if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("control.tar"))
         {
            remove_file(path)?;
         }
      }
   }
   writable_tree(&stage)?;
   let remaining = sorted_entries(&stage)?;
   if let Some(only) = remaining.first()
      && remaining.len() == 1
      && symlink_metadata(only)?.is_dir()
   {
      rename(only, dest)?;
      remove_dir_all(&stage)
   } else {
      rename(&stage, dest)
   }
}
