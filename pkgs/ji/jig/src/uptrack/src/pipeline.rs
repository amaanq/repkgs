//! discover -> resolve -> decide -> apply -> verify. Each stage takes and
//! returns package records, only `resolve` talks to upstreams, only `apply`
//! writes files.

use std::{
   cmp::Ordering,
   collections::BTreeMap,
   env::{
      current_dir,
      temp_dir,
      var,
   },
   ffi::OsStr,
   fs::{
      create_dir_all,
      read_to_string,
      remove_dir_all,
      write,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      id,
   },
   time::{
      Duration,
      Instant,
   },
};

use jig::{
   syslibs::{
      Tables,
      load,
      wanted,
   },
   tree::parallel,
   unpack::unpack,
};
use serde::{
   Deserialize,
   Deserializer,
   Serialize,
   Serializer,
};
use serde_json::{
   Map,
   Value,
   json,
};
use toml_edit::{
   Array,
   DocumentMut,
   Item,
   Table,
   value,
};

use crate::{
   datasource::{
      self,
      Candidate,
   },
   hooks,
   http,
   nix,
   purl::Purl,
   sources::Package,
   version,
};

/// The package tree, `$UPTRACK_ROOT` or the working directory.
pub fn root() -> PathBuf {
   var("UPTRACK_ROOT").map_or_else(|_unset| current_dir().unwrap_or_default(), PathBuf::from)
}

