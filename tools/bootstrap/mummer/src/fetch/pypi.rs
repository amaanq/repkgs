use std::{
   collections::{
      BTreeMap,
      VecDeque,
   },
   fs::read_to_string,
   io,
   path::Path,
   ptr::eq,
};

use mummer::{
   hash::{
      Algorithm,
      OutputHash,
   },
   syslibs::load,
};
use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Entry,
         Row,
         attr,
         fetchurls,
         json_file,
         say,
         stage,
      },
      pep508::evaluate,
      pypi_vendor::{
         Sdist,
         Stage,
         Tree,
      },
      registry::{
         basename,
         url_path,
      },
   },
   nix_store::Store,
};

const NO_MATCH: u8 = u8::MAX;

#[derive(Deserialize)]
struct Lock {
   #[serde(default)]
   version: u64,
   package: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
   name:                  String,
   version:               Option<String>,
   #[serde(default)]
   source:                BTreeMap<String, String>,
   #[serde(default)]
   dependencies:          Vec<Edge>,
   #[serde(default, rename = "optional-dependencies")]
   optional_dependencies: BTreeMap<String, Vec<Edge>>,
   #[serde(default, rename = "dev-dependencies")]
   dev_dependencies:      BTreeMap<String, Vec<Edge>>,
   #[serde(default, rename = "resolution-markers")]
   resolution_markers:    Vec<String>,
   #[serde(default)]
   wheels:                Vec<Artefact>,
   sdist:                 Option<Artefact>,
}

#[derive(Deserialize)]
struct Edge {
   name:    String,
   version: Option<String>,
   marker:  Option<String>,
   #[serde(default)]
   extra:   Vec<String>,
}

#[derive(Deserialize)]
struct Artefact {
   url:  Option<String>,
   hash: Option<String>,
}

#[derive(Default, Deserialize)]
struct Pyproject {
   #[serde(default)]
   project: Project,
   #[serde(default)]
   tool:    Tool,
}

#[derive(Default, Deserialize)]
struct Project {
   #[serde(default)]
   name: String,
}

#[derive(Default, Deserialize)]
struct Tool {
   #[serde(default)]
   uv: Uv,
}

#[derive(Default, Deserialize)]
struct Uv {
   #[serde(default, rename = "no-binary-package")]
   no_binary_package: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
   Wheel,
   Sdist,
   Tree,
}

#[derive(Serialize)]
struct Planned<'package> {
   name:    &'package str,
   version: &'package str,
   file:    String,
   kind:    Kind,
   path:    String,
}

impl<'package> Planned<'package> {
   fn select(
      package: &'package Package,
      platform: &Platform,
      force_sdist: bool,
      prefer_sdist: bool,
      git: &BTreeMap<&str, &str>,
   ) -> io::Result<(Self, Option<Row>)> {
      let name = package.name.as_str();
      let local = package
         .source
         .get("editable")
         .or_else(|| package.source.get("directory"))
         .or_else(|| package.source.get("virtual"));

      if let Some(path) = local {
         return Ok((
            Self {
               name,
               version: package.version.as_deref().unwrap_or_default(),
               kind: Kind::Tree,
               file: String::new(),
               path: path.clone(),
            },
            None,
         ));
      }

      if package.source.contains_key("git") {
         let path = git.get(name).ok_or_else(|| {
            io::Error::other(format!(
               "pythonDeps: {name} is a git source nix/python.nix did not fetch"
            ))
         })?;
         return Ok((
            Self {
               name,
               version: package.version.as_deref().unwrap_or_default(),
               kind: Kind::Tree,
               file: format!("dist/{name}"),
               path: (*path).to_owned(),
            },
            None,
         ));
      }

      let (artefact, kind) = platform.choose(package, force_sdist, prefer_sdist)?;
      let url = artefact
         .url
         .clone()
         .ok_or_else(|| io::Error::other(format!("pythonDeps: {name} artefact has no url")))?;
      let hash = artefact
         .hash
         .as_deref()
         .and_then(|hash| hash.strip_prefix("sha256:"))
         .ok_or_else(|| io::Error::other(format!("pythonDeps: {name} artefact has no sha256")))?
         .to_owned();
      let file = percent_decode(basename(url_path(&url)));
      let planned = Self {
         name,
         version: package.version.as_deref().unwrap_or_default(),
         file: file.clone(),
         kind,
         path: String::new(),
      };
      let row = Row {
         file,
         url,
         hash: OutputHash::hex(Algorithm::Sha256, &hash)?,
      };
      Ok((planned, Some(row)))
   }
}

