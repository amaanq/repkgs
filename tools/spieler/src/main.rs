mod datasource;
mod go;
mod hackage;
mod hooks;
mod http;
mod locks;
mod luarocks;
mod nix;
mod patch;
mod pipeline;
mod purl;
mod sources;
mod tag;
mod version;

use std::{
   collections::BTreeMap,
   env::args_os,
   fs::{
      create_dir_all,
      read_to_string,
      remove_file,
      write,
   },
   io::{
      self,
      Write as _,
   },
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      ExitCode,
   },
   slice,
   time::Duration,
};

use misstep::Result;
use mummer::{
   files::utf8,
   process::{
      run_main,
      text_arguments,
   },
};
use pound::Parse;
use serde_json::Value;

use crate::{
   locks::Used,
   pipeline::{
      Entry,
      root,
   },
   purl::Purl,
   sources::Package,
};

/// upstream tracking for package trees, docs/spieler.md.
#[derive(Parse)]
#[pound(name = "spieler")]
enum Mode {
   /// parse and validate every act.toml.
   List { names: Vec<String> },
   /// resolve and decide, write nothing. Exits 10 when updates are pending.
   Check {
      names:      Vec<String>,
      /// plan entries instead of tables.
      #[pound(long)]
      json:       bool,
      /// accept pre-releases.
      #[pound(long)]
      prerelease: bool,
   },
   /// prefetch and write act.lock (+ hook files, locks) for pending
   /// updates. `verify` builds them.
   Apply {
      names:      Vec<String>,
      /// apply a saved `check --json` instead of resolving.
      #[pound(long)]
      plan:       Option<PathBuf>,
      #[pound(long)]
      prerelease: bool,
      #[pound(long)]
      commit:     bool,
   },
   /// build packages at their current pin (after a manual edit, or to
   /// re-check).
   Verify { names: Vec<String> },
   /// re-prefetch sources at the current pin, e.g. after editing a url.
   Rehash { names: Vec<String> },
   /// record dependency hashes in locks/<eco>.toml for packages with [locks],
   /// at their current pin.
   Lock {
      names: Vec<String>,
      /// afterwards drop entries no package uses any more (looks at every
      /// package, ignores names).
      #[pound(long)]
      prune: bool,
   },
   /// write a new act.toml and pin it to the newest version upstream
   /// lists.
   Init {
      /// package directory.
      dir:   PathBuf,
      /// pkg:github/owner/repo, pkg:pypi/name, pkg:gnu/name,
      /// pkg:generic/name?url=…
      #[pound(validate = |spelled: &String| spelled.parse::<Purl>().map(drop))]
      purl:  String,
      /// source template with {version} {version_} {major} {minor} {tag}.
      url:   String,
      /// version range to stay in, e.g. "==3.12.*".
      #[pound(long)]
      allow: Option<String>,
   },
   /// download the url entries of act.toml's `patches` beside the act and check
   /// the list applies at the current pin.
   Patch {
      names:  Vec<String>,
      /// download every url entry again, not only the missing ones.
      #[pound(long)]
      update: bool,
   },
   /// rewrite locks/<eco>.toml files in normal form, the treefmt entry point.
   Normalize { files: Vec<PathBuf> },
   /// the candidates a purl's datasource lists, as JSON, for update.nu hooks.
   Versions {
      #[pound(parse = str::parse::<Purl>)]
      purl: Purl,
   },
   /// a cached GET as JSON {status, body}, for update.nu hooks.
   Get {
      url:     String,
      /// serve from the cache without revalidating, "10min", "1day".
      #[pound(
         long,
         parse = |text: &str| pipeline::duration(text).ok_or("not a duration"),
         default = "10min"
      )]
      max_age: Duration,
   },
}

fn say(message: &str) {
   drop(writeln!(io::stderr().lock(), "{message}"));
}

fn print_json<Value>(value: &Value) -> io::Result<()>
where
   Value: serde::Serialize,
{
   let mut stdout = io::stdout().lock();
   serde_json::to_writer(&mut stdout, value)?;
   writeln!(stdout)?;
   Ok(())
}

/// `--all` also yields packages with nothing to poll (in-tree source, frozen
/// upstream).
fn selected(names: &[String], all: bool) -> io::Result<Vec<Package>> {
   let found = sources::discover(&root())?
      .into_iter()
      .filter(|package| all || package.tracked)
      .collect::<Vec<Package>>();
   let missing = names
      .iter()
      .map(String::as_str)
      .filter(|name| !found.iter().any(|package| package.name == *name))
      .collect::<Vec<&str>>();
   if !missing.is_empty() {
      return Err(io::Error::other(format!(
         "no act.toml for {}",
         missing.join(", ")
      )));
   }
   if names.is_empty() {
      return Ok(found);
   }
   Ok(found
      .into_iter()
      .filter(|package| names.contains(&package.name))
      .collect())
}