pub fn tables() -> io::Result<Tables> {
   load(&root().join("builder/sys-libs.json"))
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Resolved {
   #[serde(flatten)]
   pub package:    Package,
   pub candidates: Vec<Candidate>,
   pub error:      Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Expanded {
   pub key:    String,
   pub url:    String,
   pub unpack: bool,
}

/// A plan entry, `to` None means nothing to do and `note` says why. In the
/// plan JSON the package's `pin` is the one to write, as `apply` reads it.
#[derive(Clone)]
pub struct Entry {
   pub package:   Package,
   pub from:      Option<String>,
   pub to:        Option<String>,
   pub note:      Option<String>,
   pub candidate: Option<Candidate>,
   pub pin:       Option<Map<String, Value>>,
   pub sources:   Vec<Expanded>,
}

#[derive(Deserialize, Serialize)]
struct Plain {
   #[serde(flatten)]
   package:   Package,
   from:      Option<String>,
   to:        Option<String>,
   note:      Option<String>,
   candidate: Option<Candidate>,
   #[serde(default)]
   sources:   Vec<Expanded>,
}

impl Serialize for Entry {
   fn serialize<Ser>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error>
   where
      Ser: Serializer,
   {
      let mut package = self.package.clone();
      if let Some(ref pin) = self.pin {
         package.pin.clone_from(pin);
      }
      Plain {
         package,
         from: self.from.clone(),
         to: self.to.clone(),
         note: self.note.clone(),
         candidate: self.candidate.clone(),
         sources: self.sources.clone(),
      }
      .serialize(serializer)
   }
}

impl<'de> Deserialize<'de> for Entry {
   fn deserialize<De>(deserializer: De) -> Result<Self, De::Error>
   where
      De: Deserializer<'de>,
   {
      let plain = Plain::deserialize(deserializer)?;
      let pin = plain.to.is_some().then(|| plain.package.pin.clone());
      Ok(Self {
         package: plain.package,
         from: plain.from,
         to: plain.to,
         note: plain.note,
         candidate: plain.candidate,
         pin,
         sources: plain.sources,
      })
   }
}

/// What to ask upstream for: [watch] if set, else the purl.
fn watch_purl(package: &Package) -> io::Result<Purl> {
   if let Some(ref text) = package.watch.purl {
      return text.parse();
   }
   if let Some(ref url) = package.watch.url {
      let mut qualifiers = BTreeMap::new();
      qualifiers.insert("url".to_owned(), url.clone());
      if let Some(ref pattern) = package.watch.regex {
         qualifiers.insert("regex".to_owned(), pattern.clone());
      }
      return Ok(Purl {
         kind: "generic".to_owned(),
         namespace: String::new(),
         name: package.name.clone(),
         qualifiers,
      });
   }
   package.upstream.purl.as_deref().unwrap_or_default().parse()
}

fn candidates_of(package: &Package) -> io::Result<Vec<Candidate>> {
   if let Some(ref hook) = package.hook
      && package.has_hook("resolve")
   {
      let listed = hooks::run(hook, "resolve", package)?;
      return Ok(listed
         .as_array()
         .map(|items| items.iter().filter_map(Value::as_object).cloned().collect())
         .unwrap_or_default());
   }
   datasource::versions(&watch_purl(package)?)
}

/// Adds `candidates` and `error` per package, the only stage that talks to
/// upstreams.
pub fn resolve(packages: &[Package]) -> io::Result<Vec<Resolved>> {
   parallel(packages, |package| {
      Ok(match candidates_of(package) {
         Ok(candidates) => {
            Resolved {
               package: package.clone(),
               candidates,
               error: None,
            }
         },
         Err(error) => {
            Resolved {
               package:    package.clone(),
               candidates: Vec::new(),
               error:      Some(error.to_string()),
            }
         },
      })
   })
}

/// `every` durations, "30d", "2w", "12h", "10min".
pub fn duration(text: &str) -> Option<Duration> {
   let digits = text
      .trim()
      .trim_end_matches(|ch: char| ch.is_ascii_alphabetic());
   let unit = text.trim().get(digits.len()..)?;
   let amount: u64 = digits.trim().parse().ok()?;
   let seconds = match unit.trim() {
      "s" | "sec" | "secs" | "second" | "seconds" => 1,
      "min" | "mins" | "minute" | "minutes" => 60,
      "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
      "d" | "day" | "days" => 86_400,
      "w" | "wk" | "wks" | "week" | "weeks" => 604_800,
      _ => return None,
   };
   Some(Duration::from_secs(amount * seconds))
}

/// Seconds since the epoch of a date's day, `YYYY-MM-DD` with anything after.
fn epoch_seconds(date: &str) -> Option<u64> {
   let mut parts = date.get(..10)?.split('-');
   let year: i64 = parts.next()?.parse().ok()?;
   let month: i64 = parts.next()?.parse().ok()?;
   let day: i64 = parts.next()?.parse().ok()?;
   let shifted = if month <= 2 { year - 1 } else { year };
   let era = shifted.div_euclid(400);
   let year_of_era = shifted - era * 400;
   let month_index = (month + 9).rem_euclid(12);
   let day_of_year = (153 * month_index + 2) / 5 + day - 1;
   let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
   let days = era * 146_097 + day_of_era - 719_468;
   u64::try_from(days * 86_400).ok()
}

fn day_of(date: &str) -> &str {
   date.get(..10).unwrap_or(date)
}

/// url templates: every [pin] key as {key}, plus {version_} (dots as
/// underscores) {major} {minor}, and {tag} falling back to the version.
pub fn expand(template: &str, pin: &Map<String, Value>) -> String {
   let release = pin
      .get("version")
      .and_then(Value::as_str)
      .unwrap_or_default();
   let parts: Vec<&str> = release.split('.').collect();
   let mut pairs: Vec<(String, String)> = vec![
      ("tag".to_owned(), release.to_owned()),
      ("version_".to_owned(), parts.join("_")),
      (
         "major".to_owned(),
         parts.first().copied().unwrap_or_default().to_owned(),
      ),
      (
         "minor".to_owned(),
         parts.get(1).copied().unwrap_or("0").to_owned(),
      ),
   ];
   for (key, item) in pin {
      if key == "sys" {
         continue;
      }
      let text = item
         .as_str()
         .map_or_else(|| item.to_string(), str::to_owned);
      if let Some(pair) = pairs.iter_mut().find(|pair| pair.0 == *key) {
         pair.1 = text;
      } else {
         pairs.push((key.clone(), text));
      }
   }
   pairs.iter().fold(template.to_owned(), |acc, pair| {
      acc.replace(&format!("{{{}}}", pair.0), &pair.1)
   })
}

/// Adds `from`, `to`, `note`, and for an update `pin` (the [pin] table to
/// write) and the expanded `sources`. Pure.
pub fn decide(resolved: &[Resolved], prerelease: bool) -> Vec<Entry> {
   resolved
      .iter()
      .map(|found| decide_one(found, prerelease))
      .collect()
}

struct Sieve<'found> {
   eligible: Vec<&'found Candidate>,
   held:     usize,
}

/// The candidates allow, prerelease and `every` (releases younger than the
/// interval are held) leave.
fn sieve(found: &Resolved, prerelease: bool) -> Sieve<'_> {
   let upstream = &found.package.upstream;
   let accept_pre = prerelease || upstream.prerelease.unwrap_or(false);
   let allowed: Vec<&Candidate> = found
      .candidates
      .iter()
      .filter(|candidate| accept_pre || !datasource::prerelease(candidate))
      .filter(|candidate| {
         version::satisfies(datasource::version_of(candidate), upstream.allow.as_deref())
      })
      .collect();
   let interval = upstream.every.as_deref().and_then(duration);
   let young = |candidate: &Candidate| {
      interval.is_some_and(|window| {
         datasource::date(candidate)
            .and_then(epoch_seconds)
            .is_some_and(|released| http::now().saturating_sub(released) < window.as_secs())
      })
   };
   let held = allowed.iter().filter(|candidate| young(candidate)).count();
   Sieve {
      eligible: allowed
         .iter()
         .copied()
         .filter(|candidate| !young(candidate))
         .collect(),
      held,
   }
}

