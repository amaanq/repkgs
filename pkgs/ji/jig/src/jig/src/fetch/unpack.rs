use std::{
   fs::{
      File,
      Permissions,
      create_dir_all,
      read_dir,
      remove_dir_all,
      remove_file,
      rename,
      set_permissions,
      symlink_metadata,
   },
   io::{
      self,
      copy,
   },
   os::unix::fs::PermissionsExt as _,
   path::{
      Path,
      PathBuf,
   },
   time::Duration,
};

use crate::{
   bootstrap::stage::run,
   fetch::drv::{
      attr,
      say,
   },
};

/// url and its mirrors (nix/mirrors.nix) in order, the first that answers wins.
fn download(urls: &[&str], to: &Path) -> io::Result<()> {
   let agent = ureq::Agent::config_builder()
      .timeout_global(Some(Duration::from_secs(600)))
      .build()
      .new_agent();
   for url in urls {
      let fetched = agent.get(*url).call().and_then(|mut response| {
         let mut body = response.body_mut().with_config().limit(u64::MAX).reader();
         let mut file = File::create(to)?;
         copy(&mut body, &mut file)?;
         Ok(())
      });
      match fetched {
         Ok(()) => return Ok(()),
         Err(error) => say(&format!("{url}: {error}")),
      }
   }
   Err(io::Error::other(format!(
      "no url answered: {}",
      urls.join(" ")
   )))
}

/// u+w,a-st on every entry, symlinks left alone as chmod -R does.
fn writable(path: &Path) -> io::Result<()> {
   let meta = symlink_metadata(path)?;
   if meta.file_type().is_symlink() {
      return Ok(());
   }
   let mode = (meta.permissions().mode() | 0o200) & !0o7000;
   set_permissions(path, Permissions::from_mode(mode))?;
   if meta.is_dir() {
      for entry in read_dir(path)? {
         writable(&entry?.path())?;
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

/// the way sources are stored, a single top-level directory (tarballs,
/// github zips) becomes `dest`, otherwise (flat zips) the entries land in
/// `dest` as they are. Shared with uptrack's unpack.nu, whose hashes must
/// agree.
fn unpack(bsdtar: &str, archive: &Path, dest: &Path) -> io::Result<()> {
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

   // a .deb: the tree is the data.tar member
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

/// nix/sources.nix's fixed-output builder, the first of `$urls` that answers
/// unpacked into `$out` with `$unpacker`'s bsdtar.
pub fn run_mode() -> io::Result<()> {
   let urls = attr("urls")?;
   let out = attr("out")?;
   let unpacker = attr("unpacker")?;
   let archive = Path::new("archive");
   download(&urls.split(' ').collect::<Vec<_>>(), archive)?;
   unpack(&format!("{unpacker}/bin/bsdtar"), archive, Path::new(&out))
}
