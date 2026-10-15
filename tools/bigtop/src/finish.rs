use std::{
   collections::BTreeMap,
   env::{
      set_current_dir,
      var,
      vars_os,
   },
   ffi::OsString,
   fs::{
      File,
      create_dir_all,
      read,
      read_link,
      read_to_string,
      remove_dir_all,
      remove_file,
      rename,
      symlink_metadata,
   },
   io::{
      self,
      Write as _,
   },
   os::unix::fs::symlink,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
   time::Instant,
};

use mummer::{
   config::BinaryFormat,
   elf::relative_from,
   files::{
      glob,
      remove_if_exists,
      utf8,
   },
   fixup,
   process::{
      Logged,
      Tool,
      logged,
      loose,
   },
   store::normalize,
   tree::{
      EntryKind,
      find,
      note,
      prune,
      relativize,
      shebangs,
      walk,
   },
};
use serde::Serialize;
use serde_json::Value;

use crate::{
   attrs::{
      Attrs,
      Ctx,
      Platform,
      Prebuilt,
      Spec,
      VersionCheck,
      build_top,
      nix_store,
   },
   debug,
   exports::{
      Request,
      exports_of,
      write_request,
   },
   implant,
   launchers::{
      entries::run as launch,
      pe,
   },
   prepare::succeed,
   system::syslibs,
};

pub fn run(keep_tree: bool) -> io::Result<()> {
   let attrs = Attrs::load()?;
   let spec = attrs.spec()?;
   let top = build_top()?;
   let ctx = Ctx::load(&top)?;
   let platform = ctx.platform()?;
   let exe = platform.ext.exe.as_str();
   let out = ctx.out.as_path();
   let mut laps = Laps::new();
   install_map(&spec, out, exe)?;
   let carvings = carve(&spec, out, &top)?;
   if keep_tree {
      let tree = attrs
         .outputs
         .tree
         .as_deref()
         .ok_or_else(|| io::Error::other("no tree output to keep"))?;
      save_tree(tree, &top)?;
   }
   if !out.exists() {
      return Err(io::Error::other("nothing was installed into $out"));
   }
   for name in bins(&spec, out, exe) {
      if !out.join(format!("bin/{name}{exe}")).exists() {
         return Err(io::Error::other(format!(
            "bin/{name}{exe} missing in output"
         )));
      }
   }
   prune(out)?;
   layout_check(out)?;
   laps.lap("checks");
   syslibs::linked(
      spec.sys.as_deref().unwrap_or_default(),
      &ctx.deps,
      out,
      &ctx.build,
      platform.binfmt,
   )?;
   laps.lap("sys");
   shebangs(out, true)?;
   laps.lap("shebangs");
   let debug = attrs
      .outputs
      .debug
      .as_deref()
      .ok_or_else(|| io::Error::other("no debug output"))?;
   create_dir_all(debug)?;
   relocate(&attrs, &spec, &platform, debug, out, &ctx.dest, &mut laps)?;
   let request = Request {
      name:    spec.name.clone(),
      ext:     platform.ext.clone(),
      exports: spec.exports.clone(),
      deps:    ctx.deps.iter().map(|dep| dep.root.clone()).collect(),
   };
   write_request(out, &request)?;
   let exported = serde_json::from_str::<Value>(&read_to_string(out.join("exports.json"))?)?;
   note("exports", &serde_json::to_string(&exported)?);
   set_current_dir(&top)?;
   succeed(Command::new("chmod").arg("-R").arg("u+w").arg(&ctx.out))?;
   relativize(utf8(&ctx.out)?, utf8(&ctx.dest)?)?;
   laps.lap("relativize");

   if platform.os == "windows" {
      succeed(Command::new("cp").arg("-rL").arg(&ctx.out).arg(&ctx.dest))?;
      remove_dir_all(&ctx.out)?;
   } else {
      succeed(Command::new("mv").arg(&ctx.out).arg(&ctx.dest))?;
   }

   laps.lap("move");

   let placing = Placing {
      attrs: &attrs,
      spec: &spec,
      platform: &platform,
      debug,
   };

   if let Some(staging) = ctx.dev.as_deref() {
      let dest = attrs
         .outputs
         .dev
         .as_deref()
         .ok_or_else(|| io::Error::other("no dev output"))?;

      if !staging.exists() {
         return Err(io::Error::other("nothing was installed into (act).dev"));
      }
      placing.place(staging, dest, &mut laps)?;
      laps.lap("dev");
   }

   for carving in carvings {
      placing.carved(&request, &ctx.dest, carving, &mut laps)?;
      laps.lap("outputs");
   }

   version_check(&spec, &ctx, &platform, &top)?;
   laps.lap("version");
   note("finish-steps", &laps.shown.join(", "));
   cache_summary()
}