fn decide_one(found: &Resolved, prerelease: bool) -> Entry {
   let package = &found.package;
   let upstream = &package.upstream;
   let current = package.version().map(str::to_owned);
   let sieve = sieve(found, prerelease);
   let eligible = sieve.eligible;
   let held = sieve.held;
   let best = version::max(
      eligible
         .iter()
         .map(|candidate| datasource::version_of(candidate)),
   )
   .map(str::to_owned);
   let mut entry = Entry {
      package:   package.clone(),
      from:      current.clone(),
      to:        None,
      note:      None,
      candidate: None,
      pin:       None,
      sources:   Vec::new(),
   };
   let problem = found.error.clone().or_else(|| {
      if found.candidates.is_empty() {
         Some("datasource returned no versions".to_owned())
      } else if best.is_none() {
         Some(format!(
            "all {} candidates filtered by allow/prerelease/every",
            found.candidates.len()
         ))
      } else {
         None
      }
   });
   if problem.is_some() {
      entry.note = problem;
      return entry;
   }
   let Some(newest) = best else {
      return entry;
   };
   if let Some(ref have) = current
      && version::cmp(&newest, have) != Ordering::Greater
   {
      return entry;
   }
   let Some(chosen) = eligible
      .iter()
      .copied()
      .find(|candidate| datasource::version_of(candidate) == newest)
   else {
      return entry;
   };
   let newer_pre = version::max(
      found
         .candidates
         .iter()
         .filter(|candidate| datasource::prerelease(candidate))
         .map(datasource::version_of)
         .filter(|release| version::cmp(release, &newest) == Ordering::Greater),
   )
   .map(str::to_owned);
   let notes = update_notes(
      chosen,
      held,
      upstream.every.as_deref(),
      newer_pre.as_deref(),
   );
   let pin = candidate_pin(chosen);
   entry.sources = package
      .source
      .iter()
      .map(|source| {
         Expanded {
            key:    source.key.clone(),
            url:    expand(&source.url, &pin),
            unpack: source.unpacks(),
         }
      })
      .collect();
   entry.to = Some(newest);
   entry.note = Some(notes);
   entry.candidate = Some(chosen.clone());
   entry.pin = Some(pin);
   entry
}

