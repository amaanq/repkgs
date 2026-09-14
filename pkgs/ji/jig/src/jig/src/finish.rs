use std::{
   env::{
      set_current_dir,
      var,
   },
   fs::{
      File,
      create_dir_all,
      read_to_string,
      remove_dir_all,
      remove_file,
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
};

use serde_json::{
   Value,
   json,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      Spec,
      build_top,
      flag,
      text,
      words,
   },
   debug,
   exports::{
      Request,
      write_request,
   },
   files::glob,
   fixup,
   implant,
   launchers,
   prepare::succeed,
   tree::{
      note,
      prune,
      relativize,
      shebangs,
   },
};

const CACHED: [&str; 4] = ["cached", "cached-error", "hit", "compile-hit"];

pub fn run(keep_tree: bool) -> io::Result<()> {
   let attrs = Attrs::load()?;
   let spec = attrs.spec()?;
   let top = build_top()?;
   let ctx = Ctx::load(&top)?;
   let out = Path::new(&ctx.out);
   install_map(&spec, out)?;
   if keep_tree {
      let tree = attrs
         .outputs
         .tree
         .as_deref()
         .ok_or_else(|| io::Error::other("no tree output to keep"))?;
      save_tree(tree, &top, ctx.njobs)?;
   }
   if !out.exists() {
      return Err(io::Error::other("nothing was installed into $out"));
   }
   let exe = text(&ctx.platform, "exe");
   for name in bins(&spec, out, exe) {
      if !out.join(format!("bin/{name}{exe}")).exists() {
         return Err(io::Error::other(format!(
            "bin/{name}{exe} missing in output"
         )));
      }
   }
   prune(out)?;
   layout_check(out)?;
   shebangs(out, true)?;
   let debug = attrs
      .outputs
      .debug
      .as_deref()
      .ok_or_else(|| io::Error::other("no debug output"))?;
   create_dir_all(debug)?;
   relocate(&attrs, &spec, &ctx, debug)?;
   write_request(out, &Request {
      name:    spec.name.clone(),
      exports: spec.exports.clone(),
      deps:    ctx.deps.iter().map(|dep| dep.root.clone()).collect(),
   })?;
   let exported: Value = serde_json::from_str(&read_to_string(out.join("exports.json"))?)?;
   note("exports", &serde_json::to_string(&exported)?);
   set_current_dir(&top)?;
   succeed(
      Command::new("chmod").arg("-R").arg("u+w").arg(&ctx.out),
      "chmod",
   )?;
   relativize(&ctx.out, &ctx.dest, text(&ctx.platform, "os") == "macos")?;
   succeed(Command::new("mv").arg(&ctx.out).arg(&ctx.dest), "mv")?;
   version_check(&spec, &ctx, &top)?;
   cache_summary()
}

pub fn mode(keep_tree: bool, tests_only: bool) -> io::Result<()> {
   if tests_only { tests() } else { run(keep_tree) }
}

pub fn tests() -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   let result = var("PKGS_RESULT").map_err(|_unset| io::Error::other("PKGS_RESULT is not set"))?;
   create_dir_all(&result)?;
   let mut rendered = serde_json::to_vec_pretty(&json!({ "package": ctx.out, "passed": true }))?;
   rendered.push(b'\n');
   File::create(Path::new(&result).join("result.json"))?.write_all(&rendered)?;
   note("tests", "passed");
   Ok(())
}