/// What relocating an output staged apart from `out` needs.
struct Placing<'build> {
   attrs:    &'build Attrs,
   spec:     &'build Spec,
   platform: &'build Platform,
   debug:    &'build str,
}

impl Placing<'_> {
   /// Relocates `staging` for `dest` and moves it there.
   fn place(&self, staging: &Path, dest: &str, laps: &mut Laps) -> io::Result<()> {
      prune(staging)?;
      shebangs(staging, true)?;
      relocate(
         self.attrs,
         self.spec,
         self.platform,
         self.debug,
         staging,
         Path::new(dest),
         laps,
      )?;
      succeed(Command::new("chmod").arg("-R").arg("u+w").arg(staging))?;
      relativize(utf8(staging)?, dest)?;
      succeed(Command::new("mv").arg(staging).arg(dest))
   }

   /// Places a carved output and links `out`'s paths to it.
   fn carved(
      &self,
      request: &Request,
      out: &Path,
      carving: Carving,
      laps: &mut Laps,
   ) -> io::Result<()> {
      let dest = self
         .attrs
         .outputs
         .carved
         .get(&carving.name)
         .ok_or_else(|| io::Error::other(format!("no {} output", carving.name)))?;
      write_request(&carving.staging, request)?;
      self.place(&carving.staging, dest, laps)?;

      if let Some(file) = naming(Path::new(dest), out)? {
         return Err(io::Error::other(format!(
            "outputs.{}: {file} refers to out, which links back to it",
            carving.name
         )));
      }

      for relative in carving.moved {
         let link = out.join(&relative);
         let directory = link.parent().unwrap_or(out);
         symlink(
            relative_from(directory, &Path::new(dest).join(&relative)),
            &link,
         )?;
      }
      Ok(())
   }
}

/// An output a recipe's `outputs` names, its files moved out of `out`.
struct Carving {
   name:    String,
   staging: PathBuf,
   moved:   Vec<PathBuf>,
}

fn carve(spec: &Spec, out: &Path, top: &str) -> io::Result<Vec<Carving>> {
   let mut carvings = Vec::new();
   for (name, carved) in &spec.outputs {
      let staging = Path::new(top).join("outputs").join(name);
      let mut moved = Vec::new();

      for pattern in &carved.files {
         let found = glob(utf8(&out.join(pattern))?)?;
         if found.is_empty() {
            return Err(io::Error::other(format!(
               "outputs.{name}: nothing matches {pattern}"
            )));
         }
         for path in found {
            let relative = path
               .strip_prefix(out)
               .map_err(|_outside| io::Error::other(format!("{} is outside out", path.display())))?
               .to_path_buf();
            let target = staging.join(&relative);

            create_dir_all(target.parent().unwrap_or(&staging))?;
            rename(&path, &target)?;
            moved.push(relative);
         }
      }

      carvings.push(Carving {
         name: name.clone(),
         staging,
         moved,
      });
   }
   Ok(carvings)
}