fn normalize(name: &str) -> String {
   let mut normalized = String::with_capacity(name.len());
   let mut separator = false;
   for character in name.chars() {
      if matches!(character, '-' | '_' | '.') {
         separator = true;
         continue;
      }
      if separator {
         normalized.push('-');
         separator = false;
      }
      normalized.extend(character.to_lowercase());
   }
   normalized
}

fn percent_decode(text: &str) -> String {
   let bytes = text.as_bytes();
   let mut decoded = Vec::with_capacity(bytes.len());
   let mut index = 0;
   while index < bytes.len() {
      let hex = (bytes[index] == b'%' && index + 2 < bytes.len())
         .then(|| u8::from_str_radix(&text[index + 1..index + 3], 16).ok())
         .flatten();
      if let Some(byte) = hex {
         decoded.push(byte);
         index += 3;
      } else {
         decoded.push(bytes[index]);
         index += 1;
      }
   }
   String::from_utf8_lossy(&decoded).into_owned()
}

struct Platform {
   cpu:        String,
   python:     String,
   cpython:    String,
   marker_env: BTreeMap<String, String>,
}

impl Platform {
   fn load() -> io::Result<Self> {
      let python = attr("pythonVersion")?;
      let cpu = attr("cpu")?;
      let mut parts = python.split('.');
      let major = parts.next().unwrap_or_default();
      let minor = parts.next().unwrap_or_default();
      let mut marker_env: BTreeMap<String, String> = [
         ("python_full_version", python.clone()),
         ("python_version", format!("{major}.{minor}")),
         ("sys_platform", "linux".to_owned()),
         ("platform_system", "Linux".to_owned()),
         ("platform_machine", cpu.clone()),
         ("os_name", "posix".to_owned()),
         ("implementation_name", "cpython".to_owned()),
         ("implementation_cap", "CPython".to_owned()),
         ("implementation_version", python.clone()),
         ("platform_release", String::new()),
         ("platform_version", String::new()),
         ("extra", String::new()),
      ]
      .into_iter()
      .map(|(key, value)| (key.to_owned(), value))
      .collect();
      marker_env.extend(serde_json::from_str::<BTreeMap<String, String>>(&attr(
         "environ",
      )?)?);
      Ok(Self {
         cpython: format!("cp{major}{minor}"),
         cpu,
         python,
         marker_env,
      })
   }

   fn cpython_minor(tag: &str) -> Option<u64> {
      tag.get(3..)?.parse().ok()
   }

   fn python_ok(&self, tag: &str, abi: &str) -> bool {
      if tag == self.cpython {
         return true;
      }
      if let Some(rest) = tag.strip_prefix("py3")
         && rest.bytes().all(|byte| byte.is_ascii_digit())
      {
         return true;
      }
      abi == "abi3"
         && tag.starts_with("cp3")
         && tag[3..].bytes().all(|byte| byte.is_ascii_digit())
         && Self::cpython_minor(tag) <= Self::cpython_minor(&self.cpython)
   }

   fn platform_ok(&self, tag: &str) -> bool {
      let Some(prefix) = tag.strip_suffix(&format!("_{}", self.cpu)) else {
         return false;
      };
      prefix == "linux"
         || prefix.strip_prefix("manylinux").is_some_and(|rest| {
            rest
               .bytes()
               .all(|byte| byte.is_ascii_digit() || byte == b'_')
         })
   }

