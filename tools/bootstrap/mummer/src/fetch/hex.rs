use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
   iter::from_fn,
   path::Path,
};

use mummer::hash::{
   Algorithm,
   Digest,
   OutputHash,
};

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Place,
         Plan,
         Row,
         lock_dir,
         reject,
      },
   },
   nix_store::Store,
};

const REPO: &str = "https://repo.hex.pm/tarballs";

struct Package {
   name:    String,
   version: String,
   sha256:  OutputHash,
}

fn is_sha256(text: &str) -> bool {
   Digest::from_hex(Algorithm::Sha256, text).is_ok()
}

/// every string between `open` and `close` in `text`, in order.
fn delimited<'text>(
   text: &'text str,
   open: &'static str,
   close: &'static str,
) -> impl Iterator<Item = &'text str> {
   let mut rest = text;
   from_fn(move || {
      let body = &rest[rest.find(open)? + open.len()..];
      let end = body.find(close)?;
      rest = &body[end + close.len()..];
      Some(&body[..end])
   })
}

/// `"app": {:hex, :name, "version", "inner", [tools], [deps], "hexpm",
/// "outer"}`.
fn mix_lock(text: &str) -> io::Result<Vec<Package>> {
   let mut foreign = Vec::new();
   let mut unhashed = Vec::new();
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
      let strings = delimited(after, "\"", "\"").collect::<Vec<_>>();
      // a dependency's `repo: "hexpm"` comes first, the outer hash follows the
      // last one
      let outer = strings
         .windows(2)
         .rev()
         .find(|pair| matches!(**pair, [repo, hash] if repo == "hexpm" && is_sha256(hash)))
         .and_then(|pair| pair.get(1).copied());
      let (Some(version), Some(sha256)) = (strings.first(), outer) else {
         unhashed.push(app);
         continue;
      };
      packages.push(Package {
         name,
         version: (*version).to_owned(),
         sha256: OutputHash::hex(Algorithm::Sha256, sha256)?,
      });
   }
   reject("hexDeps: mix.lock has non-hex entries", &foreign)?;
   reject("hexDeps: mix.lock has no hexpm hash for", &unhashed)?;
   Ok(packages)
}

/// `{<<"name">>,{pkg,<<"name">>,<<"version">>},_}` in the first term,
/// `{<<"name">>, <<"SHA">>}` under `pkg_hash_ext`.
fn rebar_lock(text: &str) -> io::Result<Vec<Package>> {
   let foreign = text
      .match_indices("\">>,{")
      .filter_map(|(at, _found)| {
         let after = &text[at + 5..];
         let scm = after.starts_with("git") || after.starts_with("hg") || after.starts_with("path");
         scm.then(|| text[..at].rsplit("<<\"").next().unwrap_or_default())
      })
      .collect::<Vec<&str>>();
   reject("hexDeps: rebar.lock has non-hex entries", &foreign)?;
   let hashes = text
      .split_once("{pkg_hash_ext,[")
      .map(|(_head, tail)| tail.split("]}").next().unwrap_or_default())
      .map(|ext| {
         delimited(ext, "<<\"", "\">>")
            .collect::<Vec<_>>()
            .chunks(2)
            .filter_map(|pair| {
               match *pair {
                  [name, hash] if is_sha256(hash) => Some((name, hash)),
                  _ => None,
               }
            })
            .collect::<BTreeMap<&str, &str>>()
      })
      .unwrap_or_default();
   let mut unhashed = Vec::new();
   let mut packages = Vec::new();
   for (at, _found) in text.match_indices("{pkg,<<\"") {
      let mut fields = delimited(&text[at..], "<<\"", "\">>");
      let (Some(name), Some(version)) = (fields.next(), fields.next()) else {
         continue;
      };
      let Some(sha256) = hashes.get(name) else {
         unhashed.push(name);
         continue;
      };
      packages.push(Package {
         name:    name.to_owned(),
         version: version.to_owned(),
         sha256:  OutputHash::hex(Algorithm::Sha256, &sha256.to_lowercase())?,
      });
   }
   reject(
      "hexDeps: rebar.lock has no pkg_hash_ext entry for",
      &unhashed,
   )?;
   Ok(packages)
}

/// mix.lock or rebar.lock -> packages/hexpm/<name>-<version>.tar, Hex's cache.
pub struct Hex;

impl Producer for Hex {
   fn name(&self) -> &'static str {
      "hex"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let dir = lock_dir()?;
      let mix = format!("{dir}/mix.lock");
      let packages = if Path::new(&mix).exists() {
         mix_lock(&read_to_string(mix)?)?
      } else {
         rebar_lock(&read_to_string(format!("{dir}/rebar.lock"))?)?
      };
      let mut plan = Plan::new("hex-deps");
      for package in packages {
         let file = format!("{}-{}.tar", package.name, package.version);
         plan.fetch(
            Row {
               url:  format!("{REPO}/{file}"),
               file: file.clone(),
               hash: package.sha256,
            },
            Place::Link(format!("packages/hexpm/{file}")),
         );
      }
      plan.submit(store)
   }
}