fn install_map(spec: &Spec, out: &Path) -> io::Result<()> {
   for (dest, from) in &spec.install {
      let patterns: Vec<&str> = match *from {
         Value::String(ref one) => vec![one.as_str()],
         Value::Array(ref many) => many.iter().filter_map(Value::as_str).collect(),
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
      let target = out.join(dest);
      if dest.ends_with('/') || matched.len() > 1 {
         create_dir_all(&target)?;
         for source in &matched {
            succeed(Command::new("cp").arg("-rp").arg(source).arg(&target), "cp")?;
         }
      } else {
         create_dir_all(target.parent().unwrap_or(out))?;
         succeed(
            Command::new("cp").arg("-rp").arg(&matched[0]).arg(&target),
            "cp",
         )?;
      }
   }
   for (path, link) in &spec.links {
      let placed = out.join(path);
      create_dir_all(placed.parent().unwrap_or(out))?;
      if symlink_metadata(&placed).is_ok() {
         remove_file(&placed)?;
      }
      symlink(link, &placed)?;
   }
   Ok(())
}

fn save_tree(tree: &str, top: &str, njobs: usize) -> io::Result<()> {
   create_dir_all(tree)?;
   let archive = format!("{tree}/tree.tar.zst");
   echo(&format!(
      "bsdtar -c --zstd --options zstd:threads={njobs} -f {archive} source build"
   ));
   succeed(
      Command::new("bsdtar")
         .args(["-c", "--zstd", "--options"])
         .arg(format!("zstd:threads={njobs}"))
         .arg("-f")
         .arg(&archive)
         .args(["source", "build"])
         .current_dir(top),
      "bsdtar",
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

fn relocate(attrs: &Attrs, spec: &Spec, ctx: &Ctx, debug: &str) -> io::Result<()> {
   let out = Path::new(&ctx.out);
   let platform = &ctx.platform;
   let implanted = spec.prebuilt_is(&Value::Bool(true));
   let ldso = spec.prebuilt_is(&Value::String("ldso".to_owned()));
   if text(platform, "binfmt") == "elf" {
      let interp = Path::new(text(platform, "interp"));
      if spec.debug {
         debug::run(out, Path::new(debug))?;
      }
      if implanted {
         implant::run(
            out,
            interp,
            Some(Path::new(text(platform, "relocStub"))),
            &attrs.dependencies,
         )?;
      }
      launchers::run(
         out,
         Path::new(text(platform, "launch")),
         interp,
         ldso,
         &attrs.dependencies,
      )?;
   }
   let denied: Vec<String> = if flag(platform, "cross") {
      attrs
         .build_dependencies
         .iter()
         .filter(|tool| !attrs.dependencies.contains(tool))
         .cloned()
         .collect()
   } else {
      Vec::new()
   };
   if !ldso {
      let shown: Vec<String> = denied.iter().map(|path| format!("--deny {path}")).collect();
      echo(&format!(
         "reloc-fixup {} --dest {} {}",
         ctx.out,
         ctx.dest,
         shown.join(" ")
      ));
      fixup::run(out, Some(&ctx.dest), &denied)?;
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
fn version_check(spec: &Spec, ctx: &Ctx, top: &str) -> io::Result<()> {
   let platform = &ctx.platform;
   let exe = text(platform, "exe");
   let dest = Path::new(&ctx.dest);
   let bins = bins(spec, dest, exe);
   let line = if spec.tests.version.is_null() {
      Value::Bool(!bins.is_empty())
   } else {
      spec.tests.version.clone()
   };
   if line == Value::Bool(false) || (flag(platform, "cross") && !ctx.tests_run) {
      return Ok(());
   }
   let given: Vec<String> = line.as_str().map_or_else(
      || vec!["--version".to_owned()],
      |command| command.split(' ').map(str::to_owned).collect(),
   );
   let command: Vec<String> = if given.first().is_some_and(|first| bins.contains(first)) {
      given
   } else {
      bins.iter().take(1).cloned().chain(given).collect()
   };
   if command.is_empty() {
      return Ok(());
   }
   let want = spec
      .version
      .rfind("-r")
      .filter(|&at| {
         spec.version[at + 2..]
            .chars()
            .all(|digit| digit.is_ascii_digit())
      })
      .map_or(spec.version.as_str(), |at| &spec.version[..at]);
   let audit = format!("{top}/dlaudit.txt");
   let mut prefix = words(platform, "emulator");
   // via qemu's -E when emulated. The audit libc needs surplus static TLS
   // (librustc_driver), taken from every thread's stack, 64K, tokio workers
   // have 2M
   let dlaudit = text(platform, "dlaudit");
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
      allowed: &spec.tests.dlopen,
   };
   probe.run(dest)?;
   let root = PathBuf::from(format!("{top}/relocated"));
   create_dir_all(&root)?;
   let launch_root = Path::new(text(platform, "launch"))
      .parent()
      .and_then(Path::parent)
      .map(|package| package.to_string_lossy().into_owned())
      .unwrap_or_default();
   let mut linked: Vec<&str> = Vec::new();
   for store_root in ctx
      .roots
      .iter()
      .map(String::as_str)
      .chain([launch_root.as_str()])
   {
      if store_root.is_empty() || linked.contains(&store_root) {
         continue;
      }
      linked.push(store_root);
      let name = Path::new(store_root)
         .file_name()
         .ok_or_else(|| io::Error::other("store root has no name"))?;
      symlink(store_root, root.join(name))?;
   }
   succeed(Command::new("cp").arg("-r").arg(dest).arg(&root), "cp")?;
   let copy = root.join(
      dest
         .file_name()
         .ok_or_else(|| io::Error::other("output has no name"))?,
   );
   probe.run(&copy)?;
   remove_dir_all(&root)?;
   note("version", &format!("{} -> {want}", command.join(" ")));
   Ok(())
}

impl Probe<'_> {
   // empty environment but for HOME, which any real session has (rebar3 crashes
   // without). bzip2 --version goes on to compress stdin, stdout can be
   // binary
   fn run(&self, root: &Path) -> io::Result<()> {
      match remove_file(&self.audit) {
         Ok(()) => {},
         Err(error) if error.kind() == io::ErrorKind::NotFound => {},
         Err(error) => return Err(error),
      }
      let shown = self.command.join(" ");
      let output = Command::new("env")
         .arg("-i")
         .arg(format!("HOME={}", self.top))
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
            String::from_utf8_lossy(&printed)
         )));
      }
      let mut missed: Vec<&str> = Vec::new();
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

struct Run<'log> {
   tool:    &'log str,
   outcome: &'log str,
   subject: &'log str,
}