fn planned(names: &[String], prerelease: bool) -> io::Result<Vec<Entry>> {
   Ok(pipeline::decide(
      &pipeline::resolve(&selected(names, false)?)?,
      prerelease,
   ))
}

/// Rows aligned in columns, the way nu prints a table without an index.
fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
   let widths = headers
      .iter()
      .enumerate()
      .map(|(column, header)| {
         rows
            .iter()
            .map(|row| row.get(column).map_or(0, |cell| cell.chars().count()))
            .chain([header.chars().count()])
            .max()
            .unwrap_or(0)
      })
      .collect::<Vec<usize>>();
   let line = |cells: Vec<&str>| {
      cells
         .iter()
         .enumerate()
         .map(|(column, cell)| {
            format!(
               "{cell:<width$}",
               width = widths.get(column).copied().unwrap_or(0)
            )
         })
         .collect::<Vec<_>>()
         .join("  ")
         .trim_end()
         .to_owned()
   };
   let mut text = line(headers.to_vec());
   for row in rows {
      text.push('\n');
      text.push_str(&line(row.iter().map(String::as_str).collect()));
   }
   text
}

fn list(names: &[String]) -> io::Result<()> {
   let rows = selected(names, false)?
      .iter()
      .map(|package| {
         vec![
            package.name.clone(),
            package.upstream.purl.clone().unwrap_or_default(),
            package.version().unwrap_or_default().to_owned(),
            package.hooks.join(" "),
         ]
      })
      .collect::<Vec<Vec<String>>>();
   writeln!(
      io::stdout().lock(),
      "{}",
      table(&["name", "purl", "version", "hooks"], &rows)
   )?;
   Ok(())
}

fn check(names: &[String], json: bool, prerelease: bool) -> io::Result<i32> {
   let plan = planned(names, prerelease)?;
   if json {
      print_json(&plan)?;
      return Ok(0);
   }
   let problems = plan
      .iter()
      .filter(|entry| entry.to.is_none() && entry.note.is_some())
      .map(|entry| {
         vec![
            entry.package.name.clone(),
            entry.from.clone().unwrap_or_default(),
            entry.note.clone().unwrap_or_default(),
         ]
      })
      .collect::<Vec<Vec<String>>>();
   let pending = plan
      .iter()
      .filter(|entry| entry.to.is_some())
      .map(|entry| {
         vec![
            entry.package.name.clone(),
            entry.from.clone().unwrap_or_default(),
            entry.to.clone().unwrap_or_default(),
            entry.note.clone().unwrap_or_default(),
         ]
      })
      .collect::<Vec<Vec<String>>>();
   if !problems.is_empty() {
      say(&table(&["name", "from", "note"], &problems));
   }
   if !pending.is_empty() {
      say(&table(&["name", "from", "to", "note"], &pending));
   }
   say(&format!(
      "{} checked, {} pending, {} problems",
      plan.len(),
      pending.len(),
      problems.len()
   ));
   Ok(if pending.is_empty() { 0 } else { 10 })
}

fn apply_entry(entry: &Entry) -> io::Result<()> {
   say(&format!(
      "{}: {} → {}",
      entry.package.name,
      entry.from.as_deref().unwrap_or_default(),
      entry.to.as_deref().unwrap_or_default()
   ));
   let before = read_to_string(&entry.package.file)?;
   pipeline::apply(entry)?;
   // a patch that stops applying holds the package at the version it had
   for updated in selected(slice::from_ref(&entry.package.name), true)? {
      if let Err(error) = check_patches(&updated, &updated.vendored) {
         write(&entry.package.file, before)?;
         return Err(error);
      }
   }
   locks::add(&entry.package)?;
   // a solver input changed (the compiler a version set was solved for): redo
   // every lock of that ecosystem
   for eco in &entry.package.upstream.relocks {
      for package in selected(&[], true)? {
         if package.locks.contains_key(eco) {
            say(&format!("  relock {}", package.name));
            locks::add(&package)?;
         }
      }
   }
   Ok(())
}

