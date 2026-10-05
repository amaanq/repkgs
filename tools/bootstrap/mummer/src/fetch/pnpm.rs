use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   fs::read_to_string,
   io,
   iter::Peekable,
};

use mummer::hash::OutputHash;

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Place,
         Plan,
         Row,
         lock_path,
         reject,
      },
      registry::{
         PackageId,
         url_path,
      },
   },
   nix_store::Store,
};

#[derive(Default)]
struct Resolution {
   integrity: Option<String>,
   tarball:   Option<String>,
}

fn unquote(value: &str) -> String {
   let trimmed = value.trim();
   if let Some(inner) = trimmed
      .strip_prefix('\'')
      .and_then(|rest| rest.strip_suffix('\''))
   {
      return inner.replace("''", "'");
   }
   if let Some(inner) = trimmed
      .strip_prefix('"')
      .and_then(|rest| rest.strip_suffix('"'))
   {
      return inner.to_owned();
   }
   trimmed.to_owned()
}

/// `{integrity: sha512-…, tarball: https://…}`, the flow mapping pnpm writes
/// for a resolution.
fn flow_pairs(text: &str) -> io::Result<Vec<(String, String)>> {
   let inner = text
      .trim()
      .strip_prefix('{')
      .and_then(|rest| rest.strip_suffix('}'))
      .ok_or_else(|| io::Error::other(format!("pnpmDeps: unexpected resolution {text}")))?;
   let mut pairs = Vec::new();
   let mut rest = inner.trim();
   while !rest.is_empty() {
      let (key, after_key) = rest
         .split_once(':')
         .ok_or_else(|| io::Error::other(format!("pnpmDeps: unexpected resolution {text}")))?;
      let value_text = after_key.trim_start();
      let (value, remaining) = if let Some(quote) = value_text
         .chars()
         .next()
         .filter(|first| *first == '\'' || *first == '"')
      {
         let body = &value_text[1..];
         let end = body
            .find(quote)
            .ok_or_else(|| io::Error::other(format!("pnpmDeps: unterminated quote in {text}")))?;
         (
            unquote(&value_text[..end + 2]),
            body[end + 1..]
               .trim_start()
               .trim_start_matches(',')
               .trim_start(),
         )
      } else {
         match value_text.split_once(',') {
            Some((value, remaining)) => (value.trim().to_owned(), remaining.trim_start()),
            None => (value_text.trim().to_owned(), ""),
         }
      };
      pairs.push((key.trim().to_owned(), value));
      rest = remaining;
   }
   Ok(pairs)
}

/// The six-space `key: value` lines of a block mapping pnpm writes for a
/// resolution.
fn block_pairs<'text>(
   lines: &mut Peekable<impl Iterator<Item = &'text str>>,
) -> io::Result<Vec<(String, String)>> {
   let mut block = Vec::new();
   while let Some(next) = lines.next_if(|next| indent(next) == 6) {
      let (key, field) = next
         .trim()
         .split_once(':')
         .ok_or_else(|| io::Error::other(format!("pnpmDeps: unexpected line {next}")))?;
      block.push((key.to_owned(), unquote(field)));
   }
   Ok(block)
}

impl From<Vec<(String, String)>> for Resolution {
   fn from(pairs: Vec<(String, String)>) -> Self {
      let mut resolution = Self::default();
      for (key, field) in pairs {
         match key.as_str() {
            "integrity" => resolution.integrity = Some(field),
            "tarball" => resolution.tarball = Some(field),
            _ => {},
         }
      }
      resolution
   }
}

fn indent(line: &str) -> usize {
   line.len() - line.trim_start_matches(' ').len()
}

/// the `packages:` section of a v9 lock, id -> resolution. The file is
/// machine written, so the shape is fixed, two-space block mappings with the
/// resolution as a flow mapping.
fn packages(text: &str) -> io::Result<BTreeMap<String, Resolution>> {
   let mut found = BTreeMap::new();
   let mut lines = text.lines().peekable();
   while let Some(line) = lines.next() {
      if line != "packages:" {
         continue;
      }
      let mut current = None::<String>;
      while let Some(entry_line) = lines.peek().copied() {
         if !entry_line.is_empty() && indent(entry_line) == 0 {
            break;
         }
         lines.next();
         if entry_line.trim().is_empty() {
            continue;
         }
         let depth = indent(entry_line);
         let body = entry_line.trim();
         if depth == 2 {
            let key = body.strip_suffix(':').ok_or_else(|| {
               io::Error::other(format!("pnpmDeps: unexpected package line {entry_line}"))
            })?;
            let id = unquote(key);
            found.insert(id.clone(), Resolution::default());
            current = Some(id);
         } else if depth == 4
            && let Some(id) = current.as_deref()
            && let Some(value) = body.strip_prefix("resolution:")
         {
            let pairs = if value.trim().is_empty() {
               block_pairs(&mut lines)?
            } else {
               flow_pairs(value)?
            };
            found.insert(id.to_owned(), Resolution::from(pairs));
         }
      }
   }
   Ok(found)
}

/// pnpm-lock.yaml (v9) -> registry/<url path>, a file:// registry mirror
/// pnpm installs from offline.
pub struct Pnpm;

impl Producer for Pnpm {
   fn name(&self) -> &'static str {
      "pnpm"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let text = read_to_string(lock_path("pnpm-lock.yaml")?)?;
      let lock_version = text
         .lines()
         .find_map(|line| line.strip_prefix("lockfileVersion:"))
         .map_or_else(|| "0".to_owned(), unquote);
      if !lock_version.starts_with("9.") {
         return Err(io::Error::other(format!(
            "pnpmDeps: pnpm-lock.yaml version {lock_version}, only 9.x is supported (pnpm >= 9)"
         )));
      }
      let mut foreign = Vec::new();
      let mut plan = Plan::new("pnpm-deps");
      let mut seen = BTreeSet::new();
      let found = packages(&text)?;
      for (id, resolution) in &found {
         let Some(ref integrity) = resolution.integrity else {
            foreign.push(id.as_str());
            continue;
         };
         let url = match resolution.tarball {
            Some(ref tarball) => tarball.clone(),
            None => PackageId::try_from(id.as_str())?.tarball_url(None),
         };
         let file = url_path(&url).trim_start_matches('/').to_owned();
         if !seen.insert(file.clone()) {
            continue;
         }
         let to = format!("registry/{file}");
         plan.fetch(
            Row {
               file,
               url,
               hash: OutputHash::integrity(integrity)?,
            },
            Place::Link(to),
         );
      }
      reject(
         "pnpmDeps: no integrity (git/file/directory resolution) for",
         &foreign,
      )?;
      plan.submit(store)
   }
}
