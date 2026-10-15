//! `locks/luarocks.toml`: the set's rock versions, `<rock> = { version,
//! sha256 }`, `sha256` of luarocks.org's `<rock>-<version>.src.rock` (the
//! manifest publishes none). A rock luarocks.org serves only as a rockspec
//! adds `rockspec`, that file's hash, and `url`, the source archive its
//! rockspec names, which `sha256` then hashes.
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
   time::Duration,
};

use mummer::process::{
   loose,
   strict,
};
use serde::Deserialize;

use crate::{
   http,
   locks::Entries,
   nix,
   pipeline::root,
};

const LUAROCKS: &str = "https://luarocks.org";

const SOURCE_URL: &str = r#"
local env = {}
assert(loadfile(os.getenv("ROCKSPEC"), "t", env))()
local url = env.source.url
local base = url:match("[^/]+$")
assert(url:match("^https?://") and base, "source.url " .. url .. " is not an archive")
assert((env.source.file or base) == base, "source.file differs from the url's basename")
io.write(url)
"#;

struct Pick {
   name:    String,
   version: String,
}

/// The tree's lua and its major.minor.
struct Tools {
   lua:         PathBuf,
   lua_version: String,
}

#[derive(Deserialize)]
struct Manifest {
   repository: BTreeMap<String, BTreeMap<String, Vec<Arch>>>,
}

#[derive(Deserialize)]
struct Arch {
   arch: String,
}

/// What luarocks installs for the rockspec under `src`, with the tree's cc
/// for C modules.
fn solve(src: &Path) -> io::Result<(Vec<Pick>, Tools)> {
   let built = nix::succeed("nix-build", [
      root().join("nix/set.nix").as_os_str(),
      OsStr::new("-A"),
      OsStr::new("pkgs.luarocks"),
      OsStr::new("-A"),
      OsStr::new("pkgs.lua"),
      OsStr::new("-A"),
      OsStr::new("toolchain"),
      OsStr::new("--no-out-link"),
   ])?;
   let bins = built
      .lines()
      .map(|line| PathBuf::from(line.trim()).join("bin"))
      .collect::<Vec<PathBuf>>();
   let luarocks = bins
      .first()
      .ok_or_else(|| io::Error::other("luarocks did not build"))?
      .join("luarocks");
   let lua = bins
      .get(1)
      .ok_or_else(|| io::Error::other("lua did not build"))?
      .join("lua");
   let work = temp_dir().join(format!("spieler-luarocks.{}", id()));
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
   let configured = Command::new(&luarocks)
      .args(["config", "lua_version"])
      .env("PATH", &path)
      .output()?;
   remove_dir_all(&work)?;
   if !made.status.success() {
      let text = format!("{}{}", loose(&made.stdout), loose(&made.stderr));
      let lines = text.lines().collect::<Vec<&str>>();
      let kept = lines.len().saturating_sub(10);
      return Err(io::Error::other(format!(
         "luarocks make: {}",
         lines[kept..].join("\n")
      )));
   }
   let picks = strict(&listed.stdout, "luarocks list")?
      .lines()
      .filter_map(|line| {
         let mut fields = line.split('\t');
         Some(Pick {
            name:    fields.next()?.to_owned(),
            version: fields.next()?.to_owned(),
         })
      })
      .collect();
   let lua_version = strict(&configured.stdout, "luarocks config")?
      .trim()
      .to_owned();
   Ok((picks, Tools { lua, lua_version }))
}

/// Whether luarocks.org's manifest lists a .src.rock of the pick.
fn has_src_rock(manifest: &Manifest, pick: &Pick) -> io::Result<bool> {
   let arches = manifest
      .repository
      .get(&pick.name)
      .and_then(|versions| versions.get(&pick.version))
      .ok_or_else(|| {
         io::Error::other(format!(
            "luarocks.org lists no {} {}",
            pick.name, pick.version
         ))
      })?;
   Ok(arches.iter().any(|served| served.arch == "src"))
}

/// The source archive url the rockspec at `rockspec` names.
fn source_url(lua: &Path, rockspec: &str) -> io::Result<String> {
   let read = Command::new(lua)
      .args(["-e", SOURCE_URL])
      .env("ROCKSPEC", rockspec)
      .output()?;
   if !read.status.success() {
      return Err(io::Error::other(format!(
         "{rockspec}: {}",
         loose(&read.stderr).trim()
      )));
   }
   Ok(strict(&read.stdout, "rockspec source")?.to_owned())
}

/// The entries of the set the package at `src` needs.
pub fn lock(src: &Path, old: &Entries) -> io::Result<Entries> {
   let mut entries = Entries::new();
   let (picks, tools) = solve(src)?;
   let mut manifest = None;
   for pick in picks {
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
      if manifest.is_none() {
         let url = format!("{LUAROCKS}/manifest-{}.json", tools.lua_version);
         let response = http::cached(&url, Duration::from_secs(600))?;
         if response.status != 200 {
            return Err(io::Error::other(format!(
               "GET {url} -> {}",
               response.status
            )));
         }
         manifest = Some(serde_json::from_str::<Manifest>(&response.body)?);
      }
      let listed = manifest
         .as_ref()
         .ok_or_else(|| io::Error::other("luarocks.org manifest unread"))?;
      let stem = format!("{}-{}", pick.name, pick.version);
      let mut fields = BTreeMap::new();
      if has_src_rock(listed, &pick)? {
         let hash = nix::prefetch_file(&format!("{LUAROCKS}/{stem}.src.rock"))?.hash;
         fields.insert("sha256".to_owned(), hash);
      } else {
         let rockspec = nix::prefetch_file(&format!("{LUAROCKS}/{stem}.rockspec"))?;
         let url = source_url(&tools.lua, &rockspec.store_path)?;
         fields.insert("sha256".to_owned(), nix::prefetch_file(&url)?.hash);
         fields.insert("rockspec".to_owned(), rockspec.hash);
         fields.insert("url".to_owned(), url);
      }
      fields.insert("version".to_owned(), pick.version);
      entries.insert(pick.name, fields);
   }
   Ok(entries)
}
