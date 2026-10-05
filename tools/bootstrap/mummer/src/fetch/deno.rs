use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   fs::read_to_string,
   io,
};

use mummer::{
   hash::{
      Algorithm,
      Digest,
      OutputHash,
   },
   store_path::{
      DrvPath,
      StorePath,
   },
};
use serde::{
   Deserialize,
   Serialize,
};
use serde_json::{
   Map,
   Value,
};

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Entry,
         Row,
         attr,
         collect,
         fetchurls,
         json_file,
         lock_path,
         say,
         stage,
      },
      registry::{
         PackageId,
         Url,
         url_path,
      },
   },
   nix_store::Store,
};

const JSR: &str = "https://jsr.io";

#[derive(Deserialize)]
struct Lock {
   #[serde(default)]
   version: String,
   #[serde(default)]
   npm:     BTreeMap<String, NpmEntry>,
   #[serde(default)]
   jsr:     BTreeMap<String, JsrEntry>,
   #[serde(default)]
   remote:  BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct NpmEntry {
   integrity: String,
}

#[derive(Deserialize)]
struct JsrEntry {
   integrity: String,
}

/// what the first stage hands the second, `stage_attrs`.
#[derive(Deserialize, Serialize)]
struct Staged {
   npm:   Vec<NpmFetched>,
   jsr:   Vec<JsrFetched>,
   https: Vec<HttpsFetched>,
}

#[derive(Deserialize, Serialize)]
struct NpmFetched {
   name:      String,
   version:   String,
   url:       String,
   integrity: String,
   tarball:   StorePath,
   drv:       DrvPath,
}

#[derive(Deserialize, Serialize)]
struct JsrFetched {
   name:    String,
   version: String,
   url:     String,
   meta:    StorePath,
   drv:     DrvPath,
}

#[derive(Deserialize, Serialize)]
struct HttpsFetched {
   url: String,
   src: StorePath,
   drv: DrvPath,
}

/// deno.lock (version 4 or 5) pins npm tarballs by sha512, jsr packages by
/// the sha256 of their _meta.json (whose manifest pins every file) and https
/// modules by sha256. This stage fetches those, `deno-files` runs once they
/// exist.
pub struct Deno;

fn npm_key_id(key: &str) -> &str {
   let rest = key.strip_prefix('@').unwrap_or(key);
   let Some(version_at) = rest.find('@') else {
      return key;
   };
   let version_start = version_at + usize::from(key.starts_with('@'));
   let after_version = &key[version_start + 1..];
   let Some(underscore) = after_version.find('_') else {
      return key;
   };
   &key[..version_start + 1 + underscore]
}

impl Deno {
   /// npm keys may carry a peer-dependency suffix,
   /// "vite@5.0.0_@types+node@20.0.0".
   fn npm(store: &mut Store, lock: &Lock) -> io::Result<Vec<NpmFetched>> {
      let mut rows = Vec::new();
      let mut ids = Vec::new();
      let mut seen = BTreeSet::new();
      for (key, entry) in &lock.npm {
         let id = PackageId::try_from(npm_key_id(key))?;
         if !seen.insert((id.name, id.version)) {
            continue;
         }
         rows.push(Row {
            file: format!("{}-{}.tgz", id.flat_name(), id.version),
            url:  id.tarball_url(None),
            hash: OutputHash::integrity(&entry.integrity)?,
         });
         ids.push((
            id.name.to_owned(),
            id.version.to_owned(),
            entry.integrity.clone(),
         ));
      }
      let fetched = fetchurls(store, &rows)?;
      Ok(ids
         .into_iter()
         .zip(&rows)
         .zip(fetched)
         .map(|(((name, version, integrity), row), made)| {
            NpmFetched {
               name,
               version,
               url: row.url.clone(),
               integrity,
               tarball: made.out,
               drv: made.drv,
            }
         })
         .collect())
   }

   fn jsr(store: &mut Store, lock: &Lock) -> io::Result<Vec<JsrFetched>> {
      let mut rows = Vec::new();
      let mut ids: Vec<(String, String)> = Vec::new();
      for (key, entry) in &lock.jsr {
         let id = PackageId::try_from(key.as_str())?;
         let (name, version) = (id.name, id.version);
         ids.push((name.to_owned(), version.to_owned()));
         rows.push(Row {
            file: format!("jsr-{}-{version}_meta.json", id.flat_name()),
            url:  format!("{JSR}/{name}/{version}_meta.json"),
            hash: OutputHash::hex(Algorithm::Sha256, &entry.integrity)?,
         });
      }
      let fetched = fetchurls(store, &rows)?;
      Ok(ids
         .into_iter()
         .zip(&rows)
         .zip(fetched)
         .map(|(((name, version), row), made)| {
            JsrFetched {
               name,
               version,
               url: row.url.clone(),
               meta: made.out,
               drv: made.drv,
            }
         })
         .collect())
   }

