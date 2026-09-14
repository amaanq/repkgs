use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
   path::Path,
};

use serde_json::Map;

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Entry,
         Hash,
         Row,
         attr,
         collect,
         fetchurls,
      },
   },
   nix_store::Store,
};

const REPO: &str = "https://repo.hex.pm/tarballs";

struct Package {
   name:    String,
   version: String,
   sha256:  String,
}

fn is_sha256(text: &str) -> bool {
   text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// the `"…"` strings of one line, in order.
fn quoted(line: &str) -> Vec<&str> {
   let mut found = Vec::new();
   let mut rest = line;
   while let Some(start) = rest.find('"') {
      let body = &rest[start + 1..];
      let Some(end) = body.find('"') else {
         break;
      };
      found.push(&body[..end]);
      rest = &body[end + 1..];
   }
   found
}

/// `"app": {:hex, :name, "version", "inner", [tools], [deps], "hexpm",
/// "outer"}`.
fn mix_lock(text: &str) -> io::Result<Vec<Package>> {
   let mut foreign = Vec::new();
   let mut packages = Vec::new();
   for line in text.lines() {
      let Some((quoted_app, rest)) = line.trim().split_once("\": {:") else {
         continue;
      };
      let app = quoted_app.trim_start_matches('"');
      if rest.starts_with("git") || rest.starts_with("path") {
         foreign.push(app);
         continue;
      }
      let Some(after) = rest.strip_prefix("hex, :") else {
         continue;
      };
      let name = after.split(',').next().unwrap_or_default().to_owned();
      let strings = quoted(after);
      // a dependency's `repo: "hexpm"` comes first, the outer hash follows the
      // last one
      let outer = strings
         .windows(2)
         .rev()
         .find(|pair| matches!(**pair, [repo, hash] if repo == "hexpm" && is_sha256(hash)))
         .and_then(|pair| pair.get(1).copied());
      if let (Some(version), Some(sha256)) = (strings.first(), outer) {
         packages.push(Package {
            name,
            version: (*version).to_owned(),
            sha256: sha256.to_owned(),
         });
      }
   }
   if !foreign.is_empty() {
      return Err(io::Error::other(format!(
         "hexDeps: mix.lock has non-hex entries: {}",
         foreign.join(", ")
      )));
   }
   Ok(packages)
}

/// every `<<"…">>` binary of a text, in order.
fn binaries(text: &str) -> Vec<&str> {
   let mut found = Vec::new();
   let mut rest = text;
   while let Some(start) = rest.find("<<\"") {
      let body = &rest[start + 3..];
      let Some(end) = body.find("\">>") else {
         break;
      };
      found.push(&body[..end]);
      rest = &body[end + 3..];
   }
   found
}

/// `{<<"name">>,{pkg,<<"name">>,<<"version">>},_}` in the first term,
/// `{<<"name">>, <<"SHA">>}` under `pkg_hash_ext`.
fn rebar_lock(text: &str) -> io::Result<Vec<Package>> {
   let foreign: Vec<&str> = text
      .match_indices("\">>,{")
      .filter_map(|(at, _found)| {
         let after = &text[at + 5..];
         let scm = after.starts_with("git") || after.starts_with("hg") || after.starts_with("path");
         scm.then(|| text[..at].rsplit("<<\"").next().unwrap_or_default())
      })
      .collect();
   if !foreign.is_empty() {
      return Err(io::Error::other(format!(
         "hexDeps: rebar.lock has non-hex entries: {}",
         foreign.join(", ")
      )));
   }
   let hashes: BTreeMap<String, String> = text
      .split_once("{pkg_hash_ext,[")
      .map(|(_head, tail)| tail.split("]}").next().unwrap_or_default())
      .map(|ext| {
         let names = binaries(ext);
         names
            .chunks(2)
            .filter_map(|pair| {
               match *pair {
                  [name, hash] if is_sha256(hash) || is_sha256(&hash.to_lowercase()) => {
                     Some((name.to_owned(), hash.to_lowercase()))
                  },
                  _ => None,
               }
            })
            .collect()
      })
      .unwrap_or_default();
   let mut packages = Vec::new();
   for (at, _found) in text.match_indices("{pkg,<<\"") {
      let fields = binaries(&text[at..]);
      if let [name, version, ..] = fields[..]
         && let Some(sha256) = hashes.get(name)
      {
         packages.push(Package {
            name:    name.to_owned(),
            version: version.to_owned(),
            sha256:  sha256.clone(),
         });
      }
   }
   Ok(packages)
}

/// mix.lock or rebar.lock -> packages/hexpm/<name>-<version>.tar, Hex's cache.
pub struct Hex;

impl Producer for Hex {
   fn name(&self) -> &'static str {
      "hex"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let dir = format!("{}/{}", attr("source")?, attr("root")?);
      let mix = format!("{dir}/mix.lock");
      let packages = if Path::new(&mix).exists() {
         mix_lock(&read_to_string(mix)?)?
      } else {
         rebar_lock(&read_to_string(format!("{dir}/rebar.lock"))?)?
      };
      let rows: Vec<Row> = packages
         .iter()
         .map(|package| {
            let file = format!("{}-{}.tar", package.name, package.version);
            Row {
               url: format!("{REPO}/{file}"),
               file,
               hash: Hash::Sha256(package.sha256.clone()),
            }
         })
         .collect();
      let fetched = fetchurls(store, &rows)?;
      let layout: Vec<Entry> = rows
         .iter()
         .zip(&fetched)
         .map(|(row, made)| Entry::link(made.out.clone(), format!("packages/hexpm/{}", row.file)))
         .collect();
      let inputs: Vec<String> = fetched.into_iter().map(|made| made.drv).collect();
      collect(store, "hex-deps", &layout, &inputs, None, Map::new())
   }
}