/// The first file under `root` naming `other`'s store path by its hash.
fn naming(root: &Path, other: &Path) -> io::Result<Option<String>> {
   let hash = other
      .file_name()
      .and_then(|name| name.to_str())
      .and_then(|name| name.split_once('-'))
      .map(|pair| pair.0)
      .ok_or_else(|| io::Error::other(format!("{} is no store path", other.display())))?;

   for entry in walk(root)? {
      let named = match entry.kind {
         EntryKind::File => find(&read(&entry.path)?, hash.as_bytes()).is_some(),
         EntryKind::Link => {
            read_link(&entry.path)?
               .as_os_str()
               .as_encoded_bytes()
               .windows(hash.len())
               .any(|window| window == hash.as_bytes())
         },
         EntryKind::Dir => false,
      };
      if named {
         return Ok(Some(entry.rel));
      }
   }
   Ok(None)
}

/// How long each step of finish took, which can pass a minute on large
/// outputs.
struct Laps {
   last:  Instant,
   shown: Vec<String>,
}

impl Laps {
   fn new() -> Self {
      Self {
         last:  Instant::now(),
         shown: Vec::new(),
      }
   }

   fn lap(&mut self, name: &str) {
      let millis = self.last.elapsed().as_millis();
      self
         .shown
         .push(format!("{name} {}.{}s", millis / 1000, millis % 1000 / 100));
      self.last = Instant::now();
   }
}

pub fn mode(keep_tree: bool, tests_only: bool) -> io::Result<()> {
   if tests_only { tests() } else { run(keep_tree) }
}

#[derive(Serialize)]
struct Passed<'ctx> {
   package: &'ctx str,
   passed:  bool,
}

pub fn tests() -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   let result = ctx.dest.as_path();
   create_dir_all(result)?;
   let mut rendered = serde_json::to_vec_pretty(&Passed {
      package: utf8(&ctx.out)?,
      passed:  true,
   })?;
   rendered.push(b'\n');
   File::create(result.join("result.json"))?.write_all(&rendered)?;
   note("tests", "passed");
   Ok(())
}

fn install_map(spec: &Spec, out: &Path, exe: &str) -> io::Result<()> {
   for (dest, from) in &spec.install {
      let patterns = match *from {
         Value::String(ref one) => vec![one.as_str()],
         Value::Array(ref many) => many.iter().filter_map(Value::as_str).collect::<Vec<_>>(),
         Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => {
            return Err(io::Error::other(format!(
               "install {dest}: expected a glob or a list of globs"
            )));
         },
      };
      let mut matched = Vec::new();
      for pattern in &patterns {
         matched.extend(glob(pattern)?);
      }
      if matched.is_empty() {
         return Err(io::Error::other(format!(
            "install {dest}: nothing matches {}",
            patterns.join(" ")
         )));
      }
      let target = normalize(&out.join(dest));
      if dest.ends_with('/') || matched.len() > 1 {
         create_dir_all(&target)?;
         for source in &matched {
            succeed(Command::new("cp").arg("-rp").arg(source).arg(&target))?;
         }
      } else {
         create_dir_all(target.parent().unwrap_or(out))?;
         succeed(Command::new("cp").arg("-rp").arg(&matched[0]).arg(&target))?;
      }
   }
   for (path, link) in &spec.links {
      let suffix = if path.starts_with("bin/") { exe } else { "" };
      let placed = out.join(format!("{path}{suffix}"));
      create_dir_all(placed.parent().unwrap_or(out))?;
      if symlink_metadata(&placed).is_ok() {
         remove_file(&placed)?;
      }
      symlink(format!("{link}{suffix}"), &placed)?;
   }
   for pattern in &spec.remove {
      let found = glob(utf8(&out.join(pattern))?)?;
      if found.is_empty() {
         return Err(io::Error::other(format!(
            "remove {pattern}: nothing matches"
         )));
      }
      for path in found {
         if symlink_metadata(&path)?.is_dir() {
            remove_dir_all(&path)?;
         } else {
            remove_file(&path)?;
         }
      }
   }
   Ok(())
}

