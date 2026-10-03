use std::{
   fs::read_to_string,
   io,
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
         lock_path,
         optional,
      },
   },
   nix_store::Store,
};

const RUBYGEMS: &str = "https://rubygems.org/gems";

struct Gem {
   name:     String,
   version:  String,
   platform: String,
   sha256:   String,
}

/// `  name (version[-platform]) sha256=<hex>` lines of the CHECKSUMS section,
/// the application's own PATH gem is listed there without a checksum and
/// dropped.
fn checksums(lock: &str) -> io::Result<Vec<Gem>> {
   let section = lock
      .split_once("\nCHECKSUMS\n")
      .map(|(_head, tail)| tail.split("\n\n").next().unwrap_or_default())
      .ok_or_else(|| {
         io::Error::other(
            "gems: Gemfile.lock has no CHECKSUMS section; `plumb lock <pkg>` adds one (bundle \
             lock --add-checksums)",
         )
      })?;
   let mut gems = Vec::new();
   for line in section.lines() {
      let Some(entry) = line.strip_prefix("  ") else {
         continue;
      };
      let Some((name, rest)) = entry.split_once(" (") else {
         continue;
      };
      let Some((spec, after)) = rest.split_once(')') else {
         continue;
      };
      let Some(sha256) = after.strip_prefix(" sha256=") else {
         continue;
      };
      if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
         continue;
      }
      let (version, platform) = spec.split_once('-').unwrap_or((spec, ""));
      gems.push(Gem {
         name:     name.to_owned(),
         version:  version.to_owned(),
         platform: platform.to_owned(),
         sha256:   sha256.to_owned(),
      });
   }
   Ok(gems)
}

/// Gemfile.lock's CHECKSUMS section -> { vendor/cache/*.gem, Gemfile.lock }
/// for `bundle install --local`.
pub struct Gems;

impl Producer for Gems {
   fn name(&self) -> &'static str {
      "gems"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let lock_file = match optional("lockFile") {
         Some(path) => path,
         None => lock_path("Gemfile.lock")?,
      };
      let lock = read_to_string(lock_file)?;
      let gem_platform = attr("gemPlatform")?;
      let gnu_platform = format!("{gem_platform}-gnu");
      // bundler installs the platform gem when both it and the ruby one are
      // cached, so ship both
      let ours: Vec<Gem> = checksums(&lock)?
         .into_iter()
         .filter(|gem| {
            gem.platform.is_empty() || gem.platform == gem_platform || gem.platform == gnu_platform
         })
         .collect();
      let rows: Vec<Row> = ours
         .iter()
         .map(|gem| {
            let suffix = if gem.platform.is_empty() {
               String::new()
            } else {
               format!("-{}", gem.platform)
            };
            let file = format!("{}-{}{suffix}.gem", gem.name, gem.version);
            Row {
               url: format!("{RUBYGEMS}/{file}"),
               file,
               hash: Hash::Sha256(gem.sha256.clone()),
            }
         })
         .collect();
      let fetched = fetchurls(store, &rows)?;
      let mut layout: Vec<Entry> = rows
         .iter()
         .zip(&fetched)
         .map(|(row, made)| Entry::link(made.out.clone(), format!("vendor/cache/{}", row.file)))
         .collect();
      layout.push(Entry::write(lock, "Gemfile.lock".to_owned()));
      let inputs: Vec<String> = fetched.into_iter().map(|made| made.drv).collect();
      collect(store, "gems", &layout, &inputs, None, Map::new(), &[])
   }
}