fn commit_update(entry: &Entry) -> io::Result<()> {
   let directory = root().canonicalize()?;
   let package = entry.package.dir.canonicalize()?;
   let path = package.strip_prefix(&directory).map_err(io::Error::other)?;

   let date = entry
      .pin
      .as_ref()
      .and_then(|pin| pin.get("date"))
      .and_then(Value::as_str)
      .map(|day| format!("released {day}"));

   let body = entry
      .sources
      .iter()
      .map(|source| source.url.as_str())
      .chain(date.as_deref())
      .collect::<Vec<_>>()
      .join("\n");

   let message = format!(
      "{}: {} -> {}\n\n{body}\n",
      entry.package.name,
      entry.from.as_deref().unwrap_or_default(),
      entry.to.as_deref().unwrap_or_default()
   );

   let mut command = Command::new("jj");
   command
      .current_dir(&directory)
      .args(["commit", "--quiet", "-m", &message, "--"])
      .arg(path);

   if directory.join("locks").exists() {
      command.arg("locks");
   }

   if !command.status()?.success() {
      return Err(io::Error::other(format!(
         "jj commit failed for {}",
         entry.package.name
      )));
   }

   Ok(())
}

fn apply(
   names: &[String],
   plan: Option<&PathBuf>,
   prerelease: bool,
   commit: bool,
) -> io::Result<()> {
   let entries = match plan {
      Some(file) => serde_json::from_str::<Vec<Entry>>(&read_to_string(file)?)?,
      None => planned(names, prerelease)?,
   };
   let mut rows = Vec::new();
   for entry in entries.iter().filter(|entry| entry.to.is_some()) {
      apply_entry(entry)?;

      if commit {
         commit_update(entry)?;
      }

      rows.push(vec![
         entry.package.name.clone(),
         entry.from.clone().unwrap_or_default(),
         entry.to.clone().unwrap_or_default(),
      ]);
   }
   writeln!(
      io::stdout().lock(),
      "{}",
      table(&["name", "from", "to"], &rows)
   )?;
   Ok(())
}

fn verify(names: &[String]) -> io::Result<()> {
   let mut rows = Vec::new();
   for package in selected(names, false)? {
      let done = pipeline::verify(&package)?;
      if let Some(ref log) = done.log {
         say(log);
      }
      rows.push(vec![
         done.name,
         done.verified.to_string(),
         done.took,
         done.closure.unwrap_or_default(),
      ]);
   }
   writeln!(
      io::stdout().lock(),
      "{}",
      table(&["name", "verified", "took", "closure"], &rows)
   )?;
   Ok(())
}

fn rehash(names: &[String]) -> io::Result<()> {
   for package in selected(names, true)? {
      say(&package.name);
      if let Err(error) = pipeline::rehash(&package) {
         say(&format!("  failed: {error}"));
      }
   }
   Ok(())
}

fn lock(names: &[String], prune: bool) -> io::Result<()> {
   let packages = selected(if prune { &[] } else { names }, true)?
      .into_iter()
      .filter(|package| !package.locks.is_empty())
      .collect::<Vec<Package>>();
   let mut used = BTreeMap::<String, Vec<String>>::new();
   for package in &packages {
      say(&package.name);
      for found in locks::add(package)? {
         let keys = used.entry(found.eco).or_default();
         for key in found.keys {
            if !keys.contains(&key) {
               keys.push(key);
            }
         }
      }
   }
   if prune {
      let listed = used
         .into_iter()
         .map(|(eco, keys)| Used { eco, keys })
         .collect::<Vec<Used>>();
      locks::prune(&listed)?;
   }
   Ok(())
}

fn vendor_patches(names: &[String], update: bool) -> io::Result<()> {
   for package in selected(names, true)? {
      let mut vendored = BTreeMap::new();
      let mut fetched = Vec::new();
      for entry in &package.patches {
         let Some(remote) = patch::Remote::parse(entry)? else {
            continue;
         };
         let target = package.dir.join(&remote.file);
         let kept = !update && package.vendored.get(entry) == Some(&remote.file) && target.exists();
         if !kept {
            let response = http::cached(&remote.download, Duration::ZERO)?;
            if response.status != 200 {
               return Err(io::Error::other(format!(
                  "{}: {} answered {}",
                  package.name, remote.download, response.status
               )));
            }
            write(&target, response.body)?;
            say(&format!("{}: {} -> {}", package.name, entry, remote.file));
            fetched.push(target);
         }
         vendored.insert(entry.clone(), remote.file);
      }
      if vendored.is_empty() && package.vendored.is_empty() {
         continue;
      }
      // the source is read through the tree, which already wants the new
      // files named, and nothing stays vendored unless the whole list applies
      let before = read_to_string(&package.file)?;
      pipeline::set_vendored(&package, &vendored)?;
      if let Err(error) = check_patches(&package, &vendored) {
         write(&package.file, before)?;
         for file in &fetched {
            remove_file(file)?;
         }
         return Err(error);
      }
      for (entry, file) in &package.vendored {
         if !vendored.contains_key(entry) && !vendored.values().any(|kept| kept == file) {
            remove_file(package.dir.join(file))?;
         }
      }
   }
   Ok(())
}