fn save_tree(tree: &str, top: &str) -> io::Result<()> {
   create_dir_all(tree)?;
   echo(&format!("cp -a source build {tree}"));

   succeed(
      Command::new("cp")
         .args(["-a", "source", "build", tree])
         .current_dir(top),
   )
}

fn echo(line: &str) {
   drop(writeln!(io::stderr().lock(), "+ {line}"));
}

fn bins(spec: &Spec, out: &Path, exe: &str) -> Vec<String> {
   match spec.bin {
      Some(ref listed) => listed.clone(),
      None if out.join(format!("bin/{}{exe}", spec.name)).exists() => vec![spec.name.clone()],
      None => Vec::new(),
   }
}

fn layout_check(out: &Path) -> io::Result<()> {
   for directory in ["lib64", "sbin"] {
      if out.join(directory).exists() {
         return Err(io::Error::other(format!(
            "{directory}/ in output: configure with --libdir/--sbindir so it installs into lib/ \
             and bin/"
         )));
      }
   }
   Ok(())
}

fn relocate(
   attrs: &Attrs,
   spec: &Spec,
   platform: &Platform,
   debug: &str,
   out: &Path,
   dest: &Path,
   laps: &mut Laps,
) -> io::Result<()> {
   let implanted = spec.prebuilt == Prebuilt::Implanted;
   let ldso = spec.prebuilt == Prebuilt::Ldso;
   match platform.binfmt {
      BinaryFormat::Elf => {
         let interp = Path::new(&platform.interp);
         if spec.debug {
            debug::run(out, Path::new(debug))?;
         }
         laps.lap("debug");
         if implanted {
            implant::run(
               out,
               interp,
               Some(Path::new(&platform.reloc_stub)),
               &attrs.dependencies,
            )?;
         }
         launch(
            out,
            Path::new(&platform.launch),
            interp,
            ldso,
            &attrs.dependencies,
         )?;
         laps.lap("relocate");
      },
      BinaryFormat::MachO => {
         if spec.debug {
            debug::macho(out, Path::new(debug))?;
         }
      },
      BinaryFormat::Coff => {
         debug::strip_archives(&walk(out)?, &["lib", "obj", "a"])?;
         pe::run(
            out,
            Path::new(&platform.launch),
            Path::new(&platform.sysroot),
            &attrs.dependencies,
         )?;
      },
   }
   let denied = if platform.cross {
      attrs
         .build_dependencies
         .iter()
         .filter(|tool| !attrs.dependencies.contains(tool))
         .cloned()
         .collect::<Vec<String>>()
   } else {
      Vec::new()
   };
   if !ldso {
      let sdk = (platform.binfmt == BinaryFormat::MachO).then(|| Path::new(&platform.sysroot));
      let mut shown = denied
         .iter()
         .map(|path| format!("--deny {path}"))
         .collect::<Vec<String>>();

      if let Some(path) = sdk {
         shown.push(format!("--sdk {}", path.display()));
      }
      let loader = Some(platform.interp.as_str())
         .filter(|path| !path.is_empty())
         .map(Path::new);
      if let Some(path) = loader {
         shown.push(format!("--loader {}", path.display()));
      }
      let mut libraries = Vec::new();
      if sdk.is_some() {
         for root in attrs.dependencies.iter().chain(&attrs.build_dependencies) {
            let library = Path::new(root).join("lib");
            if library.is_dir() {
               shown.push(format!("--library {}", library.display()));
               libraries.push(library);
            }
         }
      }

      echo(&format!(
         "reloc-fixup {} --dest {} {}",
         out.display(),
         dest.display(),
         shown.join(" ")
      ));
      fixup::run(out, Some(dest), sdk, loader, &denied, &libraries)?;
   }
   Ok(())
}