fn update_notes(
   chosen: &Candidate,
   held: usize,
   every: Option<&str>,
   newer_pre: Option<&str>,
) -> String {
   let mut notes = Vec::new();
   if let Some(released) = datasource::date(chosen) {
      notes.push(format!("released {}", day_of(released)));
   }
   if held > 0 {
      notes.push(format!(
         "{held} newer held by every={}",
         every.unwrap_or_default()
      ));
   }
   if let Some(pre) = newer_pre {
      notes.push(format!("pre-release {pre} ignored"));
   }
   notes.join(", ")
}

/// [pin] = the candidate minus datasource bookkeeping. Whatever else a
/// resolve hook put there (jdk-bootstrap's file name spelling) is kept and
/// usable as {key} in urls.
fn candidate_pin(chosen: &Candidate) -> Map<String, Value> {
   let mut pin = Map::new();
   for (key, item) in chosen {
      if matches!(key.as_str(), "prerelease" | "url" | "sha256") || item.is_null() {
         continue;
      }
      if key == "date" {
         if let Some(released) = item.as_str() {
            pin.insert(key.clone(), Value::String(day_of(released).to_owned()));
         }
         continue;
      }
      pin.insert(key.clone(), item.clone());
   }
   pin
}

pub struct Prefetched {
   pub hash: String,
   pub sys:  Vec<String>,
}

/// The hash nix/sources.nix expects: the NAR hash of the unpacked tree, or
/// the file's own for `unpack = false`. `sys`: the libraries of ours the
/// tree's lock files can link, for [pin].
pub fn prefetch(url: &str, unpacks: bool) -> io::Result<Prefetched> {
   let fetched = nix::prefetch_file(url)?;
   if !unpacks {
      return Ok(Prefetched {
         hash: fetched.hash,
         sys:  Vec::new(),
      });
   }
   let scratch = temp_dir().join(format!("uptrack-tree.{}", id()));
   drop(remove_dir_all(&scratch));
   create_dir_all(&scratch)?;
   let tree = scratch.join("src");
   unpack("bsdtar", Path::new(&fetched.store_path), &tree)?;
   let found = Prefetched {
      hash: nix::hash_path(&tree)?,
      sys:  wanted(&tables()?, &tree)?,
   };
   remove_dir_all(&scratch)?;
   Ok(found)
}

fn document(file: &Path) -> io::Result<DocumentMut> {
   read_to_string(file)?
      .parse::<DocumentMut>()
      .map_err(|error| io::Error::other(format!("{}: {error}", file.display())))
}

fn toml_item(item: &Value) -> Item {
   match *item {
      Value::Bool(flag) => value(flag),
      Value::Number(ref number) => {
         number
            .as_i64()
            .map_or_else(|| value(number.to_string()), value)
      },
      Value::String(ref text) => value(text.as_str()),
      Value::Array(ref items) => {
         let mut array = Array::new();
         for element in items {
            match *element {
               Value::String(ref text) => array.push(text.as_str()),
               Value::Null
               | Value::Bool(_)
               | Value::Number(_)
               | Value::Array(_)
               | Value::Object(_) => array.push(element.to_string()),
            }
         }
         value(array)
      },
      Value::Null | Value::Object(_) => value(item.to_string()),
   }
}

/// [pin] as `pin` plus `sys` in place of the old table, the tree's key order:
/// version, tag, whatever else a hook added, date, sys.
fn set_pin(doc: &mut DocumentMut, pin: &Map<String, Value>, sys: &[String]) {
   let mut table = Table::new();
   for fixed in ["version", "tag"] {
      if let Some(item) = pin.get(fixed) {
         table.insert(fixed, toml_item(item));
      }
   }
   for (key, item) in pin {
      if !matches!(key.as_str(), "version" | "tag" | "date" | "sys") {
         table.insert(key, toml_item(item));
      }
   }
   if let Some(item) = pin.get("date") {
      table.insert("date", toml_item(item));
   }
   if !sys.is_empty() {
      table.insert("sys", toml_item(&json!(sys)));
   }
   table.set_position(
      doc.get("pin")
         .and_then(Item::as_table)
         .and_then(Table::position),
   );
   doc.insert("pin", Item::Table(table));
}

