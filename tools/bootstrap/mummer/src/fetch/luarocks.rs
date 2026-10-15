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
         Member,
         Row,
         attr,
         collect,
         fetchurls,
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

/// A rock luarocks.org serves as a .src.rock, `sha256` that file's, or with
/// `rockspec` set one it serves only as a rockspec, `sha256` then the hash of
/// the source archive at `url` the rockspec names.
#[derive(Deserialize)]
struct Locked {
   version:  String,
   sha256:   Digest,
   rockspec: Option<Digest>,
   url:      Option<String>,
}

enum Served {
   SrcRock,
   Rockspec { archive: String },
}

/// locks/luarocks.toml -> a rocks server directory, .src.rock files, those
/// of rockspec-only rocks packed the way `luarocks pack` does, plus the
/// manifest-<lua major.minor> naming exactly those.
pub struct Luarocks;

impl Producer for Luarocks {
   fn name(&self) -> &'static str {
      "luarocks"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let locks = read_toml::<Locks, _>(attr("locks")?)?;
      let mut rows = Vec::new();
      let mut served = Vec::new();
      let mut manifest = String::from("commands = {}\nmodules = {}\nrepository = {\n");
      for (name, entry) in &locks.luarocks {
         let stem = format!("{name}-{}", entry.version);
         match (entry.rockspec.as_ref(), entry.url.as_ref()) {
            (None, None) => {
               rows.push(Row {
                  url:  format!("{LUAROCKS}/{stem}.src.rock"),
                  file: format!("{stem}.src.rock"),
                  hash: OutputHash::sri(entry.sha256.clone()),
               });
               served.push((stem, Served::SrcRock));
            },
            (Some(rockspec), Some(url)) => {
               let archive = url
                  .rsplit('/')
                  .next()
                  .filter(|base| !base.is_empty())
                  .ok_or_else(|| io::Error::other(format!("luaRocksSet: {name} url {url}")))?
                  .to_owned();
               rows.push(Row {
                  url:  format!("{LUAROCKS}/{stem}.rockspec"),
                  file: format!("{stem}.rockspec"),
                  hash: OutputHash::sri(rockspec.clone()),
               });
               rows.push(Row {
                  url:  url.clone(),
                  file: archive.clone(),
                  hash: OutputHash::sri(entry.sha256.clone()),
               });
               served.push((stem, Served::Rockspec { archive }));
            },
            _ => {
               return Err(io::Error::other(format!(
                  "luaRocksSet: {name} needs both rockspec and url, or neither"
               )));
            },
         }
         writeln!(
            manifest,
            "   [\"{name}\"] = {{ [\"{}\"] = {{ {{ arch = \"src\" }} }} }},",
            entry.version
         )
         .map_err(io::Error::other)?;
      }
      manifest.push('}');
      let fetched = fetchurls(store, &rows)?;
      let mut made = fetched.iter();
      let mut layout = Vec::with_capacity(served.len() + 1);
      for (stem, how) in served {
         let first = made
            .next()
            .ok_or_else(|| io::Error::other("luaRocksSet: fewer fetches than rows"))?;
         match how {
            Served::SrcRock => {
               layout.push(Entry::link(
                  first.out.to_string(),
                  format!("{stem}.src.rock"),
               ));
            },
            Served::Rockspec { archive } => {
               let source = made
                  .next()
                  .ok_or_else(|| io::Error::other("luaRocksSet: fewer fetches than rows"))?;
               layout.push(Entry::zip(
                  vec![
                     Member {
                        name: format!("{stem}.rockspec"),
                        path: first.out.to_string(),
                     },
                     Member {
                        name: archive,
                        path: source.out.to_string(),
                     },
                  ],
                  format!("{stem}.src.rock"),
               ));
            },
         }
      }
      say(&format!("luaRocksSet: {} rocks", locks.luarocks.len()));
      let lua_version = attr("luaVersion")?;
      let major_minor = lua_version.split('.').take(2).collect::<Vec<&str>>();
      layout.push(Entry::write(
         manifest,
         format!("manifest-{}", major_minor.join(".")),
      ));
      let inputs = fetched
         .into_iter()
         .map(|realised| realised.drv)
         .collect::<Vec<_>>();
      collect(store, "luarocks-set", &layout, &inputs)
   }
}
