use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   fs::{
      read_dir,
      read_to_string,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
};

use mummer::{
   cargo_git::{
      GIT_SOURCES,
      GitSource,
   },
   files::utf8,
   hash::{
      Algorithm,
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
use toml::{
   Table,
   Value,
};

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Assembly,
         Entry,
         NoAttrs,
         Row,
         attr,
         collect_with,
         fetchurls,
         json_file,
         reject,
      },
   },
   nix_store::Store,
};

const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

#[derive(Deserialize)]
struct Lock {
   package: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
   name:     String,
   version:  String,
   source:   Option<String>,
   checksum: Option<String>,
}

#[derive(Serialize)]
struct Checksum<'package> {
   files:   BTreeMap<String, String>,
   package: Option<&'package str>,
}

/// Vendors the crates of every Cargo.lock in `locks`, relative to the source or
/// absolute, into a directory cargo accepts as [source.vendored-sources], each
/// with its .cargo-checksum.json. `git` names the unpacked tree of each git
/// source, one `<lock source>=<path>` a line.
pub struct Cargo;

impl Producer for Cargo {
   fn name(&self) -> &'static str {
      "cargo"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let source = attr("source")?;
      let locks = attr("locks")?
         .lines()
         .map(|lock| read_to_string(Path::new(&source).join(lock)))
         .collect::<io::Result<Vec<String>>>()?;
      let git = attr("git")?
         .lines()
         .filter(|line| !line.is_empty())
         .map(|line| {
            let (origin, tree) = line
               .rsplit_once('=')
               .ok_or_else(|| io::Error::other("cargoVendor: git input must be source=path"))?;
            Ok((origin.parse()?, tree.parse()?))
         })
         .collect::<io::Result<BTreeMap<GitSource, StorePath>>>()?;
      let vendor = vendor_layout(store, &locks, "", &git)?;

      collect_with(
         store,
         "cargo-vendor",
         &vendor.layout,
         &vendor.drvs,
         Assembly {
            script: "assemble",
            attrs:  NoAttrs,
            srcs:   &vendor.srcs,
         },
      )
   }
}

pub struct Vendor {
   pub layout: Vec<Entry>,
   pub drvs:   Vec<DrvPath>,
   /// The git trees the layout links into.
   pub srcs:   Vec<StorePath>,
}

pub fn vendor_layout(
   store: &mut Store,
   locks: &[String],
   prefix: &str,
   git: &BTreeMap<GitSource, StorePath>,
) -> io::Result<Vendor> {
   let mut listed = Vec::new();

   for text in locks {
      listed.extend(
         toml::from_str::<Lock>(text)
            .map_err(io::Error::other)?
            .package,
      );
   }

   let mut seen = BTreeSet::new();
   let packages = listed
      .iter()
      .filter(|package| {
         seen.insert((
            package.name.as_str(),
            package.version.as_str(),
            package.source.as_deref(),
         ))
      })
      .collect::<Vec<&Package>>();

   let foreign = packages
      .iter()
      .filter(|package| {
         package
            .source
            .as_deref()
            .is_some_and(|origin| origin != CRATES_IO && !origin.starts_with("git+"))
      })
      .map(|package| {
         format!(
            "{} {}",
            package.name,
            package.source.as_deref().unwrap_or_default()
         )
      })
      .collect::<Vec<String>>();
   reject("cargoVendor: unsupported sources", &foreign)?;
   let mut crates = Vec::new();
   let mut rows = Vec::new();
   for &package in &packages {
      if package.source.as_deref() != Some(CRATES_IO) {
         continue;
      }
      let checksum = package.checksum.as_deref().ok_or_else(|| {
         io::Error::other(format!(
            "cargoVendor: {} {} has no checksum",
            package.name, package.version
         ))
      })?;
      rows.push(Row {
         file: format!("{}-{}.tar.gz", package.name, package.version),
         url:  format!(
            "https://static.crates.io/crates/{}/{}-{}.crate",
            package.name, package.name, package.version
         ),
         hash: OutputHash::hex(Algorithm::Sha256, checksum)?,
      });
      crates.push((package, checksum));
   }
   let fetched = fetchurls(store, &rows)?;
   let mut layout = Vec::with_capacity(crates.len() * 2);
   for (&(package, checksum), made) in crates.iter().zip(&fetched) {
      let dir = format!("{prefix}{}-{}", package.name, package.version);
      layout.push(Entry::unpack(made.out.to_string(), dir.clone()));
      layout.push(json_file(
         &format!("{dir}/.cargo-checksum.json"),
         &Checksum {
            files:   BTreeMap::new(),
            package: Some(checksum),
         },
      )?);
   }
   let drvs = fetched.into_iter().map(|made| made.drv).collect();

   let srcs = vendor_git(&packages, prefix, git, &mut layout)?;
   Ok(Vendor { layout, drvs, srcs })
}

