use std::{
   collections::BTreeMap,
   fs::{
      canonicalize,
      create_dir_all,
      metadata,
      read_to_string,
   },
   io::{
      self,
      Write as _,
   },
   os::unix::fs::PermissionsExt as _,
   path::{
      Path,
      PathBuf,
   },
};

use serde::{
   Deserialize as _,
   Serialize,
};
use serde_json::Value;

use crate::{
   attrs::{
      Attrs,
      Cc,
      Platform,
      Spec,
      build_top,
   },
   exports::{
      Exports,
      closure,
      exports_of,
   },
   hardening::{
      Line,
      flags as hardening_flags,
   },
};

const EPOCH: &str = "315532800";

#[derive(Serialize)]
struct PackageFlags {
   cflags:   Vec<String>,
   cxxflags: Vec<String>,
   ldflags:  Vec<String>,
}

pub fn run(out: &Path, cache: bool) -> io::Result<()> {
   let attrs = Attrs::load()?;
   let spec = attrs.spec()?;
   let deps = closure(&attrs.dependencies)?;
   let env = compute(&attrs, &spec, &deps, out, cache)?;
   let mut stdout = io::stdout().lock();
   serde_json::to_writer(&mut stdout, &env)?;
   writeln!(stdout)?;
   Ok(())
}

pub fn compute(
   attrs: &Attrs,
   spec: &Spec,
   deps: &[Exports],
   out: &Path,
   cache: bool,
) -> io::Result<BTreeMap<String, Value>> {
   let top = build_top()?;
   let bins: Vec<PathBuf> = attrs
      .build_dependencies
      .iter()
      .map(|root| Path::new(root).join("bin"))
      .collect();
   let mut env: BTreeMap<String, Value> = BTreeMap::new();
   sandbox_dirs(&mut env, &top, &bins, out)?;
   reproducible(&mut env, attrs);
   toolchain(&mut env, attrs, spec, deps, &top, &bins)?;
   let platform = Platform::deserialize(&attrs.platform)?;
   package_cc(&mut env, spec, &platform, deps, &bins)?;
   for root in &attrs.build_dependencies {
      for (name, value) in exports_of(Path::new(root))?.env {
         env.insert(name, Value::String(value));
      }
   }
   env.extend(
      spec
         .env
         .iter()
         .map(|(name, value)| (name.clone(), value.clone())),
   );
   if cache {
      compiler_caches(&mut env, &bins);
   }
   Ok(env)
}

fn set(env: &mut BTreeMap<String, Value>, name: &str, value: impl Into<String>) {
   env.insert(name.to_owned(), Value::String(value.into()));
}

fn sandbox_dirs(
   env: &mut BTreeMap<String, Value>,
   top: &str,
   bins: &[PathBuf],
   out: &Path,
) -> io::Result<()> {
   // the builder's stdout is a pty, ninja would overwrite instead of logging
   // each step, so CI and a dumb TERM keep every tool from prompting or
   // redrawing a status line
   let home = format!("{top}/home");
   create_dir_all(&home)?;
   env.insert(
      "PATH".to_owned(),
      Value::Array(
         bins
            .iter()
            .map(|bin| Value::String(bin.to_string_lossy().into_owned()))
            .collect(),
      ),
   );
   set(env, "HOME", home.as_str());
   set(env, "XDG_CACHE_HOME", format!("{home}/.cache"));
   set(env, "XDG_DATA_HOME", format!("{home}/.local/share"));
   set(env, "XDG_CONFIG_HOME", format!("{home}/.config"));
   set(env, "TMPDIR", top);
   set(env, "CI", "true");
   set(env, "TERM", "dumb");
   set(env, "out", out.to_string_lossy());
   Ok(())
}

fn reproducible(env: &mut BTreeMap<String, Value>, attrs: &Attrs) {
   // 1980-01-01 is the earliest mtime ZIP archives (wheels, jars) can store,
   // and clang derives __DATE__ from SOURCE_DATE_EPOCH
   set(env, "SOURCE_DATE_EPOCH", EPOCH);
   set(env, "TZ", "UTC");
   set(env, "LC_ALL", "C.UTF-8");
   set(env, "ZERO_AR_DATE", "1");
   set(env, "PERL_HASH_SEED", "0");
   set(env, "PYTHONHASHSEED", "0");
   set(env, "KBUILD_BUILD_TIMESTAMP", format!("@{EPOCH}"));
   set(env, "KBUILD_BUILD_USER", "pkgs");
   set(env, "KBUILD_BUILD_HOST", "pkgs");
   set(env, "CONFIG_SITE", attrs.config_site.as_str());
}

fn dirs(deps: &[Exports], field: fn(&Exports) -> &[String]) -> Vec<String> {
   deps
      .iter()
      .flat_map(|dep| {
         field(dep)
            .iter()
            .map(move |rel| format!("{}/{rel}", dep.root))
      })
      .collect()
}

fn masked(path: &str, under: &str) -> String {
   let name = path.rsplit('/').next().unwrap_or(path);
   format!("{path}=/{under}/{}", name.get(33..).unwrap_or_default())
}

