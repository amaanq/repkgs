use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
};

use serde::{
   Deserialize,
   Serialize,
};
use serde_json::{
   Map,
   Value,
};
use sha2::{
   Digest as _,
   Sha256,
};

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
         hex,
         json_file,
         lock_path,
         say,
         stage,
      },
      registry::{
         flat_name,
         split_id,
         tarball_url,
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
   tarball:   String,
   drv:       String,
}

#[derive(Deserialize, Serialize)]
struct JsrFetched {
   name:    String,
   version: String,
   url:     String,
   meta:    String,
   drv:     String,
}

#[derive(Deserialize, Serialize)]
struct HttpsFetched {
   url: String,
   src: String,
   drv: String,
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
      let mut ids: Vec<(String, String, String)> = Vec::new();
      for (key, entry) in &lock.npm {
         let id = npm_key_id(key);
         let (name, version) = split_id(id);
         if ids.iter().any(|seen| seen.0 == name && seen.1 == version) {
            continue;
         }
         ids.push((name.to_owned(), version.to_owned(), entry.integrity.clone()));
         rows.push(Row {
            file: format!("{}-{version}.tgz", flat_name(name)),
            url:  tarball_url(name, version, ""),
            hash: Hash::Sri(entry.integrity.clone()),
         });
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
         let (name, version) = split_id(key);
         ids.push((name.to_owned(), version.to_owned()));
         rows.push(Row {
            file: format!("jsr-{}-{version}_meta.json", flat_name(name)),
            url:  format!("{JSR}/{name}/{version}_meta.json"),
            hash: Hash::Sha256(entry.integrity.clone()),
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
            let (host, _path) = host_and_path(url);
            Row {
               file: format!("{host}{}", url_path(url)),
               url:  url.clone(),
               hash: Hash::Sha256(sha256.clone()),
            }
         })
         .collect();
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
      let inputs: Vec<String> = npm
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

fn host_and_path(url: &str) -> (&str, &str) {
   let rest = url.split_once("://").map_or(url, |(_scheme, rest)| rest);
   rest.split_once('/').unwrap_or((rest, ""))
}

/// where deno's global cache keeps a URL,
/// remote/<scheme>/<host>[_PORT<n>]/<sha256 of path?query>.
fn cache_path(url: &str) -> String {
   let (scheme, rest) = url.split_once("://").unwrap_or(("https", url));
   let (authority, path_query) = rest
      .find('/')
      .map_or((rest, ""), |slash| (&rest[..slash], &rest[slash..]));
   let (host, port) = authority
      .split_once(':')
      .map_or((authority, ""), |(host, port)| (host, port));
   let key = hex(&Sha256::digest(path_query.as_bytes()));
   let port_part = if port.is_empty() {
      String::new()
   } else {
      format!("_PORT{port}")
   };
   format!("remote/{scheme}/{host}{port_part}/{key}")
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
   Ok(meta
      .manifest
      .iter()
      .filter(|module| graph.contains_key(module.0))
      .map(|(path, entry)| {
         let url = format!("{JSR}/{}/{}{path}", package.name, package.version);
         let row = Row {
            file: format!("jsr-{}-{}{path}", package.name, package.version),
            url:  url.clone(),
            hash: Hash::Sha256(
               entry
                  .checksum
                  .strip_prefix("sha256-")
                  .unwrap_or(&entry.checksum)
                  .to_owned(),
            ),
         };
         (url, row)
      })
      .collect())
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
            made.out.clone(),
            trailer(url, content_type(url))?,
            cache_path(url),
         ));
      }
      for module in &fetched.https {
         layout.push(Entry::copy(
            module.src.clone(),
            trailer(&module.url, content_type(&module.url))?,
            cache_path(&module.url),
         ));
      }
      for package in &fetched.jsr {
         layout.push(Entry::copy(
            package.meta.clone(),
            trailer(&package.url, "application/json")?,
            cache_path(&package.url),
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
            cache_path(&url),
         ));
      }
      // npm/registry.npmjs.org/<name>/<version>/ plus a registry.json cut to
      // the locked versions ("full" and dist.tarball present, deno asks
      // the registry nothing more)
      let mut npm_by_name: BTreeMap<&str, Vec<&NpmFetched>> = BTreeMap::new();
      for package in &fetched.npm {
         layout.push(Entry::unpack(
            package.tarball.clone(),
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
      let inputs: Vec<String> = modules
         .into_iter()
         .map(|made| made.drv)
         .chain(fetched.npm.iter().map(|package| package.drv.clone()))
         .chain(fetched.jsr.iter().map(|package| package.drv.clone()))
         .chain(fetched.https.iter().map(|module| module.drv.clone()))
         .collect();
      collect(store, "deno-deps", &layout, &inputs, None, Map::new(), &[])
   }
}
