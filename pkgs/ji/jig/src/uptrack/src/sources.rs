//! `package.toml`, one per package, validated into package records.
//!
//! Humans write `[upstream]`, `[[source]].url`, `[watch]` and `[locks]`, the
//! tool writes `hash` and `[pin]`.

use std::{
   collections::BTreeMap,
   fs::{
      read_dir,
      read_to_string,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
};

use serde::{
   Deserialize,
   Serialize,
   de::IgnoredAny,
};
use serde_json::{
   Map,
   Value,
};

use crate::hooks;

const LOCK_ECOSYSTEMS: [&str; 3] = ["go", "hackage", "luarocks"];

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
   pub purl:       Option<String>,
   pub allow:      Option<String>,
   pub prerelease: Option<bool>,
   pub every:      Option<String>,
   pub group:      Option<String>,
   pub cpe:        Option<String>,
   pub frozen:     Option<String>,
   #[serde(default)]
   pub relocks:    Vec<String>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Watch {
   pub url:   Option<String>,
   pub regex: Option<String>,
   pub purl:  Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
   pub key:    String,
   pub url:    String,
   pub hash:   Option<String>,
   pub unpack: Option<bool>,
   pub name:   Option<String>,
   pub frozen: Option<String>,
}

impl Source {
   pub fn unpacks(&self) -> bool {
      self.unpack.unwrap_or(true)
   }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
   #[serde(default)]
   upstream: Upstream,
   #[serde(default)]
   source:   Vec<Source>,
   #[serde(default)]
   pin:      Map<String, Value>,
   #[serde(default)]
   watch:    Watch,
   #[serde(default)]
   locks:    BTreeMap<String, String>,
   // the build half of package.toml, nix/package/manifest.nix reads and checks it
   #[serde(default, rename = "package")]
   _package: Option<IgnoredAny>,
   #[serde(default, rename = "vars")]
   _vars:    Option<IgnoredAny>,
   #[serde(default, rename = "when")]
   _when:    Option<IgnoredAny>,
   #[serde(default, rename = "unless")]
   _unless:  Option<IgnoredAny>,
   #[serde(default, rename = "from")]
   _from:    Option<IgnoredAny>,
}

/// A package as its package.toml describes it, what every stage passes on.
#[derive(Clone, Deserialize, Serialize)]
pub struct Package {
   pub name:     String,
   pub dir:      PathBuf,
   pub file:     PathBuf,
   pub tracked:  bool,
   pub hook:     Option<PathBuf>,
   pub hooks:    Vec<String>,
   pub upstream: Upstream,
   pub source:   Vec<Source>,
   pub pin:      Map<String, Value>,
   pub watch:    Watch,
   pub locks:    BTreeMap<String, String>,
}

impl Package {
   pub fn version(&self) -> Option<&str> {
      self.pin.get("version").and_then(Value::as_str)
   }

   pub fn has_hook(&self, stage: &str) -> bool {
      self.hooks.iter().any(|hook| hook == stage)
   }
}

fn sources_files(root: &Path) -> io::Result<Vec<PathBuf>> {
   let mut found = Vec::new();
   let mut pending = vec![root.to_path_buf()];
   while let Some(directory) = pending.pop() {
      for child in read_dir(&directory)? {
         let path = child?.path();
         let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
         if path.is_dir() {
            if !name.starts_with('.') && name != "target" && !name.starts_with("result") {
               pending.push(path);
            }
         } else if name == "package.toml" {
            found.push(path);
         }
      }
   }
   found.sort();
   Ok(found)
}

fn parse(file: &Path) -> io::Result<Option<Package>> {
   let parsed: File = toml::from_str(&read_to_string(file)?)
      .map_err(|error| io::Error::other(format!("{}: {error}", file.display())))?;
   let fail = |message: String| io::Error::other(format!("{}: {message}", file.display()));
   for ecosystem in parsed.locks.keys() {
      if !LOCK_ECOSYSTEMS.contains(&ecosystem.as_str()) {
         return Err(fail(format!(
            "unknown locks key {ecosystem}. Known: {}",
            LOCK_ECOSYSTEMS.join(" ")
         )));
      }
   }
   // in-tree sources (source = ./src) have nothing to track but may still lock
   // dependencies. `frozen = "<reason>"` pins a dead upstream: nothing to
   // poll, hash stays as written
   let tracked = parsed.upstream.purl.is_some();
   let frozen = parsed.upstream.frozen.is_some();
   let unsourced = parsed.source.is_empty() && parsed.pin.is_empty() && parsed.locks.is_empty();
   if !tracked && !frozen && unsourced {
      return Ok(None);
   }
   if !tracked && !frozen && (!parsed.source.is_empty() || parsed.locks.is_empty()) {
      return Err(fail("upstream.purl (or frozen) is required".to_owned()));
   }
   for source in &parsed.source {
      // a bump would keep fetching the old file under the new version. `frozen
      // = "<reason>"` on the source: a file that does not follow the pin
      // (a distro's build for one cpu)
      if tracked && source.frozen.is_none() && !source.url.contains('{') {
         return Err(fail(format!(
            "source {} url has no {{placeholder}} (frozen = \"reason\" if it is not meant to \
             follow the version)",
            source.key
         )));
      }
   }
   let dir = file
      .parent()
      .ok_or_else(|| fail("no parent directory".to_owned()))?
      .to_path_buf();
   let name = dir
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_default();
   let hook_file = dir.join("update.nu");
   let hook = hook_file.exists().then_some(hook_file);
   let hooks = match hook {
      Some(ref path) => hooks::exports(path)?,
      None => Vec::new(),
   };
   Ok(Some(Package {
      name,
      dir,
      file: file.to_path_buf(),
      tracked,
      hook,
      hooks,
      upstream: parsed.upstream,
      source: parsed.source,
      pin: parsed.pin,
      watch: parsed.watch,
      locks: parsed.locks,
   }))
}

/// Every package.toml under `root`, validated, as package records.
pub fn discover(root: &Path) -> io::Result<Vec<Package>> {
   sources_files(root)?
      .iter()
      .filter_map(|file| parse(file).transpose())
      .collect()
}