fn toolchain(
   env: &mut BTreeMap<String, Value>,
   attrs: &Attrs,
   spec: &Spec,
   deps: &[Exports],
   top: &str,
   bins: &[PathBuf],
) -> io::Result<()> {
   let mut roots: Vec<String> = deps.iter().map(|dep| dep.root.clone()).collect();
   roots.extend(attrs.build_dependencies.iter().cloned());
   roots.extend(spec.lock_trees().map(str::to_owned));
   if let Some(cc) = find_tool(bins, "cc") {
      let package = cc
         .parent()
         .and_then(Path::parent)
         .ok_or_else(|| io::Error::other("cc has no installation root"))?;
      let listed = read_to_string(package.join("etc/roots"))?;
      roots.extend(listed.split_whitespace().map(str::to_owned));
   }
   let mut store_roots: Vec<String> = Vec::new();
   for root in roots {
      if !store_roots.contains(&root) {
         store_roots.push(root);
      }
   }
   let mut prefix_map = vec![format!("{top}=/build")];
   prefix_map.extend(deps.iter().map(|dep| masked(&dep.root, "deps")));
   prefix_map.extend(
      attrs
         .build_dependencies
         .iter()
         .map(|root| masked(root, "tools")),
   );
   set(env, "CC", "cc");
   set(env, "CXX", "c++");
   set(env, "AR", "llvm-ar");
   set(env, "RANLIB", "llvm-ranlib");
   set(env, "NM", "llvm-nm");
   set(env, "STRIP", "llvm-strip");
   set(
      env,
      "PKG_CONFIG_PATH",
      dirs(deps, |dep| &dep.pkgconfig_dirs).join(":"),
   );
   set(
      env,
      "CMAKE_PREFIX_PATH",
      deps
         .iter()
         .map(|dep| dep.root.as_str())
         .collect::<Vec<_>>()
         .join(";"),
   );
   set(
      env,
      "ACLOCAL_PATH",
      dirs(deps, |dep| &dep.aclocal_dirs).join(":"),
   );
   set(env, "JIG_LOG", format!("{top}/jig.log"));
   set(env, "JIG_LOG_ARGS", format!("{top}/jig-uncached.log"));
   set(env, "JIG_STORE_IDENTITY", "content");
   set(env, "JIG_STORE_ROOTS", store_roots.join(" "));
   set(env, "PKGS_PREFIX_MAP", prefix_map.join(":"));
   Ok(())
}

fn package_cc(
   env: &mut BTreeMap<String, Value>,
   spec: &Spec,
   platform: &Platform,
   deps: &[Exports],
   bins: &[PathBuf],
) -> io::Result<()> {
   // keyed by toolchain root so cc-build gets none of it, and not CFLAGS
   // because a Makefile that sets CFLAGS must not lose them
   let cc = spec.cc.as_ref();
   let choice = cc.and_then(|found| found.hardening.as_ref());
   let harden = |line| hardening_flags(platform.binfmt, &platform.hardening, choice, line);
   let (debug_flag, cargo_debug) = if spec.debug {
      ("-g", "full")
   } else {
      ("-g0", "none")
   };
   let given = |field: fn(&Cc) -> &[String]| cc.map(field).unwrap_or_default().to_vec();
   let mut cflags: Vec<String> = dirs(deps, |dep| &dep.include_dirs)
      .into_iter()
      .map(|dir| format!("-isystem{dir}"))
      .collect();
   cflags.extend(
      [
         "-O2",
         debug_flag,
         "-fno-omit-frame-pointer",
         "-mno-omit-leaf-frame-pointer",
      ]
      .map(str::to_owned),
   );
   cflags.extend(harden(Line::Compile));
   cflags.extend(given(|found| &found.cflags));
   let mut cxxflags = harden(Line::Cxx);
   cxxflags.extend(given(|found| &found.cxxflags));
   let mut ldflags: Vec<String> = dirs(deps, |dep| &dep.lib_dirs)
      .into_iter()
      .map(|dir| format!("-L{dir}"))
      .collect();
   ldflags.extend(harden(Line::Link));
   ldflags.extend(given(|found| &found.ldflags));
   let cc_path = find_tool(bins, "cc").ok_or_else(|| io::Error::other("cc not on PATH"))?;
   let root = canonicalize(&cc_path)?
      .parent()
      .and_then(Path::parent)
      .ok_or_else(|| io::Error::other("cc has no installation root"))?
      .to_string_lossy()
      .into_owned();
   let flags = BTreeMap::from([(root, PackageFlags {
      cflags,
      cxxflags,
      ldflags,
   })]);
   set(env, "PKGS_CC", serde_json::to_string(&flags)?);
   set(env, "CARGO_PROFILE_RELEASE_DEBUG", cargo_debug);
   set(env, "CARGO_PROFILE_RELEASE_STRIP", "none");
   Ok(())
}

fn compiler_caches(env: &mut BTreeMap<String, Value>, bins: &[PathBuf]) {
   if let Some(rustc) = find_tool(bins, "rustc") {
      set(env, "RUSTC", rustc.to_string_lossy());
      if let Some(wrapper) = find_tool(bins, "rustcwrap") {
         set(env, "RUSTC_WRAPPER", wrapper.to_string_lossy());
      }
      set(env, "CARGO_INCREMENTAL", "0");
   }
   if find_tool(bins, "go").is_some()
      && let Some(program) = find_tool(bins, "gocacheprog")
   {
      set(env, "GOCACHEPROG", program.to_string_lossy());
   }
}

fn find_tool(bins: &[PathBuf], name: &str) -> Option<PathBuf> {
   bins.iter().map(|bin| bin.join(name)).find(|path| {
      metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
   })
}