fn pin_sys(doc: &DocumentMut) -> Vec<String> {
   doc.get("pin")
      .and_then(Item::as_table)
      .and_then(|pin| pin.get("sys"))
      .and_then(Item::as_array)
      .map(|listed| {
         listed
            .iter()
            .filter_map(|item| item.as_str())
            .map(str::to_owned)
            .collect()
      })
      .unwrap_or_default()
}

/// recipe.toml with `hash` filled in for every [[source]] and [pin] set to
/// `pin` plus `sys`. `known`: key -> hash already at hand (an
/// upstream-published sha256), not prefetched again.
fn with_hashes(
   package: &Package,
   doc: &mut DocumentMut,
   pin: &Map<String, Value>,
   known: &Map<String, Value>,
) -> io::Result<()> {
   let mut sys: Vec<String> = Vec::new();
   for (index, source) in package.source.iter().enumerate() {
      let url = expand(&source.url, pin);
      nix::say(&format!("  {url}"));
      // a frozen source keeps its hash, a known one needs no download
      let have = if source.frozen.is_some() {
         source.hash.clone()
      } else {
         known
            .get(&source.key)
            .and_then(Value::as_str)
            .map(str::to_owned)
      };
      let fetched = match have {
         Some(hash) => {
            Prefetched {
               hash,
               sys: Vec::new(),
            }
         },
         None => prefetch(&url, source.unpacks())?,
      };
      if let Some(entry) = doc
         .get_mut("source")
         .and_then(Item::as_array_of_tables_mut)
         .and_then(|tables| tables.get_mut(index))
      {
         entry.insert("hash", value(fetched.hash.as_str()));
      }
      for lib in fetched.sys {
         if !sys.contains(&lib) {
            sys.push(lib);
         }
      }
   }
   sys.sort();
   // [locks] hackage: no lock file in the tree, sys is the lock step's
   // (set-sys) and kept here
   if package.locks.contains_key("hackage") {
      sys = pin_sys(doc);
   }
   if !sys.is_empty() {
      nix::say(&format!("  sys: {}", sys.join(" ")));
   }
   set_pin(doc, pin, &sys);
   Ok(())
}

/// recipe.toml as treefmt (taplo) formats it, so an update commits clean.
fn save(doc: &DocumentMut, file: &Path) -> io::Result<()> {
   write(file, doc.to_string())?;
   drop(Command::new("taplo").args(["format"]).arg(file).output());
   Ok(())
}

/// [pin] sys = `sys` for ecosystems whose lock lives outside the source.
pub fn set_sys(package: &Package, sys: &[String]) -> io::Result<()> {
   let mut doc = document(&package.file)?;
   if pin_sys(&doc) == sys {
      return Ok(());
   }
   nix::say(&format!("  sys: {}", sys.join(" ")));
   let pin = doc
      .get("pin")
      .and_then(Item::as_table)
      .map(|table| {
         table
            .iter()
            .filter(|pair| pair.0 != "sys")
            .map(|pair| (pair.0.to_owned(), json_item(pair.1)))
            .collect::<Map<String, Value>>()
      })
      .unwrap_or_default();
   set_pin(&mut doc, &pin, sys);
   save(&doc, &package.file)
}

fn json_item(item: &Item) -> Value {
   if let Some(text) = item.as_str() {
      return Value::String(text.to_owned());
   }
   if let Some(number) = item.as_integer() {
      return json!(number);
   }
   if let Some(flag) = item.as_bool() {
      return Value::Bool(flag);
   }
   if let Some(listed) = item.as_array() {
      return Value::Array(
         listed
            .iter()
            .map(|element| {
               element.as_str().map_or_else(
                  || Value::String(element.to_string()),
                  |text| Value::String(text.to_owned()),
               )
            })
            .collect(),
      );
   }
   Value::String(item.to_string())
}