   /// 0 pure python, 1 built for exactly our cpython, 2 abi3/none for our cpu,
   /// `NO_MATCH` otherwise. Wheel names are
   /// `<name>-<version>[-<build>]-<python tag>-<abi tag>-<platform tag>.whl`.
   fn wheel_rank(&self, file: &str) -> u8 {
      let stem = file.strip_suffix(".whl").unwrap_or(file);
      let fields: Vec<&str> = stem.split('-').collect();
      if fields.len() < 5 {
         return NO_MATCH;
      }
      let python_tags = fields[fields.len() - 3].split('.');
      let abi = fields[fields.len() - 2];
      let platform_tags: Vec<&str> = fields[fields.len() - 1].split('.').collect();
      if !python_tags.clone().any(|tag| self.python_ok(tag, abi)) {
         return NO_MATCH;
      }
      if abi == "none" && platform_tags == ["any"] {
         return 0;
      }
      if !platform_tags.iter().any(|tag| self.platform_ok(tag)) {
         return NO_MATCH;
      }
      if abi == self.cpython {
         1
      } else if abi == "abi3" || abi == "none" {
         2
      } else {
         NO_MATCH
      }
   }

   /// the best-ranked compatible wheel unless an sdist is forced (a pure wheel
   /// still wins then, nothing to link), else the sdist.
   fn choose<'package>(
      &self,
      package: &'package Package,
      force_sdist: bool,
      prefer_sdist: bool,
   ) -> io::Result<(&'package Artefact, Kind)> {
      if !package.source.contains_key("registry") {
         return Err(io::Error::other(format!(
            "pythonDeps: {} comes from {}, not a registry; uv.lock records no hash for that",
            package.name,
            package.source.keys().cloned().collect::<Vec<_>>().join(" ")
         )));
      }
      if prefer_sdist && let Some(ref sdist) = package.sdist {
         return Ok((sdist, Kind::Sdist));
      }
      let best = package
         .wheels
         .iter()
         .map(|wheel| {
            (
               self.wheel_rank(basename(url_path(wheel.url.as_deref().unwrap_or_default()))),
               wheel,
            )
         })
         .filter(|ranked| ranked.0 < NO_MATCH)
         .min_by_key(|ranked| ranked.0);
      if let Some((rank, wheel)) = best
         && (!force_sdist || rank == 0 || package.sdist.is_none())
      {
         return Ok((wheel, Kind::Wheel));
      }
      package.sdist.as_ref().map_or_else(
         || {
            Err(io::Error::other(format!(
               "pythonDeps: {} {}: no wheel for cp{} linux/{} and no sdist",
               package.name,
               package.version.as_deref().unwrap_or_default(),
               self.python,
               self.cpu
            )))
         },
         |sdist| Ok((sdist, Kind::Sdist)),
      )
   }
}

struct Closure<'lock> {
   packages: BTreeMap<&'lock str, Vec<&'lock Package>>,
   holds:    BTreeMap<&'lock str, bool>,
   env:      &'lock BTreeMap<String, String>,
}

struct Visit<'lock> {
   extras: Vec<&'lock str>,
   entry:  &'lock Package,
}

impl<'lock> Closure<'lock> {
   fn marker_holds(
      holds: &mut BTreeMap<&'lock str, bool>,
      env: &BTreeMap<String, String>,
      marker: &'lock str,
   ) -> io::Result<bool> {
      if let Some(known) = holds.get(marker) {
         return Ok(*known);
      }
      let value = evaluate(marker, env)?;
      holds.insert(marker, value);
      Ok(value)
   }