struct Probe<'check> {
   command: &'check [String],
   want:    &'check str,
   exe:     &'check str,
   top:     &'check str,
   prefix:  Vec<String>,
   audit:   String,
   allowed: &'check [String],
}

// run in place and from a copy of the closure under another root, where an
// absolute store path would show
fn version_check(spec: &Spec, ctx: &Ctx, platform: &Platform, top: &str) -> io::Result<()> {
   let exe = platform.ext.exe.as_str();
   let dest = ctx.dest.as_path();
   let bins = bins(spec, dest, exe);
   let line = spec
      .tests
      .version
      .clone()
      .unwrap_or(VersionCheck::Enabled(!bins.is_empty()));
   let emulator = platform.emulator.clone();

   if line == VersionCheck::Enabled(false)
      || (platform.cross && !ctx.tests_run && (platform.os != "windows" || emulator.is_empty()))
   {
      return Ok(());
   }

   let given = match line {
      VersionCheck::Enabled(_) => vec!["--version".to_owned()],
      VersionCheck::Command(ref command) => {
         command.split(' ').map(str::to_owned).collect::<Vec<_>>()
      },
   };
   let command = if given.first().is_some_and(|first| bins.contains(first)) {
      given
   } else {
      bins
         .iter()
         .take(1)
         .cloned()
         .chain(given)
         .collect::<Vec<_>>()
   };
   if command.is_empty() {
      return Ok(());
   }
   let version = spec
      .version
      .split_once("-unstable-")
      .map_or(spec.version.as_str(), |cut| cut.0);
   let want = version
      .rfind("-r")
      .filter(|&at| {
         version[at + 2..]
            .chars()
            .all(|digit| digit.is_ascii_digit())
      })
      .map_or(version, |at| &version[..at]);
   let audit = format!("{top}/dlaudit.txt");
   let mut prefix = emulator;
   // via qemu's -E when emulated. The audit libc needs surplus static TLS
   // (librustc_driver), taken from every thread's stack, 64K, tokio workers
   // have 2M
   let dlaudit = &platform.dlaudit;
   if !dlaudit.is_empty() {
      let emulated = !prefix.is_empty();
      for setting in [
         format!("LD_AUDIT={dlaudit}"),
         format!("DLAUDIT_OUT={audit}"),
         "GLIBC_TUNABLES=glibc.rtld.optional_static_tls=0x10000".to_owned(),
      ] {
         if emulated {
            prefix.push("-E".to_owned());
         }
         prefix.push(setting);
      }
   }
   let probe = Probe {
      command: &command,
      want,
      exe,
      top,
      prefix,
      audit,
      allowed: spec.tests.dlopen.as_deref().unwrap_or_default(),
   };
   probe.run(dest)?;
   probe.relocated(ctx, &platform.launch)?;
   note("version", &format!("{} -> {want}", command.join(" ")));
   Ok(())
}