/// Fails unless the package's patches apply, in order, to its source at the
/// current pin.
fn check_patches(package: &Package, vendored: &BTreeMap<String, String>) -> io::Result<()> {
   if package.patches.is_empty() {
      return Ok(());
   }
   let files = package
      .patches
      .iter()
      .map(|entry| package.dir.join(vendored.get(entry).unwrap_or(entry)))
      .collect::<Vec<_>>();
   match patch::check(&locks::source_of(package)?, &files)? {
      Ok(()) => Ok(()),
      Err(patch::Refusal::Contained(file)) => {
         Err(io::Error::other(format!(
            "{}: {} is already in the source, drop it from patches",
            package.name,
            file.display()
         )))
      },
      Err(patch::Refusal::Conflict(file)) => {
         Err(io::Error::other(format!(
            "{}: {} no longer applies",
            package.name,
            file.display()
         )))
      },
   }
}

fn init(dir: &PathBuf, purl: &str, url: &str, allow: Option<&str>) -> io::Result<()> {
   let file = dir.join("act.toml");
   if file.exists() {
      return Err(io::Error::other(format!("{} exists", file.display())));
   }
   create_dir_all(dir)?;
   let track = purl
      .strip_prefix("pkg:")
      .unwrap_or(purl)
      .replacen('/', ":", 1);
   let fetch = format!("fetch = {{ url = \"{url}\" }}\n");
   let text = allow.map_or_else(
      || format!("track = \"{track}\"\n{fetch}"),
      |range| format!("{fetch}\n[track]\nfrom  = \"{track}\"\nallow = \"{range}\"\n"),
   );
   write(&file, text)?;
   let name = utf8(Path::new(dir.file_name().unwrap_or_default()))?.to_owned();
   let plan = planned(&[name], false)?;
   let Some(entry) = plan.first() else {
      remove_file(&file)?;
      return Err(io::Error::other(format!("{purl}: not discovered")));
   };
   if entry.to.is_none() {
      remove_file(&file)?;
      return Err(io::Error::other(format!(
         "{purl}: {}",
         entry.note.as_deref().unwrap_or_default()
      )));
   }
   pipeline::apply(entry)?;
   say(&format!(
      "{}: {}",
      file.display(),
      entry.to.as_deref().unwrap_or_default()
   ));
   Ok(())
}

fn get(url: &str, max_age: Duration) -> io::Result<()> {
   let response = http::cached(url, max_age)?;
   let body = serde_json::from_str::<Value>(&response.body).unwrap_or(Value::String(response.body));
   print_json(&serde_json::json!({ "status": response.status, "body": body }))
}

fn run(mode: Mode) -> io::Result<i32> {
   match mode {
      Mode::List { names } => list(&names)?,
      Mode::Check {
         names,
         json,
         prerelease,
      } => return check(&names, json, prerelease),
      Mode::Apply {
         names,
         plan,
         prerelease,
         commit,
      } => apply(&names, plan.as_ref(), prerelease, commit)?,
      Mode::Verify { names } => verify(&names)?,
      Mode::Rehash { names } => rehash(&names)?,
      Mode::Lock { names, prune } => lock(&names, prune)?,
      Mode::Patch { names, update } => vendor_patches(&names, update)?,
      Mode::Init {
         dir,
         purl,
         url,
         allow,
      } => init(&dir, &purl, &url, allow.as_deref())?,
      Mode::Normalize { files } => {
         for file in &files {
            locks::normalize(file)?;
         }
      },
      Mode::Versions { purl } => {
         print_json(&datasource::versions(&purl)?)?;
      },
      Mode::Get { url, max_age } => get(&url, max_age)?,
   }
   Ok(0)
}

fn dispatch() -> Result<i32> {
   let arguments = args_os().collect::<Vec<_>>();
   let text = text_arguments(arguments.get(1..).unwrap_or_default())?;
   let mode = Mode::try_parse_from(text.iter().map(String::as_str))?;
   Ok(run(mode)?)
}

fn main() -> ExitCode {
   run_main(dispatch)
}
