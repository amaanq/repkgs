//! `act.toml` and `act.lock`, one pair per package, validated into package
//! records (docs/act.md). Humans write act.toml, spieler writes act.lock.

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
};
use serde_json::{
   Map,
   Value,
};

use crate::{
   hooks,
   tag::Template,
};

const LOCK_ECOSYSTEMS: [&str; 3] = ["go", "hackage", "luarocks"];

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
   pub purl:       Option<String>,
   pub allow:      Option<String>,
   pub prerelease: Option<bool>,
   pub every:      Option<String>,
   pub base:       Option<String>,
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

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackTable {
   from:       Option<String>,
   allow:      Option<String>,
   prerelease: Option<bool>,
   every:      Option<String>,
   base:       Option<String>,
   group:      Option<String>,
   cpe:        Option<String>,
   frozen:     Option<String>,
   #[serde(default)]
   relocks:    Vec<String>,
   #[serde(default)]
   watch:      Watch,
   #[serde(default)]
   locks:      BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Track {
   Short(String),
   Table(Box<TrackTable>),
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fetched {
   url:     Option<String>,
   asset:   Option<String>,
   archive: Option<bool>,
   from:    Option<String>,
   unpack:  Option<bool>,
   name:    Option<String>,
   frozen:  Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Fetch {
   One(Fetched),
   Many(BTreeMap<String, Fetched>),
}

// the build keys of act.toml are nix/package/manifest.nix's to read and check
#[derive(Deserialize)]
struct Act {
   tag:     Option<String>,
   track:   Option<Track>,
   fetch:   Option<Fetch>,
   patches: Option<Vec<String>>,
}

/// A package as its act.toml and act.lock describe it, what every stage passes
/// on. `file` is the act.lock spieler writes.
#[derive(Clone, Deserialize, Serialize)]
pub struct Package {
   pub name:      String,
   pub dir:       PathBuf,
   pub file:      PathBuf,
   pub tracked:   bool,
   pub hook:      Option<PathBuf>,
   pub hooks:     Vec<String>,
   pub upstream:  Upstream,
   pub source:    Vec<Source>,
   pub pin:       Map<String, Value>,
   pub watch:     Watch,
   pub locks:     BTreeMap<String, String>,
   /// act.toml's tag template, `v{version}`.
   pub tag:       Option<String>,
   /// One inline `fetch`, so act.lock holds `hash` rather than `[hash]`.
   pub single:    bool,
   /// act.toml's `patches`, in the order they apply.
   pub patches:   Vec<String>,
   /// act.lock's `[patches]`, remote entry -> the file it was vendored as.
   pub vendored:  BTreeMap<String, String>,
   /// act.lock's `[sources]`, what the `sources` hook named for the pin.
   pub generated: Vec<Source>,
}

impl Package {
   /// act.toml's tag template, checked when the package was read.
   pub fn template(&self) -> Option<Template> {
      let raw = self.tag.as_deref()?;
      raw.parse().ok()
   }

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
         } else if name == "act.toml" {
            found.push(path);
         }
      }
   }
   found.sort();
   Ok(found)
}

/// The source's url, `asset` and `archive` spelled out against the github
/// repository `track` names.
fn url_of(fetched: &Fetched, purl: Option<&str>) -> Result<String, String> {
   if let Some(ref url) = fetched.url {
      return Ok(url.clone());
   }
   let repo = purl
      .and_then(|text| text.strip_prefix("pkg:github/"))
      .filter(|repo| repo.matches('/').count() == 1 && !repo.contains('?'))
      .ok_or("asset and archive need a github track")?;
   if let Some(ref asset) = fetched.asset {
      return Ok(format!(
         "https://github.com/{repo}/releases/download/{{tag}}/{asset}"
      ));
   }
   if fetched.archive == Some(true) {
      return Ok(format!(
         "https://github.com/{repo}/archive/refs/tags/{{tag}}.tar.gz"
      ));
   }
   Err("a source needs url, asset or archive".to_owned())
}

/// `github:owner/repo` as the purl the datasources take.
fn purl_of(short: &str) -> String {
   format!("pkg:{}", short.replacen(':', "/", 1))
}

/// act.lock's `hash`, or its `[hash]` table by source.
fn hashes_of(recorded: Option<Value>) -> BTreeMap<String, String> {
   match recorded {
      Some(Value::String(hash)) => BTreeMap::from([("default".to_owned(), hash)]),
      Some(Value::Object(listed)) => {
         listed
            .into_iter()
            .filter_map(|(key, hash)| hash.as_str().map(|text| (key, text.to_owned())))
            .collect()
      },
      _ => BTreeMap::new(),
   }
}

/// act.lock's `[sources]`, or a `sources` hook's answer, name -> `{ url, name
/// }`.
pub fn generated_of(
   recorded: Option<Value>,
   hashes: &BTreeMap<String, String>,
) -> Result<Vec<Source>, String> {
   let listed = match recorded {
      Some(Value::Object(listed)) => listed,
      None => return Ok(Vec::new()),
      Some(other) => return Err(format!("sources must be a table, not {other}")),
   };
   listed
      .into_iter()
      .map(|(key, entry)| {
         let field = |name: &str| entry.get(name).and_then(Value::as_str).map(str::to_owned);
         Ok(Source {
            url: field("url").ok_or_else(|| format!("source {key} has no url"))?,
            name: field("name"),
            hash: hashes.get(&key).cloned(),
            unpack: None,
            frozen: None,
            key,
         })
      })
      .collect()
}

