use std::{
   collections::BTreeMap,
   env::var,
   fs::{
      copy,
      create_dir_all,
      read_dir,
      rename,
   },
   io,
   os::unix::fs::symlink,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
   },
};

use mummer::{
   attrs,
   files::{
      sorted_entries,
      utf8,
      write_file,
   },
   process::Checked as _,
   store_path::StorePath,
   unpack::Bsdtar,
};
use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   bootstrap::stage::{
      dirs,
      files,
   },
   fetch::{
      msi::{
         cabinets,
         install_paths,
      },
      winsdk::PayloadKind,
   },
};

#[derive(Deserialize)]
struct Attrs {
   outputs:  BTreeMap<String, String>,
   peanut:   StorePath,
   sevenzip: StorePath,
   payloads: Vec<Payload>,
}

#[derive(Deserialize)]
struct Payload {
   file: String,
   kind: PayloadKind,
   out:  StorePath,
}

#[derive(Serialize)]
struct VfsFile {
   name:              String,
   #[serde(rename = "type")]
   kind:              &'static str,
   #[serde(rename = "external-contents")]
   external_contents: String,
}

#[derive(Serialize)]
struct VfsDir {
   name:     String,
   #[serde(rename = "type")]
   kind:     &'static str,
   contents: Vec<VfsFile>,
}

#[derive(Serialize)]
struct Overlay {
   version:        u8,
   #[serde(rename = "case-sensitive")]
   case_sensitive: &'static str,
   #[serde(rename = "overlay-relative")]
   relative:       &'static str,
   #[serde(rename = "root-relative")]
   root_relative:  &'static str,
   roots:          Vec<VfsDir>,
}

fn first_entry(directory: &Path) -> io::Result<PathBuf> {
   sorted_entries(directory)?
      .into_iter()
      .next()
      .ok_or_else(|| io::Error::other(format!("{} is empty", directory.display())))
}

fn seven_zip(sevenzip: &StorePath, archive: &Path, into: &Path) -> io::Result<()> {
   Command::new(sevenzip.join("bin/7zz"))
      .arg("x")
      .arg("-y")
      .arg(format!("-o{}", into.display()))
      .arg(archive)
      .stdout(Stdio::null())
      .checked()
}

/// every header dir as a case-insensitive VFS directory listing its own files,
/// paths relative to the overlay file so the sysroot's symlink merge keeps it
/// valid.
fn vfs_overlay(out: &str, header_dirs: &[String]) -> io::Result<String> {
   let mut roots = Vec::new();
   for header_dir in header_dirs {
      for dir in dirs(&format!("{out}/{header_dir}/**"))? {
         let rel = format!("../../{}", dir.strip_prefix(out).unwrap_or(&dir).display());
         let mut names = Vec::new();
         for entry in read_dir(&dir)? {
            let found = entry?;
            if !found.file_type()?.is_dir() {
               names.push(utf8(Path::new(&found.file_name()))?.to_owned());
            }
         }
         if names.is_empty() {
            continue;
         }
         names.sort();
         roots.push(VfsDir {
            contents: names
               .into_iter()
               .map(|name| {
                  VfsFile {
                     external_contents: format!("{rel}/{name}"),
                     name,
                     kind: "file",
                  }
               })
               .collect(),
            name:     rel,
            kind:     "directory",
         });
      }
   }
   Ok(serde_json::to_string_pretty(&Overlay {
      version: 0,
      case_sensitive: "false",
      relative: "true",
      root_relative: "overlay-dir",
      roots,
   })?)
}

fn unpack_msi(attrs: &Attrs, work: &Path, out: &str, payload: &Payload) -> io::Result<()> {
   let tables = work.join(format!("{}.t", payload.file));
   let extracted = work.join(format!("{}.x", payload.file));
   seven_zip(
      &attrs.sevenzip,
      &work.join("in").join(&payload.file),
      &tables,
   )?;
   for cab in cabinets(&tables)? {
      seven_zip(&attrs.sevenzip, &work.join("in").join(cab), &extracted)?;
   }
   for (id, rel) in install_paths(&tables)? {
      let Some(rest) = rel.strip_prefix("Windows Kits/10/") else {
         continue;
      };
      let Some((kind, path)) = rest.split_once('/') else {
         continue;
      };
      if kind != "Include" && kind != "Lib" {
         continue;
      }
      let member = extracted.join(&id);
      if !member.exists() {
         continue;
      }
      let dst = PathBuf::from(format!("{out}/sdk/{}/{path}", kind.to_lowercase()));
      create_dir_all(dst.parent().unwrap_or(&dst))?;
      rename(member, dst)?;
   }
   Ok(())
}