/// Re-prefetch every source at the current pin (after editing a url, or for
/// sys), no version change.
pub fn rehash(package: &Package) -> io::Result<()> {
   if package.version().is_none() {
      return Err(io::Error::other(format!(
         "{}: no [pin] version to rehash at",
         package.file.display()
      )));
   }
   let mut doc = document(&package.file)?;
   let pin = package.pin.clone();
   with_hashes(package, &mut doc, &pin, &Map::new())?;
   save(&doc, &package.file)
}

/// Write hashes + [pin] for the decided update into recipe.toml, then the
/// `files` hook's outputs.
pub fn apply(entry: &Entry) -> io::Result<()> {
   let (Some(candidate), Some(pin)) = (entry.candidate.as_ref(), entry.pin.as_ref()) else {
      return Ok(());
   };
   // a registry-published sha256 is the flat file's: usable only where we keep
   // the file as is
   let mut known = Map::new();
   if let (Some(url), Some(digest)) = (
      candidate.get("url").and_then(Value::as_str),
      candidate.get("sha256").and_then(Value::as_str),
   ) {
      for source in &entry.sources {
         if !source.unpack && source.url == url {
            known.insert(source.key.clone(), Value::String(nix::sri(digest)?));
         }
      }
   }
   let mut doc = document(&entry.package.file)?;
   with_hashes(&entry.package, &mut doc, pin, &known)?;
   save(&doc, &entry.package.file)?;
   if let Some(ref hook) = entry.package.hook
      && entry.package.has_hook("files")
   {
      let written = hooks::run(hook, "files", entry)?;
      if let Some(files) = written.as_object() {
         for (path, content) in files {
            let text = content
               .as_str()
               .map_or_else(|| content.to_string(), str::to_owned);
            write(entry.package.dir.join(path), text)?;
            nix::say(&format!("  wrote {path}"));
         }
      }
   }
   Ok(())
}

#[derive(Serialize)]
pub struct Outcome {
   pub name:     String,
   pub verified: bool,
   pub took:     String,
   pub closure:  Option<String>,
   pub log:      Option<String>,
}

fn elapsed(start: Instant) -> String {
   let seconds = start.elapsed().as_secs();
   if seconds >= 60 {
      format!("{}min {}s", seconds / 60, seconds % 60)
   } else {
      format!("{seconds}s")
   }
}

/// nix-build the package (or run its verify hook).
pub fn verify(package: &Package) -> io::Result<Outcome> {
   if let Some(ref hook) = package.hook
      && package.has_hook("verify")
   {
      let answer = hooks::run(hook, "verify", package)?;
      return Ok(Outcome {
         name:     package.name.clone(),
         verified: answer
            .get("verified")
            .and_then(Value::as_bool)
            .unwrap_or(false),
         took:     String::new(),
         closure:  None,
         log:      answer.get("log").and_then(Value::as_str).map(str::to_owned),
      });
   }
   let start = Instant::now();
   let built = nix::capture("nix-build", [
      root().as_os_str(),
      OsStr::new("-A"),
      OsStr::new(&package.name),
      OsStr::new("--no-out-link"),
   ])?;
   if built.status != 0_i32 {
      let tail: Vec<&str> = built.stderr.lines().collect();
      let kept = tail.len().saturating_sub(40);
      return Ok(Outcome {
         name:     package.name.clone(),
         verified: false,
         took:     elapsed(start),
         closure:  None,
         log:      Some(tail[kept..].join("\n")),
      });
   }
   let out = built.stdout.lines().last().unwrap_or_default().to_owned();
   let info = nix::succeed("nix", ["path-info", "-S", "--json", out.as_str()])?;
   let parsed: Value = serde_json::from_str(&info)?;
   let first = parsed
      .as_object()
      .and_then(|map| map.values().next().cloned())
      .or_else(|| {
         let items = parsed.as_array()?;
         items.first().cloned()
      });
   let closure = first
      .and_then(|entry| entry.get("closureSize").and_then(Value::as_u64))
      .map(nix::filesize);
   Ok(Outcome {
      name: package.name.clone(),
      verified: true,
      took: elapsed(start),
      closure,
      log: None,
   })
}