   fn edges(
      &mut self,
      package: &'lock Package,
      extras: &[String],
      groups: &[String],
   ) -> io::Result<Vec<&'lock Edge>> {
      let optional = extras
         .iter()
         .filter_map(|extra| package.optional_dependencies.get(extra))
         .flatten();
      let grouped = groups
         .iter()
         .filter_map(|group| package.dev_dependencies.get(group))
         .flatten();
      let mut kept = Vec::new();
      for edge in package.dependencies.iter().chain(grouped).chain(optional) {
         let Some(marker) = edge.marker.as_deref() else {
            kept.push(edge);
            continue;
         };
         if Self::marker_holds(&mut self.holds, self.env, marker)? {
            kept.push(edge);
         }
      }
      Ok(kept)
   }

   fn resolve(&mut self, edge: &Edge) -> io::Result<&'lock Package> {
      let candidates = self
         .packages
         .get(edge.name.as_str())
         .ok_or_else(|| io::Error::other(format!("pythonDeps: {} is not in uv.lock", edge.name)))?;
      if let Some(version) = edge.version.as_deref() {
         return candidates
            .iter()
            .copied()
            .find(|candidate| candidate.version.as_deref() == Some(version))
            .ok_or_else(|| {
               io::Error::other(format!("pythonDeps: no lock entry for {}", edge.name))
            });
      }
      if candidates.len() == 1 {
         return Ok(candidates[0]);
      }
      let mut ours = Vec::new();
      for candidate in candidates.iter().copied() {
         for marker in &candidate.resolution_markers {
            if Self::marker_holds(&mut self.holds, self.env, marker)? {
               ours.push(candidate);
               break;
            }
         }
      }
      if ours.len() != 1 {
         return Err(io::Error::other(format!(
            "pythonDeps: {} has {} in uv.lock and resolution-markers pick {} of them",
            edge.name,
            candidates
               .iter()
               .map(|candidate| candidate.version.as_deref().unwrap_or_default())
               .collect::<Vec<_>>()
               .join(", "),
            ours.len(),
         )));
      }
      Ok(ours[0])
   }

   /// entries of every package the project needs at run time, breadth-first
   /// from the project's own lock entry over `dependencies` plus the requested
   /// extras, skipping edges whose marker is false here.
   fn runtime(
      &mut self,
      project: &'lock Package,
      extras: &[String],
      groups: &[String],
   ) -> io::Result<Vec<&'lock Package>> {
      let mut visited: BTreeMap<&str, Visit<'lock>> = BTreeMap::new();
      let mut order = Vec::new();
      let mut queue: VecDeque<&Edge> = self.edges(project, extras, groups)?.into();
      while let Some(edge) = queue.pop_front() {
         let wanted = &edge.extra;
         let target = self.resolve(edge)?;
         if let Some(done) = visited.get(edge.name.as_str()) {
            if !eq(done.entry, target) {
               return Err(io::Error::other(format!(
                  "pythonDeps: conflicting lock entries for {}",
                  edge.name
               )));
            }
            if wanted
               .iter()
               .all(|extra| done.extras.contains(&extra.as_str()))
            {
               continue;
            }
         }
         let seen = visited.entry(edge.name.as_str()).or_insert_with(|| {
            order.push(edge.name.as_str());
            Visit {
               extras: Vec::new(),
               entry:  target,
            }
         });
         for extra in wanted {
            if !seen.extras.contains(&extra.as_str()) {
               seen.extras.push(extra.as_str());
            }
         }
         queue.extend(self.edges(target, wanted, &[])?);
      }
      Ok(order.into_iter().map(|name| visited[name].entry).collect())
   }
}

/// uv.lock -> { dist/, plan.json }, one artefact per package of the
/// application's runtime closure, a compatible wheel if the lock has one,
/// else the sdist.
pub struct Pypi;

impl Producer for Pypi {
   fn name(&self) -> &'static str {
      "pypi"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let source = attr("source")?;
      let root = format!("{source}/{}", attr("root")?);
      let lock: Lock =
         toml::from_str(&read_to_string(format!("{root}/uv.lock"))?).map_err(io::Error::other)?;
      let project_file = format!("{root}/pyproject.toml");
      let pyproject: Pyproject = if Path::new(&project_file).exists() {
         toml::from_str(&read_to_string(project_file)?).map_err(io::Error::other)?
      } else {
         Pyproject::default()
      };

      if lock.version < 1 {
         return Err(io::Error::other(
            "pythonDeps: uv.lock has no `version`, too old",
         ));
      }

