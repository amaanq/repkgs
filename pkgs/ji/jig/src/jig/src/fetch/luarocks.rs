use std::{
   collections::BTreeMap,
   fmt::Write as _,
   fs::read_to_string,
   io,
};

use serde::Deserialize;
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
   sha256:  String,
}

/// locks/luarocks.toml -> a rocks server directory, .src.rock files plus the
/// manifest-<lua major.minor> naming exactly those.
pub struct Luarocks;

impl Producer for Luarocks {
   fn name(&self) -> &'static str {
      "luarocks"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let locks: Locks =
         toml::from_str(&read_to_string(attr("locks")?)?).map_err(io::Error::other)?;
      let rows: Vec<Row> = locks
         .luarocks
         .iter()
         .map(|(name, entry)| {
            let file = format!("{name}-{}.src.rock", entry.version);
            Row {
               url: format!("{LUAROCKS}/{file}"),
               file,
               hash: Hash::Sri(entry.sha256.clone()),
            }
         })
         .collect();
      let fetched = fetchurls(store, &rows)?;
      let mut manifest = String::from("commands = {}\nmodules = {}\nrepository = {\n");
      for (name, entry) in &locks.luarocks {
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
      let major_minor: Vec<&str> = lua_version.split('.').take(2).collect();
      let mut layout: Vec<Entry> = rows
         .iter()
         .zip(&fetched)
         .map(|(row, made)| Entry::link(made.out.clone(), row.file.clone()))
         .collect();
      layout.push(Entry::write(
         manifest,
         format!("manifest-{}", major_minor.join(".")),
      ));
      let inputs: Vec<String> = fetched.into_iter().map(|made| made.drv).collect();
      collect(store, "luarocks-set", &layout, &inputs, None, Map::new())
   }
}