   fn https(store: &mut Store, lock: &Lock) -> io::Result<Vec<HttpsFetched>> {
      let rows: Vec<Row> = lock
         .remote
         .iter()
         .map(|(url, sha256)| {
            let parts = Url::try_from(url.as_str())?;
            Ok(Row {
               file: format!("{}{}", parts.authority, parts.path),
               url:  url.clone(),
               hash: OutputHash::hex(Algorithm::Sha256, sha256)?,
            })
         })
         .collect::<io::Result<Vec<_>>>()?;
      let fetched = fetchurls(store, &rows)?;
      Ok(rows
         .iter()
         .zip(fetched)
         .map(|(row, made)| {
            HttpsFetched {
               url: row.url.clone(),
               src: made.out,
               drv: made.drv,
            }
         })
         .collect())
   }
}

impl Producer for Deno {
   fn name(&self) -> &'static str {
      "deno"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let lock: Lock = serde_json::from_str(&read_to_string(lock_path("deno.lock")?)?)?;
      let lock_version: u64 = if lock.version.is_empty() {
         3
      } else {
         lock
            .version
            .parse()
            .map_err(|_bad| io::Error::other("denoDeps: deno.lock version is not a number"))?
      };
      if lock_version < 4 {
         return Err(io::Error::other(
            "denoDeps: deno.lock older than version 4, regenerate it with deno >= 2",
         ));
      }
      let npm = Self::npm(store, &lock)?;
      let jsr = Self::jsr(store, &lock)?;
      let https = Self::https(store, &lock)?;
      say(&format!(
         "denoDeps: {} npm, {} jsr, {} https",
         npm.len(),
         jsr.len(),
         https.len()
      ));
      let inputs: Vec<DrvPath> = npm
         .iter()
         .map(|entry| entry.drv.clone())
         .chain(jsr.iter().map(|entry| entry.drv.clone()))
         .chain(https.iter().map(|entry| entry.drv.clone()))
         .collect();
      stage(
         store,
         "deno-deps",
         "deno-files",
         &Staged { npm, jsr, https },
         &inputs,
         &[],
      )
   }
}

/// where deno's global cache keeps a URL,
/// remote/<scheme>/<host>[_PORT<n>]/<sha256 of path?query>.
fn cache_path(url: &str) -> io::Result<String> {
   let Url {
      scheme,
      authority,
      path: path_query,
   } = Url::try_from(url)?;
   let (host, port) = authority
      .split_once(':')
      .map_or((authority, ""), |(host, port)| (host, port));
   let key = Digest::sha256(path_query.as_bytes()).hex();
   let port_part = if port.is_empty() {
      String::new()
   } else {
      format!("_PORT{port}")
   };
   Ok(format!("remote/{scheme}/{host}{port_part}/{key}"))
}

#[derive(Serialize)]
struct Headers<'meta> {
   #[serde(rename = "content-type")]
   content_type: &'meta str,
}

#[derive(Serialize)]
struct Metadata<'meta> {
   headers: Headers<'meta>,
   url:     &'meta str,
}

/// what deno appends to a cached body, the response headers it cares about
/// and the URL.
fn trailer(url: &str, content_type: &str) -> io::Result<String> {
   let metadata = serde_json::to_string(&Metadata {
      headers: Headers { content_type },
      url,
   })?;
   Ok(format!("\n// denoCacheMetadata={metadata}"))
}

/// the lock records no content-type, deno picks the loader by it, so derive
/// it from the extension.
fn content_type(url: &str) -> &'static str {
   let path = url_path(url);
   let extension = path
      .rsplit('/')
      .next()
      .and_then(|file| file.rsplit_once('.'))
      .map_or("", |(_stem, extension)| extension);
   match extension {
      "js" | "mjs" | "cjs" => "text/javascript",
      "jsx" => "text/jsx",
      "tsx" => "text/tsx",
      "json" => "application/json",
      "wasm" => "application/wasm",
      _ => "text/typescript",
   }
}

#[derive(Deserialize)]
struct Meta {
   manifest:      BTreeMap<String, ManifestEntry>,
   #[serde(default, rename = "moduleGraph2")]
   module_graph2: Option<BTreeMap<String, Value>>,
   #[serde(default, rename = "moduleGraph1")]
   module_graph1: Option<BTreeMap<String, Value>>,
}

#[derive(Deserialize)]
struct ManifestEntry {
   checksum: String,
}

/// the loadable modules of one jsr package version as rows plus their URLs.
/// jsr's publish-time module graph names them, test data, docs and CI files in
/// the manifest are not fetched.
fn jsr_modules(package: &JsrFetched) -> io::Result<Vec<(String, Row)>> {
   let meta: Meta = serde_json::from_str(&read_to_string(&package.meta)?)?;
   let graph = meta
      .module_graph2
      .or(meta.module_graph1)
      .unwrap_or_default();
   meta
      .manifest
      .iter()
      .filter(|module| graph.contains_key(module.0))
      .map(|(path, entry)| {
         let url = format!("{JSR}/{}/{}{path}", package.name, package.version);
         let row = Row {
            file: format!("jsr-{}-{}{path}", package.name, package.version),
            url:  url.clone(),
            hash: OutputHash::hex(
               Algorithm::Sha256,
               entry
                  .checksum
                  .strip_prefix("sha256-")
                  .unwrap_or(&entry.checksum),
            )?,
         };
         Ok((url, row))
      })
      .collect::<io::Result<_>>()
}