/// The crates of `packages` from git sources, linked out of their trees in
/// `git` and listed in [`GIT_SOURCES`], and the trees they use.
fn vendor_git(
   packages: &[&Package],
   prefix: &str,
   git: &BTreeMap<GitSource, StorePath>,
   layout: &mut Vec<Entry>,
) -> io::Result<Vec<StorePath>> {
   let mut used = BTreeSet::new();
   let mut config = String::new();
   let mut scanned = BTreeMap::<&StorePath, Vec<Manifest>>::new();
   for &package in packages {
      let Some(origin) = package
         .source
         .as_deref()
         .filter(|origin| origin.starts_with("git+"))
      else {
         continue;
      };
      let source = origin.parse::<GitSource>()?;
      let tree = git.get(&source).ok_or_else(|| {
         io::Error::other(format!(
            "cargoVendor: no tree for {} {origin}",
            package.name
         ))
      })?;
      let dir = format!(
         "{prefix}{}-{}+{}",
         package.name,
         package.version,
         &source.commit[..12]
      );
      if !scanned.contains_key(tree) {
         scanned.insert(tree, manifests(tree.as_ref())?);
      }
      let found = scanned.get(tree).map(Vec::as_slice).unwrap_or_default();
      layout.extend(git_crate(found, tree.as_ref(), package, &dir)?);
      if used.insert(source.clone()) {
         config.push_str(&source.cargo_config("vendored"));
         config.push('\n');
      }
   }
   if !config.is_empty() {
      layout.push(Entry::write(config, format!("{prefix}{GIT_SOURCES}")));
   }
   Ok(used
      .iter()
      .filter_map(|source| git.get(source).cloned())
      .collect())
}

/// A Cargo.toml of a git tree and the directory it sits in.
struct Manifest {
   dir:   PathBuf,
   table: Table,
}

fn manifests(root: &Path) -> io::Result<Vec<Manifest>> {
   let mut found = Vec::new();
   let mut pending = vec![root.to_path_buf()];
   while let Some(dir) = pending.pop() {
      let manifest = dir.join("Cargo.toml");
      if manifest.exists() {
         let table = read_to_string(&manifest)?
            .parse::<Table>()
            .map_err(|error| io::Error::other(format!("{}: {error}", manifest.display())))?;
         found.push(Manifest {
            dir: dir.clone(),
            table,
         });
      }
      for listed in read_dir(&dir)? {
         let child = listed?;
         let hidden = child
            .file_name()
            .to_str()
            .is_none_or(|name| name.starts_with('.') || name == "target");
         if child.file_type()?.is_dir() && !hidden {
            pending.push(child.path());
         }
      }
   }
   Ok(found)
}