/// etc/cc/, the libc facts bootstrap/stage.rs `cc_facts` documents. clang's
/// msvc driver derives include and lib paths for the arch from the two roots,
/// for lld-link too. The STL is part of the CRT.
fn write_facts(out: &str, version: &str) -> io::Result<()> {
   create_dir_all(format!("{out}/etc/cc"))?;
   let include_dirs = format!(
      "crt/include\nsdk/include/{version}/ucrt\nsdk/include/{version}/um\nsdk/include/{version}/\
       shared\n"
   );
   write_file(
      Path::new(&format!("{out}/etc/cc/include-dirs")),
      include_dirs.as_bytes(),
      None,
   )?;
   let flags = format!(
      "-resource-dir=SYSROOT/lib/clang\n-rtlib=compiler-rt\n-fuse-ld=lld\\
       n-Xmicrosoft-visualc-tools-root\nSYSROOT/crt\n-Xmicrosoft-windows-sdk-root\nSYSROOT/sdk\\
       n-Xmicrosoft-windows-sdk-version\n{version}\n-ivfsoverlay\nSYSROOT/etc/cc/vfs.yaml\n"
   );
   write_file(
      Path::new(&format!("{out}/etc/cc/flags")),
      flags.as_bytes(),
      None,
   )?;
   let overlay = vfs_overlay(out, &[
      "crt/include".to_owned(),
      format!("sdk/include/{version}"),
   ])?;
   write_file(
      Path::new(&format!("{out}/etc/cc/vfs.yaml")),
      overlay.as_bytes(),
      None,
   )?;
   write_file(Path::new(&format!("{out}/etc/cc/cxxflags")), b"\n", None)
}

fn install_crt(attrs: &Attrs, tar: &Bsdtar, work: &Path, out: &str) -> io::Result<()> {
   for payload in attrs
      .payloads
      .iter()
      .filter(|payload| payload.kind == PayloadKind::Crt)
   {
      tar.extract(payload.out.as_ref(), work, &["Contents/VC"])?;
   }
   let vc = first_entry(&work.join("Contents/VC/Tools/MSVC"))?;
   Command::new(attrs.peanut.join("bin/cp"))
      .arg("-r")
      .arg(vc.join("include"))
      .arg(vc.join("lib"))
      .arg(format!("{out}/crt/"))
      .checked()?;
   let redist = PathBuf::from(format!("{out}/crt/redist"));
   create_dir_all(&redist)?;
   let dlls = files(&format!(
      "{}/Contents/VC/Redist/MSVC/*/*/Microsoft.VC*.{{CRT,OpenMP}}/*.dll",
      work.display()
   ))?;
   if dlls.is_empty() {
      return Err(io::Error::other("windows-sdk has no redistributable DLLs"));
   }
   for dll in dlls {
      let name = dll
         .file_name()
         .ok_or_else(|| io::Error::other(format!("{} has no file name", dll.display())))?;
      copy(&dll, redist.join(name))?;
   }
   Ok(())
}

/// The SDK's headers and libraries under `sdk/`, out of its .msi payloads, and
/// the version they install.
fn install_sdk(attrs: &Attrs, work: &Path, out: &str) -> io::Result<String> {
   // 7zz finds an msi's cabinets next to it by the names in its Media table
   for payload in attrs
      .payloads
      .iter()
      .filter(|payload| payload.kind != PayloadKind::Crt)
   {
      symlink(&payload.out, work.join("in").join(&payload.file))?;
   }
   for payload in attrs
      .payloads
      .iter()
      .filter(|payload| payload.kind == PayloadKind::Msi)
   {
      unpack_msi(attrs, work, out, payload)?;
   }
   Command::new(attrs.peanut.join("bin/chmod"))
      .args(["-R", "u+w", out])
      .checked()?;
   // clang's msvc driver appends the installer's spelling
   symlink("include", format!("{out}/sdk/Include"))?;
   symlink("lib", format!("{out}/sdk/Lib"))?;
   let include = first_entry(Path::new(&format!("{out}/sdk/include")))?;
   include
      .file_name()
      .and_then(|name| name.to_str())
      .map(str::to_owned)
      .ok_or_else(|| io::Error::other(format!("{} names no SDK version", include.display())))
}

fn lowercase_links(out: &str) -> io::Result<()> {
   for file in files(&format!("{out}/{{crt,sdk}}/lib/**/*"))? {
      let Some(file_name) = file.file_name() else {
         continue;
      };
      let name = utf8(Path::new(file_name))?.to_owned();
      let lower = name.to_lowercase();
      let Some(dir) = file.parent() else {
         continue;
      };
      if name != lower && !dir.join(&lower).exists() {
         symlink(&name, dir.join(&lower))?;
      }
   }
   Ok(())
}

/// the CRT .vsix are zips with Contents/VC/Tools/MSVC/<v>/{include,lib}, the
/// SDK .msi install below "Windows Kits/10/{Include,Lib}/<v>/" out of
/// cabinets whose members are named by File-table id. Result:
/// crt/{include,lib,redist} and sdk/{include,lib}/<v>/, plus a clang VFS
/// overlay that makes the header dirs case-insensitive (SDK headers include
/// each other in spellings that match no file) and lowercase symlinks for
/// lld-link, which has no VFS.
pub fn run_mode() -> io::Result<()> {
   let attrs = attrs::load::<Attrs>()?;
   let out = attrs
      .outputs
      .get("out")
      .ok_or_else(|| io::Error::other("no out output"))?;
   let work = PathBuf::from(format!(
      "{}/w",
      var("NIX_BUILD_TOP").map_err(|_unset| io::Error::other("NIX_BUILD_TOP is not set"))?
   ));
   create_dir_all(format!("{out}/crt"))?;
   create_dir_all(format!("{out}/sdk"))?;
   create_dir_all(work.join("in"))?;
   install_crt(
      &attrs,
      &Bsdtar::new(attrs.peanut.join("bin/bsdtar")),
      &work,
      out,
   )?;
   let version = install_sdk(&attrs, &work, out)?;
   write_facts(out, &version)?;
   lowercase_links(out)
}