struct Tally<'log> {
   name:  &'log str,
   count: usize,
}

fn tally<'log>(names: impl Iterator<Item = &'log str>) -> Vec<Tally<'log>> {
   let mut tallies: Vec<Tally<'log>> = Vec::new();
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
   let Ok(log) = var("JIG_LOG") else {
      return Ok(());
   };
   let text = match read_to_string(&log) {
      Ok(text) => text,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
      Err(error) => return Err(error),
   };
   let mut runs: Vec<Run<'_>> = Vec::new();
   for line in text.lines() {
      let mut fields = line.split('\t');
      let (Some(tool), Some(outcome)) = (fields.next(), fields.next()) else {
         continue;
      };
      if outcome != "query" {
         runs.push(Run {
            tool,
            outcome,
            subject: fields.next().unwrap_or_default(),
         });
      }
   }
   let tools = tally(runs.iter().map(|run| run.tool));
   let mut parts = Vec::new();
   for tool in &tools {
      let outcomes = tally(
         runs
            .iter()
            .filter(|run| run.tool == tool.name)
            .map(|run| run.outcome),
      );
      let cached: usize = outcomes
         .iter()
         .filter(|outcome| CACHED.contains(&outcome.name))
         .map(|outcome| outcome.count)
         .sum();
      let mut part = format!(
         "{} cached={cached}/{} ({}%)",
         tool.name,
         tool.count,
         cached * 100 / tool.count.max(1)
      );
      for outcome in outcomes
         .iter()
         .filter(|outcome| !CACHED.contains(&outcome.name))
      {
         part.push(' ');
         part.push_str(outcome.name);
         part.push('=');
         part.push_str(&outcome.count.to_string());
      }
      parts.push(part);
   }
   if !parts.is_empty() {
      note("jig", &parts.join(", "));
   }
   for tool in &tools {
      miss_notes(tool.name, &runs);
   }
   uncached_notes();
   Ok(())
}

fn miss_notes(tool: &str, runs: &[Run<'_>]) {
   let reasons: Vec<&str> = runs
      .iter()
      .filter(|run| {
         run.tool == tool && run.outcome.starts_with("compiled") && run.subject.contains(' ')
      })
      .filter_map(|run| run.subject.rsplit(' ').next())
      .collect();
   if reasons.is_empty() {
      return;
   }
   let kinds = tally(
      reasons
         .iter()
         .map(|reason| reason.split(':').next().unwrap_or(reason)),
   );
   let shown: Vec<String> = kinds
      .iter()
      .map(|kind| format!("{}={}", kind.name, kind.count))
      .collect();
   note(&format!("{tool}-misses"), &shown.join(" "));
   let mut stale = tally(
      reasons
         .iter()
         .filter_map(|reason| reason.strip_prefix("inputs-changed:")),
   );
   stale.sort_by(|left, right| right.count.cmp(&left.count).then(left.name.cmp(right.name)));
   let top: Vec<String> = stale
      .iter()
      .take(5)
      .map(|path| format!("{} ×{}", path.name, path.count))
      .collect();
   if !top.is_empty() {
      note(&format!("{tool}-stale"), &top.join(", "));
   }
}

fn uncached_notes() {
   let Ok(path) = var("JIG_LOG_ARGS") else {
      return;
   };
   let Ok(listed) = read_to_string(path) else {
      return;
   };
   let lines: Vec<&str> = listed.lines().collect();
   let stride = lines.len().div_ceil(5).max(1);
   for line in lines.iter().step_by(stride).take(5) {
      let shown: String = line.chars().take(300).collect();
      note("uncached", &shown);
   }
}