impl Probe<'_> {
   fn relocated(&self, ctx: &Ctx, launch: &str) -> io::Result<()> {
      let dest = ctx.dest.as_path();
      let root = PathBuf::from(format!("{}/relocated", self.top));
      create_dir_all(&root)?;
      let launch_root = Path::new(launch)
         .parent()
         .and_then(Path::parent)
         .map(|package| utf8(package).map(str::to_owned))
         .transpose()?
         .unwrap_or_default();
      let store = PathBuf::from(nix_store());
      let dll_roots = exports_of(dest)?
         .dll_dirs
         .iter()
         .filter_map(|directory| {
            let name = Path::new(directory)
               .strip_prefix(&store)
               .ok()?
               .components()
               .next()?;
            store.join(name).to_str().map(str::to_owned)
         })
         .collect::<Vec<String>>();

      let mut linked = Vec::<&str>::new();
      for store_root in ctx
         .roots
         .iter()
         .map(String::as_str)
         .chain([launch_root.as_str()])
         .chain(dll_roots.iter().map(String::as_str))
      {
         if store_root.is_empty() || linked.contains(&store_root) {
            continue;
         }
         linked.push(store_root);
         let name = Path::new(store_root)
            .file_name()
            .ok_or_else(|| io::Error::other("store root has no name"))?;
         let sibling = root.join(name);

         if symlink_metadata(&sibling).is_ok() {
            remove_file(&sibling)?;
         }

         symlink(store_root, sibling)?;
      }
      succeed(Command::new("cp").arg("-r").arg(dest).arg(&root))?;
      let copy = root.join(
         dest
            .file_name()
            .ok_or_else(|| io::Error::other("output has no name"))?,
      );
      self.run(&copy)?;
      remove_dir_all(&root)?;
      Ok(())
   }
   // HOME is needed by rebar3, WINE and QEMU_ settings by emulators.
   // bzip2 --version goes on to compress stdin, stdout can be binary
   fn run(&self, root: &Path) -> io::Result<()> {
      remove_if_exists(Path::new(&self.audit))?;
      let shown = self.command.join(" ");
      let emulator_env = vars_os()
         .filter(|entry| {
            entry
               .0
               .to_str()
               .is_some_and(|text| text.starts_with("WINE") || text.starts_with("QEMU_"))
         })
         .map(|(mut name, value)| {
            name.push("=");
            name.push(value);
            name
         })
         .collect::<Vec<OsString>>();

      let output = Command::new("env")
         .arg("-i")
         .arg(format!("HOME={}", self.top))
         .args(emulator_env)
         .args(&self.prefix)
         .arg(root.join(format!("bin/{}{}", self.command[0], self.exe)))
         .args(&self.command[1..])
         .current_dir("/")
         .output()?;
      let mut printed = output.stdout.clone();
      printed.extend_from_slice(&output.stderr);
      let found = printed
         .windows(self.want.len())
         .any(|window| window == self.want.as_bytes());
      if !output.status.success() || !found {
         return Err(io::Error::other(format!(
            "version check: `{shown}` did not print {} (exit {})\n{}",
            self.want,
            output.status.code().unwrap_or(-1),
            loose(&printed)
         )));
      }
      let mut missed = Vec::<&str>::new();
      let listed = match read_to_string(&self.audit) {
         Ok(listed) => listed,
         Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
         Err(error) => return Err(error),
      };
      for library in listed.lines() {
         if !missed.contains(&library) && !self.allowed.iter().any(|name| name == library) {
            missed.push(library);
         }
      }
      if !missed.is_empty() {
         return Err(io::Error::other(format!(
            "version check: `{shown}` dlopens {}, not in the closure (a missing dependency, or \
             tests.dlopen = [names] if optional)",
            missed.join(", ")
         )));
      }
      Ok(())
   }
}

struct Tally<Name> {
   name:  Name,
   count: usize,
}

fn tally<Name>(names: impl Iterator<Item = Name>) -> Vec<Tally<Name>>
where
   Name: PartialEq,
{
   let mut tallies = Vec::<Tally<Name>>::new();
   for name in names {
      match tallies.iter_mut().find(|tally| tally.name == name) {
         Some(tally) => tally.count += 1,
         None => {
            tallies.push(Tally { name, count: 1 });
         },
      }
   }
   tallies
}