/// The crate `package` from `tree` laid out at `dir`, its manifest
/// normalized as `cargo package` leaves one.
fn git_crate(
   found: &[Manifest],
   tree: &Path,
   package: &Package,
   dir: &str,
) -> io::Result<Vec<Entry>> {
   let workspace = |member: &Path| {
      found
         .iter()
         .filter(|manifest| {
            member.starts_with(&manifest.dir) && manifest.table.contains_key("workspace")
         })
         .max_by_key(|manifest| manifest.dir.components().count())
         .and_then(|manifest| manifest.table.get("workspace"))
         .and_then(Value::as_table)
   };
   let versions = found
      .iter()
      .filter_map(|manifest| {
         let fields = Normalized::new(manifest, workspace(&manifest.dir)).ok()?;
         Some((fields.name()?.to_owned(), fields.version()?.to_owned()))
      })
      .collect::<BTreeMap<String, String>>();
   let manifest = found
      .iter()
      .find(|manifest| {
         Normalized::new(manifest, workspace(&manifest.dir)).is_ok_and(|fields| {
            fields.name() == Some(package.name.as_str())
               && fields.version() == Some(package.version.as_str())
         })
      })
      .ok_or_else(|| {
         io::Error::other(format!(
            "cargoVendor: {} {} is in no manifest of {}",
            package.name,
            package.version,
            tree.display()
         ))
      })?;
   let mut normalized = Normalized::new(manifest, workspace(&manifest.dir))?;
   normalized.local_dependencies(&versions);

   let nested = found
      .iter()
      .filter(|other| other.dir.parent() == Some(manifest.dir.as_path()))
      .map(|other| other.dir.clone())
      .collect::<BTreeSet<PathBuf>>();
   let mut entries = vec![
      Entry::write(
         toml::to_string(&normalized.table).map_err(io::Error::other)?,
         format!("{dir}/Cargo.toml"),
      ),
      json_file(&format!("{dir}/.cargo-checksum.json"), &Checksum {
         files:   BTreeMap::new(),
         package: None,
      })?,
   ];
   // links into the pinned tree, compilers keep the path they were given, so
   // the vendor prefix map still covers what they record
   for child in read_dir(&manifest.dir)? {
      let path = child?.path();
      let name = utf8(Path::new(path.file_name().unwrap_or_default()))?;
      if name == "Cargo.toml"
         || name == "target"
         || name.starts_with(".git")
         || nested.contains(&path)
      {
         continue;
      }
      entries.push(Entry::link(
         utf8(&path)?.to_owned(),
         format!("{dir}/{name}"),
      ));
   }
   Ok(entries)
}

const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "build-dependencies", "dev-dependencies"];

/// A manifest with what it inherits from its workspace written in and the
/// `[workspace]` table gone, as a crate stands outside its workspace.
struct Normalized {
   table: Table,
}

impl Normalized {
   fn new(manifest: &Manifest, workspace: Option<&Table>) -> io::Result<Self> {
      let mut table = manifest.table.clone();
      table.remove("workspace");
      let inherited = |section: &str| {
         workspace
            .and_then(|root| root.get(section))
            .and_then(Value::as_table)
      };
      let missing = |what: &str| {
         io::Error::other(format!(
            "{}: inherits {what}, which its workspace does not set",
            manifest.dir.display()
         ))
      };

      if let Some(fields) = table.get_mut("package").and_then(Value::as_table_mut) {
         for (key, value) in fields.iter_mut() {
            if inherits(value) {
               *value = inherited("package")
                  .and_then(|shared| shared.get(key))
                  .cloned()
                  .ok_or_else(|| missing(&format!("package.{key}")))?;
            }
         }
      }
      if table.get("lints").is_some_and(inherits) {
         match workspace.and_then(|root| root.get("lints")) {
            Some(lints) => table.insert("lints".to_owned(), lints.clone()),
            None => table.remove("lints"),
         };
      }
      let shared = inherited("dependencies");
      for (_, dependencies) in dependency_tables(&mut table) {
         for (name, value) in dependencies.iter_mut() {
            if !inherits(value) {
               continue;
            }
            let base = shared
               .and_then(|listed| listed.get(name))
               .ok_or_else(|| missing(&format!("dependency {name}")))?;
            *value = Value::Table(inherit_dependency(base, value).ok_or_else(|| {
               io::Error::other(format!(
                  "{}: workspace dependency {name} is neither a version nor a table",
                  manifest.dir.display()
               ))
            })?);
         }
      }
      Ok(Self { table })
   }