/// `fetch` and then act.lock's `[sources]` as sources by name, with the
/// hashes act.lock records for them, taken out of `lock`.
fn sources_of(
   fetch: Option<Fetch>,
   lock: &mut Map<String, Value>,
   purl: Option<&str>,
) -> Result<(Vec<Source>, Vec<Source>), String> {
   let hashes = hashes_of(lock.remove("hash"));
   let tables = match fetch {
      Some(Fetch::One(one)) if one.from.is_none() => vec![("default".to_owned(), one)],
      Some(Fetch::Many(many)) => many.into_iter().collect(),
      Some(Fetch::One(_)) | None => Vec::new(),
   };
   let declared = tables
      .into_iter()
      .map(|(key, fetched)| {
         Ok(Source {
            url: url_of(&fetched, purl).map_err(|message| format!("source {key}: {message}"))?,
            hash: hashes.get(&key).cloned(),
            key,
            unpack: fetched.unpack,
            name: fetched.name,
            frozen: fetched.frozen,
         })
      })
      .collect::<Result<Vec<_>, String>>()?;
   Ok((declared, generated_of(lock.remove("sources"), &hashes)?))
}

/// act.lock's values as one map, `[vars]` among them, with the tag act.toml's
/// `tag` template spells where act.lock records none.
fn pin_of(mut lock: Map<String, Value>, tag: Option<&Template>) -> Map<String, Value> {
   let mut pin = match lock.remove("vars") {
      Some(Value::Object(vars)) => vars,
      _ => Map::new(),
   };
   pin.extend(lock);
   if let (Some(template), Some(version)) = (tag, pin.get("version").and_then(Value::as_str))
      && !pin.contains_key("tag")
      && let Some(spelt) = template.spell(version)
   {
      pin.insert("tag".to_owned(), Value::String(spelt));
   }
   pin
}

/// act.lock's `[patches]`, remote entry -> vendored file.
fn vendored_of(recorded: Option<Value>) -> BTreeMap<String, String> {
   match recorded {
      Some(Value::Object(listed)) => {
         listed
            .into_iter()
            .filter_map(|(entry, name)| name.as_str().map(|text| (entry, text.to_owned())))
            .collect()
      },
      _ => BTreeMap::new(),
   }
}

#[expect(
   clippy::too_many_lines,
   reason = "one linear read of act.toml and act.lock into a Package"
)]
fn parse(file: &Path) -> io::Result<Option<Package>> {
   let fail = |message: String| io::Error::other(format!("{}: {message}", file.display()));
   let act: Act =
      toml::from_str(&read_to_string(file)?).map_err(|error| fail(error.to_string()))?;
   let dir = file
      .parent()
      .ok_or_else(|| fail("no parent directory".to_owned()))?
      .to_path_buf();
   let lock_file = dir.join("act.lock");
   let mut lock: Map<String, Value> = if lock_file.exists() {
      toml::from_str(&read_to_string(&lock_file)?)
         .map_err(|error| io::Error::other(format!("{}: {error}", lock_file.display())))?
   } else {
      Map::new()
   };

   let table = match act.track {
      Some(Track::Short(short)) => {
         TrackTable {
            from: Some(short),
            ..TrackTable::default()
         }
      },
      Some(Track::Table(table)) => *table,
      None => TrackTable::default(),
   };
   for ecosystem in table.locks.keys() {
      if !LOCK_ECOSYSTEMS.contains(&ecosystem.as_str()) {
         return Err(fail(format!(
            "unknown locks key {ecosystem}. Known: {}",
            LOCK_ECOSYSTEMS.join(" ")
         )));
      }
   }
   let upstream = Upstream {
      purl:       table.from.as_deref().map(purl_of),
      allow:      table.allow,
      prerelease: table.prerelease,
      every:      table.every,
      base:       table.base,
      group:      table.group,
      cpe:        table.cpe,
      frozen:     table.frozen,
      relocks:    table.relocks,
   };

   let single = matches!(act.fetch, Some(Fetch::One(ref one)) if one.from.is_none());
   let (source, generated) =
      sources_of(act.fetch, &mut lock, upstream.purl.as_deref()).map_err(fail)?;

   let template = act
      .tag
      .as_deref()
      .map(str::parse::<Template>)
      .transpose()
      .map_err(|error| fail(error.to_string()))?;
   let vendored = vendored_of(lock.remove("patches"));
   let patches = act.patches.unwrap_or_default();
   let pin = pin_of(lock, template.as_ref());

   // in-tree sources have nothing to track but may still lock dependencies.
   // `frozen = "<reason>"` pins a dead upstream: nothing to poll, hash stays as
   // written
   let tracked = upstream.purl.is_some();
   let frozen = upstream.frozen.is_some();
   let unsourced = source.is_empty() && pin.is_empty() && table.locks.is_empty();
   if !tracked && !frozen && unsourced {
      return Ok(None);
   }
   if !tracked && !frozen && (!source.is_empty() || table.locks.is_empty()) {
      return Err(fail("track (or [track] frozen) is required".to_owned()));
   }
   for entry in &source {
      // a bump would keep fetching the old file under the new version. `frozen
      // = "<reason>"` on the source: a file that does not follow the pin
      // (a distro's build for one cpu)
      if tracked && entry.frozen.is_none() && !entry.url.contains('{') {
         return Err(fail(format!(
            "source {} url has no {{placeholder}} (frozen = \"reason\" if it is not meant to \
             follow the version)",
            entry.key
         )));
      }
   }
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
      file: lock_file,
      tracked,
      hook,
      hooks,
      upstream,
      source,
      pin,
      watch: table.watch,
      locks: table.locks,
      tag: act.tag,
      single,
      patches,
      vendored,
      generated,
   }))
}

/// Every act.toml under `root`, validated, as package records.
pub fn discover(root: &Path) -> io::Result<Vec<Package>> {
   sources_files(root)?
      .iter()
      .filter_map(|file| parse(file).transpose())
      .collect()
}
