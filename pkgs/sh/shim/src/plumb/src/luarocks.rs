//! `locks/luarocks.toml`: the set's rock versions, `<rock> = { version,
//! sha256 }`, `sha256` of luarocks.org's `<rock>-<version>.src.rock` (the
//! manifest publishes none).
//!
//! Locking a package lets the tree's luarocks build its rockspec's
//! dependencies from luarocks.org into a scratch tree and records what it
//! installed. A rock another package locked at a different version moves
//! (printed).

use std::{
   collections::BTreeMap,
   env::{
      temp_dir,
      var_os,
   },
   ffi::{
      OsStr,
      OsString,
   },
   fs::remove_dir_all,
   io,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      id,
   },
};

use crate::{
   locks::Entries,
   nix,
   pipeline::root,
};

const LUAROCKS: &str = "https://luarocks.org";

struct Pick {
   name:    String,
   version: String,
}

/// What luarocks installs for the rockspec under `src`, with the tree's cc
/// for C modules.
fn solve(src: &Path) -> io::Result<Vec<Pick>> {
   let built = nix::succeed("nix-build", [
      root().join("nix/set.nix").as_os_str(),
      OsStr::new("-A"),
      OsStr::new("pkgs.luarocks"),
      OsStr::new("-A"),
      OsStr::new("toolchain"),
      OsStr::new("--no-out-link"),
   ])?;
   let bins: Vec<PathBuf> = built
      .lines()
      .map(|line| PathBuf::from(line.trim()).join("bin"))
      .collect();
   let luarocks = bins
      .first()
      .ok_or_else(|| io::Error::other("luarocks did not build"))?
      .join("luarocks");
   let work = temp_dir().join(format!("plumb-luarocks.{}", id()));
   drop(remove_dir_all(&work));
   nix::succeed("cp", [
      OsStr::new("-r"),
      src.join(".").as_os_str(),
      work.as_os_str(),
   ])?;
   nix::succeed("chmod", [
      OsStr::new("-R"),
      OsStr::new("u+w"),
      work.as_os_str(),
   ])?;
   let mut path = OsString::new();
   for bin in &bins {
      path.push(bin);
      path.push(":");
   }
   path.push(var_os("PATH").unwrap_or_default());
   let tree = work.join(".tree");
   let made = Command::new(&luarocks)
      .arg("make")
      .arg("--tree")
      .arg(&tree)
      .args(["--only-deps", "--deps-mode", "one"])
      .arg(format!("--only-server={LUAROCKS}"))
      .env("PATH", &path)
      .current_dir(&work)
      .output()?;
   let listed = Command::new(&luarocks)
      .arg("list")
      .arg("--tree")
      .arg(&tree)
      .arg("--porcelain")
      .env("PATH", &path)
      .current_dir(&work)
      .output()?;
   remove_dir_all(&work)?;
   if !made.status.success() {
      let text = format!(
         "{}{}",
         String::from_utf8_lossy(&made.stdout),
         String::from_utf8_lossy(&made.stderr)
      );
      let lines: Vec<&str> = text.lines().collect();
      let kept = lines.len().saturating_sub(10);
      return Err(io::Error::other(format!(
         "luarocks make: {}",
         lines[kept..].join("\n")
      )));
   }
   Ok(String::from_utf8_lossy(&listed.stdout)
      .lines()
      .filter_map(|line| {
         let mut fields = line.split('\t');
         Some(Pick {
            name:    fields.next()?.to_owned(),
            version: fields.next()?.to_owned(),
         })
      })
      .collect())
}

/// The entries of the set the package at `src` needs.
pub fn lock(src: &Path, old: &Entries) -> io::Result<Entries> {
   let mut entries = Entries::new();
   for pick in solve(src)? {
      let previous = old.get(&pick.name);
      if let Some(fields) = previous
         && fields.get("version") == Some(&pick.version)
      {
         entries.insert(pick.name, fields.clone());
         continue;
      }
      let was = previous
         .and_then(|fields| fields.get("version"))
         .map(|release| format!(" (was {release})"))
         .unwrap_or_default();
      nix::say(&format!("  {} {}{was}", pick.name, pick.version));
      let hash = nix::prefetch_file(&format!(
         "{LUAROCKS}/{}-{}.src.rock",
         pick.name, pick.version
      ))?
      .hash;
      let mut fields = BTreeMap::new();
      fields.insert("version".to_owned(), pick.version);
      fields.insert("sha256".to_owned(), hash);
      entries.insert(pick.name, fields);
   }
   Ok(entries)
}
