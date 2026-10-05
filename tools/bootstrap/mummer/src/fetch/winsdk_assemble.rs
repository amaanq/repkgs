use std::{
   collections::BTreeMap,
   env::var,
   fs::{
      copy,
      create_dir_all,
      read_dir,
      read_to_string,
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

use mummer::files::write_file;
use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   bootstrap::stage::{
      dirs,
      files,
      run,
   },
   fetch::msi::{
      cabinets,
      install_paths,
   },
};

#[derive(Deserialize)]
struct Attrs {
   outputs:  BTreeMap<String, String>,
   peanut:   String,
   sevenzip: String,
   payloads: Vec<Payload>,
}

#[derive(Deserialize)]
struct Payload {
   file: String,
   kind: String,
   out:  String,
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
   let mut names: Vec<PathBuf> = read_dir(directory)?
      .map(|entry| entry.map(|found| found.path()))
      .collect::<io::Result<_>>()?;
   names.sort();
   names
      .into_iter()
      .next()
      .ok_or_else(|| io::Error::other(format!("{} is empty", directory.display())))
}

fn seven_zip(sevenzip: &str, archive: &Path, into: &Path) -> io::Result<()> {
   let status = Command::new(format!("{sevenzip}/bin/7zz"))
      .arg("x")
      .arg("-y")
      .arg(format!("-o{}", into.display()))
      .arg(archive)
      .stdout(Stdio::null())
      .status()?;
   if !status.success() {
      return Err(io::Error::other(format!(
         "7zz failed on {}",
         archive.display()
      )));
   }
   Ok(())
}

/// every header dir as a case-insensitive VFS directory listing its own files,
/// paths relative to the overlay file so the sysroot's symlink merge keeps it
/// valid.
fn vfs_overlay(out: &str, header_dirs: &[String]) -> io::Result<String> {
   let mut roots = Vec::new();
   for header_dir in header_dirs {
      for dir in dirs(&format!("{out}/{header_dir}/**"))? {
         let rel = format!("../../{}", dir.strip_prefix(out).unwrap_or(&dir).display());
         let mut names: Vec<String> = read_dir(&dir)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| !kind.is_dir()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
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

/// the CRT .vsix are zips with Contents/VC/Tools/MSVC/<v>/{include,lib}, the
/// SDK .msi install below "Windows Kits/10/{Include,Lib}/<v>/" out of
/// cabinets whose members are named by File-table id. Result:
/// crt/{include,lib,redist} and sdk/{include,lib}/<v>/, plus a clang VFS
/// overlay that makes the header dirs case-insensitive (SDK headers include
/// each other in spellings that match no file) and lowercase symlinks for
/// lld-link, which has no VFS.
pub fn run_mode() -> io::Result<()> {
   let attrs: Attrs = serde_json::from_str(&read_to_string(
      var("NIX_ATTRS_JSON_FILE")
         .map_err(|_unset| io::Error::other("NIX_ATTRS_JSON_FILE is not set"))?,
   )?)?;
   let out = attrs
      .outputs
      .get("out")
      .cloned()
      .ok_or_else(|| io::Error::other("no out output"))?;
   let bin = format!("{}/bin", attrs.peanut);
   let work = PathBuf::from(format!(
      "{}/w",
      var("NIX_BUILD_TOP").map_err(|_unset| io::Error::other("NIX_BUILD_TOP is not set"))?
   ));
   create_dir_all(format!("{out}/crt"))?;
   create_dir_all(format!("{out}/sdk"))?;
   create_dir_all(work.join("in"))?;

   let work_text = work.to_string_lossy().into_owned();
   for payload in attrs
      .payloads
      .iter()
      .filter(|payload| payload.kind == "crt")
   {
      run(&format!("{bin}/bsdtar"), &[
         "-xf",
         &payload.out,
         "-C",
         &work_text,
         "Contents/VC",
      ])?;
   }
   let vc = first_entry(&work.join("Contents/VC/Tools/MSVC"))?;
   let vc_text = vc.to_string_lossy().into_owned();
   run(&format!("{bin}/cp"), &[
      "-r",
      &format!("{vc_text}/include"),
      &format!("{vc_text}/lib"),
      &format!("{out}/crt/"),
   ])?;

   let redist = PathBuf::from(format!("{out}/crt/redist"));
   create_dir_all(&redist)?;

   let dlls = files(&format!(
      "{work_text}/Contents/VC/Redist/MSVC/*/*/Microsoft.VC*.{{CRT,OpenMP}}/*.dll"
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

   // 7zz finds an msi's cabinets next to it by the names in its Media table
   for payload in attrs
      .payloads
      .iter()
      .filter(|payload| payload.kind != "crt")
   {
      symlink(&payload.out, work.join("in").join(&payload.file))?;
   }
   for payload in attrs
      .payloads
      .iter()
      .filter(|payload| payload.kind == "msi")
   {
      unpack_msi(&attrs, &work, &out, payload)?;
   }
   run(&format!("{bin}/chmod"), &["-R", "u+w", &out])?;
   // clang's msvc driver appends the installer's spelling
   symlink("include", format!("{out}/sdk/Include"))?;
   symlink("lib", format!("{out}/sdk/Lib"))?;

   let version = first_entry(Path::new(&format!("{out}/sdk/include")))?
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_default();
   write_facts(&out, &version)?;
   for file in files(&format!("{out}/{{crt,sdk}}/lib/**/*"))? {
      let Some(name) = file
         .file_name()
         .map(|name| name.to_string_lossy().into_owned())
      else {
         continue;
      };
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