#[derive(Serialize)]
struct Dist<'entry> {
   tarball:   &'entry str,
   integrity: &'entry str,
}

#[derive(Serialize)]
struct Listed<'entry> {
   version:      &'entry str,
   dependencies: Map<String, Value>,
   dist:         Dist<'entry>,
}

#[derive(Serialize)]
struct Packument<'entry> {
   name:      &'entry str,
   #[serde(rename = "dist-tags")]
   dist_tags: Map<String, Value>,
   #[serde(rename = "_deno.packumentFormat")]
   format:    &'static str,
   versions:  BTreeMap<&'entry str, Listed<'entry>>,
}

#[derive(Serialize)]
struct Versions<'entry> {
   versions: BTreeMap<&'entry str, Map<String, Value>>,
}

/// second stage of `deno`, reads the fetched jsr _meta.json files, each module
/// they list becomes a builtin:fetchurl fixed by the manifest's sha256, and
/// the collecting derivation lays out what deno reads under `$DENO_DIR`.
pub struct DenoFiles;

impl Producer for DenoFiles {
   fn name(&self) -> &'static str {
      "deno-files"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let fetched: Staged = serde_json::from_str(&read_to_string(attr("stage_attrs")?)?)?;
      let mut module_urls = Vec::new();
      let mut module_rows = Vec::new();
      for package in &fetched.jsr {
         for (url, row) in jsr_modules(package)? {
            module_urls.push(url);
            module_rows.push(row);
         }
      }
      let modules = fetchurls(store, &module_rows)?;
      say(&format!("denoDeps: {} jsr modules", modules.len()));

      let mut layout = Vec::new();
      for (url, made) in module_urls.iter().zip(&modules) {
         layout.push(Entry::copy(
            made.out.to_string(),
            trailer(url, content_type(url))?,
            cache_path(url)?,
         ));
      }
      for module in &fetched.https {
         layout.push(Entry::copy(
            module.src.to_string(),
            trailer(&module.url, content_type(&module.url))?,
            cache_path(&module.url)?,
         ));
      }
      for package in &fetched.jsr {
         layout.push(Entry::copy(
            package.meta.to_string(),
            trailer(&package.url, "application/json")?,
            cache_path(&package.url)?,
         ));
      }
      let mut jsr_by_name: BTreeMap<&str, Vec<&JsrFetched>> = BTreeMap::new();
      for package in &fetched.jsr {
         jsr_by_name.entry(&package.name).or_default().push(package);
      }
      for (name, versions) in &jsr_by_name {
         let url = format!("{JSR}/{name}/meta.json");
         let listing = Versions {
            versions: versions
               .iter()
               .map(|package| (package.version.as_str(), Map::new()))
               .collect(),
         };
         layout.push(Entry::write(
            format!(
               "{}{}",
               serde_json::to_string(&listing)?,
               trailer(&url, "application/json")?
            ),
            cache_path(&url)?,
         ));
      }
      // npm/registry.npmjs.org/<name>/<version>/ plus a registry.json cut to
      // the locked versions ("full" and dist.tarball present, deno asks
      // the registry nothing more)
      let mut npm_by_name: BTreeMap<&str, Vec<&NpmFetched>> = BTreeMap::new();
      for package in &fetched.npm {
         layout.push(Entry::unpack(
            package.tarball.to_string(),
            format!(
               "npm/registry.npmjs.org/{}/{}",
               package.name, package.version
            ),
         ));
         npm_by_name.entry(&package.name).or_default().push(package);
      }
      for (name, versions) in &npm_by_name {
         let packument = Packument {
            name,
            dist_tags: Map::new(),
            format: "full",
            versions: versions
               .iter()
               .map(|package| {
                  (package.version.as_str(), Listed {
                     version:      &package.version,
                     dependencies: Map::new(),
                     dist:         Dist {
                        tarball:   &package.url,
                        integrity: &package.integrity,
                     },
                  })
               })
               .collect(),
         };
         layout.push(json_file(
            &format!("npm/registry.npmjs.org/{name}/registry.json"),
            &packument,
         )?);
      }
      // the first stage's files are laid out too, so they are inputs of the
      // collecting derivation as well
      let inputs: Vec<DrvPath> = modules
         .into_iter()
         .map(|made| made.drv)
         .chain(fetched.npm.iter().map(|package| package.drv.clone()))
         .chain(fetched.jsr.iter().map(|package| package.drv.clone()))
         .chain(fetched.https.iter().map(|module| module.drv.clone()))
         .collect();
      collect(store, "deno-deps", &layout, &inputs)
   }
}