fn cache_summary() -> io::Result<()> {
   let Ok(log) = var("MUMMER_LOG") else {
      return Ok(());
   };
   let text = match read_to_string(&log) {
      Ok(text) => text,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
      Err(error) => return Err(error),
   };
   let runs = logged(&text).collect::<Vec<_>>();
   let tools = tally(runs.iter().map(|run| run.tool));
   let mut parts = Vec::new();
   for tool in &tools {
      let outcomes = tally(
         runs
            .iter()
            .filter(|run| run.tool == tool.name)
            .map(|run| run.outcome),
      );
      let cached = outcomes
         .iter()
         .filter(|outcome| outcome.name.is_cached())
         .map(|outcome| outcome.count)
         .sum::<usize>();
      let mut part = format!(
         "{} cached={cached}/{} ({}%)",
         tool.name,
         tool.count,
         cached * 100 / tool.count.max(1)
      );
      for outcome in outcomes.iter().filter(|outcome| !outcome.name.is_cached()) {
         part.push(' ');
         part.push_str(outcome.name.name());
         part.push('=');
         part.push_str(&outcome.count.to_string());
      }
      parts.push(part);
   }
   if !parts.is_empty() {
      note("mummer", &parts.join(", "));
   }
   let mut spent = BTreeMap::<(String, &str), (u64, usize)>::new();
   for run in &runs {
      let entry = spent
         .entry((run.tool.to_string(), run.outcome.name()))
         .or_insert((0, 0));
      entry.0 += run.millis;
      entry.1 += 1;
   }
   if !spent.is_empty() {
      let shown = spent
         .iter()
         .map(|(&(ref tool, outcome), &(millis, count))| {
            format!(
               "{tool} {outcome} {}.{}s/{count}",
               millis / 1000,
               millis % 1000 / 100
            )
         })
         .collect::<Vec<String>>();
      note("mummer-time", &shown.join(", "));
   }
   for tool in &tools {
      miss_notes(tool.name, &runs);
   }
   uncached_notes();
   Ok(())
}

const MISSED_SHOWN: usize = 300;

fn miss_notes(tool: Tool, runs: &[Logged<'_>]) {
   // every call the cache did not answer, so a warm run accounts for each one
   let missed = runs
      .iter()
      .filter(|run| run.tool == tool && !run.outcome.is_cached())
      .collect::<Vec<_>>();
   for run in missed.iter().take(MISSED_SHOWN) {
      note(
         &format!("{tool}-missed"),
         &format!(
            "{} {}",
            run.outcome.name(),
            run.subject.chars().take(240).collect::<String>()
         ),
      );
   }
   if missed.len() > MISSED_SHOWN {
      note(
         &format!("{tool}-missed"),
         &format!("{} more", missed.len() - MISSED_SHOWN),
      );
   }
   let reasons = runs
      .iter()
      .filter(|run| run.tool == tool && run.outcome.is_compiled() && run.subject.contains(' '))
      .filter_map(|run| run.subject.rsplit(' ').next())
      .collect::<Vec<&str>>();
   if reasons.is_empty() {
      return;
   }
   let kinds = tally(
      reasons
         .iter()
         .map(|reason| reason.split(':').next().unwrap_or(reason)),
   );
   let shown = kinds
      .iter()
      .map(|kind| format!("{}={}", kind.name, kind.count))
      .collect::<Vec<String>>();
   note(&format!("{tool}-misses"), &shown.join(" "));
   let mut stale = tally(
      reasons
         .iter()
         .filter_map(|reason| reason.strip_prefix("inputs-changed:")),
   );
   stale.sort_by(|left, right| right.count.cmp(&left.count).then(left.name.cmp(right.name)));
   let top = stale
      .iter()
      .take(5)
      .map(|path| format!("{} ×{}", path.name, path.count))
      .collect::<Vec<String>>();
   if !top.is_empty() {
      note(&format!("{tool}-stale"), &top.join(", "));
   }
}

fn uncached_notes() {
   let Ok(path) = var("MUMMER_LOG_ARGS") else {
      return;
   };
   let Ok(listed) = read_to_string(path) else {
      return;
   };
   let lines = listed.lines().collect::<Vec<&str>>();
   let stride = lines.len().div_ceil(5).max(1);
   for line in lines.iter().step_by(stride).take(5) {
      let shown = line.chars().take(300).collect::<String>();
      note("uncached", &shown);
   }
}
