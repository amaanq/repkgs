use std::{
   fs::read_to_string,
   io,
};

use mummer::hash::{
   Algorithm,
   OutputHash,
};

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Entry,
         Place,
         Plan,
         Row,
         attr,
         lock_path,
      },
   },
   nix_store::Store,
};

const RUBYGEMS: &str = "https://rubygems.org/gems";

struct Gem {
   name:     String,
   version:  String,
   platform: Option<String>,
   sha256:   OutputHash,
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
            "gems: Gemfile.lock has no CHECKSUMS section; `spieler lock <pkg>` adds one (bundle \
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
      let (version, platform) = spec
         .split_once('-')
         .map_or((spec, None), |(version, platform)| {
            (version, Some(platform))
         });
      gems.push(Gem {
         name:     name.to_owned(),
         version:  version.to_owned(),
         platform: platform.map(str::to_owned),
         sha256:   OutputHash::hex(Algorithm::Sha256, sha256)
            .map_err(|error| io::Error::other(format!("gems: {name} ({spec}), {error}")))?,
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
      let lock = read_to_string(lock_path("Gemfile.lock")?)?;
      let gem_platform = attr("gemPlatform")?;
      let gnu_platform = format!("{gem_platform}-gnu");
      // bundler installs the platform gem when both it and the ruby one are
      // cached, so ship both
      let mut plan = Plan::new("gems");
      for gem in checksums(&lock)? {
         let suffix = match gem.platform {
            None => String::new(),
            Some(ref platform) if *platform == gem_platform || *platform == gnu_platform => {
               format!("-{platform}")
            },
            Some(_) => continue,
         };
         let file = format!("{}-{}{suffix}.gem", gem.name, gem.version);
         plan.fetch(
            Row {
               url:  format!("{RUBYGEMS}/{file}"),
               file: file.clone(),
               hash: gem.sha256,
            },
            Place::Link(format!("vendor/cache/{file}")),
         );
      }
      plan.add(Entry::write(lock, "Gemfile.lock".to_owned()));
      plan.submit(store)
   }
}
