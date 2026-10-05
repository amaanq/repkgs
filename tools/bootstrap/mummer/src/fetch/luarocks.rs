use std::{
   collections::BTreeMap,
   fmt::Write as _,
   io,
};

use mummer::hash::{
   Digest,
   OutputHash,
};
use serde::Deserialize;

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Entry,
         Place,
         Plan,
         Row,
         attr,
         read_toml,
         say,
      },
   },
   nix_store::Store,
};

const LUAROCKS: &str = "https://luarocks.org";

#[derive(Deserialize)]
struct Locks {
   luarocks: BTreeMap<String, Locked>,
}

#[derive(Deserialize)]
struct Locked {
   version: String,
   sha256:  Digest,
}

/// locks/luarocks.toml -> a rocks server directory, .src.rock files plus the
/// manifest-<lua major.minor> naming exactly those.
pub struct Luarocks;

impl Producer for Luarocks {
   fn name(&self) -> &'static str {
      "luarocks"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let locks = read_toml::<Locks, _>(attr("locks")?)?;
      let mut plan = Plan::new("luarocks-set");
      let mut manifest = String::from("commands = {}\nmodules = {}\nrepository = {\n");
      for (name, entry) in &locks.luarocks {
         let file = format!("{name}-{}.src.rock", entry.version);
         plan.fetch(
            Row {
               url:  format!("{LUAROCKS}/{file}"),
               file: file.clone(),
               hash: OutputHash::sri(entry.sha256.clone()),
            },
            Place::Link(file),
         );
         writeln!(
            manifest,
            "   [\"{name}\"] = {{ [\"{}\"] = {{ {{ arch = \"src\" }} }} }},",
            entry.version
         )
         .map_err(io::Error::other)?;
      }
      manifest.push('}');
      say(&format!("luaRocksSet: {} rocks", locks.luarocks.len()));
      let lua_version = attr("luaVersion")?;
      let major_minor = lua_version.split('.').take(2).collect::<Vec<&str>>();
      plan.add(Entry::write(
         manifest,
         format!("manifest-{}", major_minor.join(".")),
      ));
      plan.submit(store)
   }
}