   fn name(&self) -> Option<&str> {
      self.package()?.get("name")?.as_str()
   }

   fn version(&self) -> Option<&str> {
      self.package()?.get("version")?.as_str()
   }

   fn package(&self) -> Option<&Table> {
      self.table.get("package")?.as_table()
   }

   /// Dependencies on crates of the same tree by `path`, which does not
   /// resolve from the vendor directory, as `cargo package` publishes them:
   /// by the version the tree gives them, a dev-dependency with none dropped.
   fn local_dependencies(&mut self, versions: &BTreeMap<String, String>) {
      for (kind, dependencies) in dependency_tables(&mut self.table) {
         let dev = kind == "dev-dependencies";
         dependencies.retain(|name, value| {
            let Some(spec) = value.as_table_mut() else {
               return true;
            };
            if spec.remove("path").is_none() || spec.contains_key("version") {
               return true;
            }
            let crate_name = spec.get("package").and_then(Value::as_str).unwrap_or(name);
            match versions.get(crate_name) {
               Some(version) if !dev => {
                  spec.insert("version".to_owned(), Value::String(version.clone()));
                  true
               },
               _ => false,
            }
         });
      }
   }
}

fn inherits(value: &Value) -> bool {
   value
      .as_table()
      .and_then(|table| table.get("workspace"))
      .and_then(Value::as_bool)
      == Some(true)
}

/// A workspace dependency with what the member adds, its features joined to
/// the shared ones, none for a workspace entry no manifest could spell.
fn inherit_dependency(base: &Value, local: &Value) -> Option<Table> {
   let mut merged = match *base {
      Value::String(_) => Table::from_iter([("version".to_owned(), base.clone())]),
      Value::Table(ref spec) => spec.clone(),
      Value::Integer(_)
      | Value::Float(_)
      | Value::Boolean(_)
      | Value::Datetime(_)
      | Value::Array(_) => return None,
   };
   let Some(added) = local.as_table() else {
      return Some(merged);
   };
   for (key, value) in added {
      match key.as_str() {
         "workspace" => {},
         "features" => {
            let mut features = merged
               .get("features")
               .and_then(Value::as_array)
               .cloned()
               .unwrap_or_default();
            for feature in value.as_array().into_iter().flatten() {
               if !features.contains(feature) {
                  features.push(feature.clone());
               }
            }
            merged.insert(key.clone(), Value::Array(features));
         },
         _ => {
            merged.insert(key.clone(), value.clone());
         },
      }
   }
   Some(merged)
}

/// The manifest's dependency tables with their kind, its own and each
/// `[target.<cfg>]`'s.
fn dependency_tables(table: &mut Table) -> Vec<(&'static str, &mut Table)> {
   let mut found = Vec::new();
   for (key, value) in table.iter_mut() {
      if key == "target" {
         let sections = value
            .as_table_mut()
            .into_iter()
            .flat_map(|platforms| platforms.iter_mut().map(|entry| entry.1))
            .filter_map(Value::as_table_mut)
            .flat_map(|sections| sections.iter_mut());
         found.extend(sections.filter_map(|(kind, section)| kinded(kind, section)));
      } else {
         found.extend(kinded(key, value));
      }
   }
   found
}

fn kinded<'table>(
   key: &str,
   value: &'table mut Value,
) -> Option<(&'static str, &'table mut Table)> {
   DEPENDENCY_TABLES
      .into_iter()
      .find(|kind| *kind == key)
      .zip(value.as_table_mut())
}
