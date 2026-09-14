use std::{
   collections::{
      BTreeMap,
      VecDeque,
   },
   fs::read_to_string,
   io,
   ptr::eq,
};

use serde::{
   Deserialize,
   Serialize,
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
         json_file,
         say,
      },
      pep508::evaluate,
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

#[derive(Deserialize)]
struct Pyproject {
   project: Project,
   #[serde(default)]
   tool:    Tool,
}

#[derive(Deserialize)]
struct Project {
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

#[derive(Deserialize)]
struct SysLibs {
   python: BTreeMap<String, serde_json::Value>,
}

#[derive(Serialize)]
struct Planned<'package> {
   name:    &'package str,
   version: &'package str,
   file:    String,
   kind:    &'static str,
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
   marker_env: BTreeMap<&'static str, String>,
}

impl Platform {
   fn load() -> io::Result<Self> {
      let python = attr("pythonVersion")?;
      let cpu = attr("cpu")?;
      let mut parts = python.split('.');
      let major = parts.next().unwrap_or_default();
      let minor = parts.next().unwrap_or_default();
      let marker_env = BTreeMap::from([
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
      ]);
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
   ) -> io::Result<(&'package Artefact, &'static str)> {
      if !package.source.contains_key("registry") {
         return Err(io::Error::other(format!(
            "pythonDeps: {} comes from {}, not a registry; uv.lock records no hash for that",
            package.name,
            package.source.keys().cloned().collect::<Vec<_>>().join(" ")
         )));
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
         && (!force_sdist || rank == 0)
      {
         return Ok((wheel, "wheel"));
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
         |sdist| Ok((sdist, "sdist")),
      )
   }
}

struct Closure<'lock> {
   packages: BTreeMap<&'lock str, Vec<&'lock Package>>,
   holds:    BTreeMap<&'lock str, bool>,
   env:      &'lock BTreeMap<&'static str, String>,
}

struct Visit<'lock> {
   extras: Vec<&'lock str>,
   entry:  &'lock Package,
}

impl<'lock> Closure<'lock> {
   fn marker_holds(
      holds: &mut BTreeMap<&'lock str, bool>,
      env: &BTreeMap<&str, String>,
      marker: &'lock str,
   ) -> io::Result<bool> {
      if let Some(known) = holds.get(marker) {
         return Ok(*known);
      }
      let value = evaluate(marker, env)?;
      holds.insert(marker, value);
      Ok(value)
   }

   fn edges(&mut self, package: &'lock Package, extras: &[String]) -> io::Result<Vec<&'lock Edge>> {
      let optional = extras
         .iter()
         .filter_map(|extra| package.optional_dependencies.get(extra))
         .flatten();
      let mut kept = Vec::new();
      for edge in package.dependencies.iter().chain(optional) {
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
   ) -> io::Result<Vec<&'lock Package>> {
      let mut visited: BTreeMap<&str, Visit<'lock>> = BTreeMap::new();
      let mut order = Vec::new();
      let mut queue: VecDeque<&Edge> = self.edges(project, extras)?.into();
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
         queue.extend(self.edges(target, wanted)?);
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
      let root = format!("{}/{}", attr("source")?, attr("root")?);
      let lock: Lock =
         toml::from_str(&read_to_string(format!("{root}/uv.lock"))?).map_err(io::Error::other)?;
      let pyproject: Pyproject = toml::from_str(&read_to_string(format!("{root}/pyproject.toml"))?)
         .map_err(io::Error::other)?;
      if lock.version < 1 {
         return Err(io::Error::other(
            "pythonDeps: uv.lock has no `version`, too old",
         ));
      }
      let platform = Platform::load()?;
      let extras: Vec<String> = attr("extras")?
         .split(',')
         .filter(|extra| !extra.is_empty())
         .map(str::to_owned)
         .collect();
      let sys_libs: SysLibs = serde_json::from_str(&read_to_string(attr("sysLibs")?)?)?;
      let mut force_sdist = pyproject.tool.uv.no_binary_package.clone();
      force_sdist.extend(sys_libs.python.keys().cloned());

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
      let selected = closure.runtime(project, &extras)?;

      let mut rows = Vec::with_capacity(selected.len());
      let mut plan = Vec::with_capacity(selected.len());
      for package in selected {
         let name = package.name.as_str();
         let (artefact, kind) = platform.choose(package, force_sdist.contains(&package.name))?;
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
         plan.push(Planned {
            name,
            version: package.version.as_deref().unwrap_or_default(),
            file: file.clone(),
            kind,
         });
         rows.push(Row {
            file,
            url,
            hash: Hash::Sha256(hash),
         });
      }
      let fetched = fetchurls(store, &rows)?;
      let from_sdist: Vec<&str> = plan
         .iter()
         .filter(|planned| planned.kind == "sdist")
         .map(|planned| planned.name)
         .collect();
      say(&format!(
         "pythonDeps: {} packages, {} from sdist",
         plan.len(),
         from_sdist.join(" ")
      ));
      let mut layout: Vec<Entry> = plan
         .iter()
         .zip(&fetched)
         .map(|(planned, made)| Entry::link(made.out.clone(), format!("dist/{}", planned.file)))
         .collect();
      layout.push(json_file("plan.json", &plan)?);
      let inputs: Vec<String> = fetched.into_iter().map(|made| made.drv).collect();
      collect(store, "python-deps", &layout, &inputs, None, Map::new())
   }
}