      let platform = Platform::load()?;
      let csv = |value: String| {
         value
            .split(',')
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
      };
      let extras = csv(attr("extras")?);
      let groups = csv(attr("groups")?);
      let prefer_sdist = attr("prefer")? == "sdist";
      let sys_libs = load(Path::new(&attr("sysLibs")?))?;
      let mut force_sdist = pyproject.tool.uv.no_binary_package;
      force_sdist.extend(
         sys_libs
            .get("python")
            .into_iter()
            .flat_map(|table| table.iter())
            .filter(|&(_name, entry)| entry.pkg.is_some())
            .map(|(name, _entry)| name.clone()),
      );
      force_sdist.extend(csv(attr("sdist")?));
      let git_sources = attr("git")?;
      let git: BTreeMap<&str, &str> = git_sources
         .lines()
         .filter(|line| !line.is_empty())
         .map(|line| {
            line
               .split_once('=')
               .ok_or_else(|| io::Error::other("pythonDeps: git input must be name=path"))
         })
         .collect::<io::Result<_>>()?;
      let mut packages: BTreeMap<&str, Vec<&Package>> = BTreeMap::new();

      for package in &lock.package {
         packages.entry(&package.name).or_default().push(package);
      }

      let project_name = normalize(&pyproject.project.name);
      let project = lock
         .package
         .iter()
         .find(|package| {
            package.name == project_name
               || package.source.get("editable").is_some_and(|dir| dir == ".")
               || package.source.get("virtual").is_some_and(|dir| dir == ".")
         })
         .ok_or_else(|| {
            io::Error::other(format!("pythonDeps: {project_name} is not in uv.lock"))
         })?;
      let mut closure = Closure {
         packages,
         holds: BTreeMap::new(),
         env: &platform.marker_env,
      };
      let selected = closure.runtime(project, &extras, &groups)?;
      let mut rows = Vec::with_capacity(selected.len());
      let mut plan = Vec::with_capacity(selected.len());
      let mut trees = Vec::new();

      for package in selected {
         let (planned, row) = Planned::select(
            package,
            &platform,
            force_sdist.contains(&package.name),
            prefer_sdist,
            &git,
         )?;

         if let Some(fetch) = row {
            rows.push(fetch);
            plan.push(planned);
         } else {
            trees.push(planned);
         }
      }

      Self::submit(store, &source, plan, trees, &rows)
   }
}

impl Pypi {
   fn submit<'package>(
      store: &mut Store,
      source: &str,
      mut plan: Vec<Planned<'package>>,
      trees: Vec<Planned<'package>>,
      rows: &[Row],
   ) -> io::Result<()> {
      let fetched = fetchurls(store, rows)?;
      let mut layout = Vec::with_capacity(plan.len() + trees.len() + 1);
      let mut sdists = Vec::new();
      let mut inputs = Vec::new();
      let mut drvs = Vec::with_capacity(fetched.len());

      for (planned, made) in plan.iter().zip(fetched) {
         layout.push(Entry::link(
            made.out.clone(),
            format!("dist/{}", planned.file),
         ));

         if matches!(planned.kind, Kind::Sdist) {
            sdists.push(Sdist {
               name: planned.name.to_owned(),
               out:  made.out,
            });
            inputs.push(made.drv.clone());
         }

         drvs.push(made.drv);
      }

      let srcs: Vec<String> = trees
         .iter()
         .filter(|tree| !tree.file.is_empty())
         .map(|tree| tree.path.clone())
         .collect();
      let stage_trees = trees
         .iter()
         .map(|tree| {
            Tree {
               name: tree.name.to_owned(),
               dir:  if tree.file.is_empty() {
                  format!("{source}/{}", tree.path)
               } else {
                  tree.path.clone()
               },
            }
         })
         .collect();

      for tree in &trees {
         if !tree.file.is_empty() {
            layout.push(Entry::link(tree.path.clone(), tree.file.clone()));
         }
      }

      say(&format!(
         "pythonDeps: {} packages, {} from sdist, {} from trees",
         plan.len(),
         sdists
            .iter()
            .map(|sdist| sdist.name.as_str())
            .collect::<Vec<_>>()
            .join(" "),
         trees
            .iter()
            .map(|tree| tree.name)
            .collect::<Vec<_>>()
            .join(" ")
      ));
      plan.extend(trees);
      layout.push(json_file("plan.json", &plan)?);
      let mut stage_sources = srcs.clone();
      stage_sources.push(source.to_owned());
      stage(
         store,
         "python-deps",
         "pypi-vendor",
         &Stage {
            layout,
            drvs,
            srcs,
            sdists,
            trees: stage_trees,
         },
         &inputs,
         &stage_sources,
      )
   }
}
