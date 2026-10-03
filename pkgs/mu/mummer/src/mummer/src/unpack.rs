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

fn run(program: &str, args: &[&str]) -> io::Result<()> {
   let status = Command::new(program).args(args).status()?;
   if status.success() {
      return Ok(());
   }
   Err(io::Error::other(format!(
      "{program} {} failed with {}",
      args.join(" "),
      status.code().unwrap_or(-1)
   )))
}

/// u+w and no setuid, setgid or sticky bits, recursively.
fn writable(path: &Path) -> io::Result<()> {
   let meta = symlink_metadata(path)?;
   if meta.is_symlink() {
      return Ok(());
   }
   let mode = meta.permissions().mode();
   set_permissions(path, Permissions::from_mode((mode | 0o200) & !0o7000))?;
   if meta.is_dir() {
      for child in read_dir(path)? {
         writable(&child?.path())?;
      }
   }
   Ok(())
}

fn entries(directory: &Path) -> io::Result<Vec<PathBuf>> {
   let mut found: Vec<PathBuf> = read_dir(directory)?
      .map(|entry| entry.map(|found| found.path()))
      .collect::<io::Result<_>>()?;
   found.sort();
   Ok(found)
}

/// Unpacks `archive` into `dest`.
///
/// If everything sits under one top-level directory (tarballs, github zips)
/// that directory becomes `dest`, otherwise (flat zips such as deno's
/// releases) the entries land in `dest` as they are. A .deb unpacks to its
/// data.tar member's tree.
#[inline]
pub fn unpack(bsdtar: &str, archive: &Path, dest: &Path) -> io::Result<()> {
   let stage = PathBuf::from(format!("{}.unpack", dest.to_string_lossy()));
   create_dir_all(&stage)?;
   let stage_text = stage.to_string_lossy().into_owned();
   let archive_text = archive.to_string_lossy().into_owned();
   let extract = [
      "-C",
      &stage_text,
      "--no-same-owner",
      "--no-same-permissions",
   ];
   run(bsdtar, &[&["-xf", &archive_text], &extract[..]].concat())?;
   let top = entries(&stage)?;
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
      let member_text = member.to_string_lossy().into_owned();
      run(bsdtar, &[&["-xf", &member_text], &extract[..]].concat())?;
      remove_file(member)?;
      remove_file(stage.join("debian-binary"))?;
      for path in entries(&stage)? {
         if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("control.tar"))
         {
            remove_file(path)?;
         }
      }
   }
   writable(&stage)?;
   let remaining = entries(&stage)?;
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
