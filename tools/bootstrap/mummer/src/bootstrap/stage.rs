use std::{
   env::var,
   fmt::Display,
   fs::{
      Metadata,
      create_dir_all,
      metadata,
      read,
      read_to_string,
   },
   io::{
      self,
      Write as _,
   },
   num::NonZero,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
   thread::available_parallelism,
};

use mummer::{
   attrs::load,
   config::BinaryFormat,
   files::{
      glob,
      remove_if_exists,
      write_file,
   },
   process::Checked as _,
   store::on_path,
   tree::{
      note,
      parallel_with,
   },
};
use serde::{
   Deserialize,
   de::IgnoredAny,
};

use crate::bootstrap::target::{
   Cpu,
   Libc,
   Os,
   Profile,
};

#[derive(Deserialize)]
struct Outputs {
   out: PathBuf,
}

/// The structured attrs every stage has, see bootstrap/stage.nix.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Facts {
   outputs:      Outputs,
   clang_target: String,
   build_triple: String,
   cpu:          Cpu,
   libc:         Libc,
   os:           Os,
   binfmt:       BinaryFormat,
   interp:       String,
   karch:        String,
   flags:        String,
   llvm:         PathBuf,
   #[serde(default)]
   src:          Option<PathBuf>,
   #[serde(default)]
   patches:      Vec<PathBuf>,
   #[serde(default)]
   sysroot:      Option<PathBuf>,
}

/// One bootstrap stage, what every recipe runs against. Recipe inputs beyond
/// these are each recipe's own attrs.
pub struct Stage {
   pub out:          PathBuf,
   pub clang_target: String,
   pub build_triple: String,
   pub cpu:          Cpu,
   pub libc:         Libc,
   pub os:           Os,
   pub binfmt:       BinaryFormat,
   pub profile:      Profile,
   pub interp:       String,
   pub karch:        String,
   pub llvm:         PathBuf,
   pub sysroot:      Option<PathBuf>,
   pub build_top:    PathBuf,
   pub flags:        Vec<String>,
   pub src:          Option<PathBuf>,
   pub patches:      Vec<PathBuf>,
}

/// What a libc recipe run twice is given, `headersOnly` on the first run and
/// compiler-rt on the second.
#[derive(Deserialize)]
pub struct LibcAttrs {
   #[serde(default, rename = "headersOnly")]
   headers_only: Option<IgnoredAny>,
   #[serde(default, rename = "compiler-rt")]
   compiler_rt:  Option<PathBuf>,
}

impl LibcAttrs {
   pub const fn headers_only(&self) -> bool {
      self.headers_only.is_some()
   }

   pub fn compiler_rt(&self) -> Option<&Path> {
      self.compiler_rt.as_deref()
   }

   pub fn required_compiler_rt(&self) -> io::Result<&Path> {
      self
         .compiler_rt()
         .ok_or_else(|| io::Error::other("this stage has no compiler-rt"))
   }
}

impl Stage {
   pub fn load() -> io::Result<Self> {
      let facts = load::<Facts>()?;
      Ok(Self {
         profile:      Profile::of(facts.binfmt, facts.libc, facts.cpu)?,
         out:          facts.outputs.out,
         clang_target: facts.clang_target,
         build_triple: facts.build_triple,
         cpu:          facts.cpu,
         libc:         facts.libc,
         os:           facts.os,
         binfmt:       facts.binfmt,
         interp:       facts.interp,
         karch:        facts.karch,
         llvm:         facts.llvm,
         sysroot:      facts.sysroot,
         build_top:    var("NIX_BUILD_TOP")
            .map_err(|_unset| io::Error::other("NIX_BUILD_TOP is not set"))?
            .into(),
         flags:        facts.flags.split_whitespace().map(str::to_owned).collect(),
         src:          facts.src,
         patches:      facts.patches,
      })
   }

   pub fn src(&self) -> io::Result<&Path> {
      self
         .src
         .as_deref()
         .ok_or_else(|| io::Error::other("this stage has no src"))
   }

   pub fn sysroot(&self) -> io::Result<&Path> {
      self
         .sysroot
         .as_deref()
         .ok_or_else(|| io::Error::other("this stage has no sysroot"))
   }

   /// Where clang looks for the compiler-rt library `name`.
   pub fn runtime(&self, name: &str) -> String {
      self.profile.runtime(&self.clang_target, name)
   }

   /// --target plus the platform's -march and hardening flags.
   pub fn target(&self) -> Vec<String> {
      let mut args = vec![format!("--target={}", self.clang_target)];
      args.extend_from_slice(&self.flags);
      args
   }

   /// `--host` and `--build` for configure.
   pub fn host_build(&self) -> [String; 2] {
      [
         format!("--host={}", self.clang_target),
         format!("--build={}", self.build_triple),
      ]
   }

   /// compile and link against the sysroot with the raw peanut clang, which
   /// defaults to libunwind, built last. Plain C needs no unwinder.
   pub fn ccflags(&self) -> io::Result<Vec<String>> {
      let sysroot = self.sysroot()?.display();
      let mut args = self.target();
      args.extend([
         format!("--sysroot={sysroot}"),
         format!("-resource-dir={sysroot}/lib/clang"),
         "-rtlib=compiler-rt".to_owned(),
         "-unwindlib=none".to_owned(),
         "-fuse-ld=lld".to_owned(),
      ]);
      Ok(args)
   }

   /// The raw clang as one CC word list for configure, against compiler-rt at
   /// `resource`, with `before` ahead of the link flags and `linked` among
   /// them.
   pub fn raw_cc(&self, resource: &Path, before: &[String], linked: &[String]) -> String {
      let mut words = vec!["clang".to_owned()];
      words.extend(self.target());
      words.push(format!("-resource-dir={}", resource.display()));
      words.extend_from_slice(before);
      words.extend(
         [
            "--start-no-unused-arguments",
            "-rtlib=compiler-rt",
            "-unwindlib=none",
            "-fuse-ld=lld",
         ]
         .map(str::to_owned),
      );
      words.extend_from_slice(linked);
      words.push("--end-no-unused-arguments".to_owned());
      words.join(" ")
   }

   /// a writable copy of the unpacked source under the build top with the
   /// stage's patches applied, `only` limits it to subdirectories. -p keeps
   /// the store's uniform mtimes so generated files stay newer than their
   /// inputs for make.
   pub fn unpack(&self, name: &str, only: &[&str]) -> io::Result<PathBuf> {
      let src = self.src()?;
      let dest = self.build_top.join(name);
      create_dir_all(&dest)?;
      if only.is_empty() {
         execute(
            Command::new("cp")
               .arg("-rp")
               .arg(format!("{}/.", src.display()))
               .arg(&dest),
         )?;
      }
      for sub in only {
         let target = dest.join(sub);
         if let Some(parent) = target.parent() {
            create_dir_all(parent)?;
         }
         execute(
            Command::new("cp")
               .arg("-rp")
               .arg(src.join(sub))
               .arg(&target),
         )?;
      }
      execute(Command::new("chmod").args(["-R", "u+w"]).arg(&dest))?;
      for patch in &self.patches {
         execute(
            Command::new("patch")
               .args(["-p1", "-i"])
               .arg(patch)
               .current_dir(&dest),
         )?;
      }
      Ok(dest)
   }
}

pub fn say(message: &str) {
   let (kind, detail) = message.split_once(": ").unwrap_or(("bootstrap", message));
   note(kind, detail);
}

/// the parallelism Nix granted, 0 meaning every cpu.
pub fn cores() -> usize {
   let asked = var("NIX_BUILD_CORES")
      .ok()
      .and_then(|text| text.parse::<usize>().ok())
      .unwrap_or(1);
   if asked == 0 {
      available_parallelism().map_or(1, NonZero::get)
   } else {
      asked
   }
}

/// Echoes `command` as the build log shows it, then runs it to success.
pub fn execute(command: &mut Command) -> io::Result<()> {
   let words = command
      .get_args()
      .map(|arg| arg.to_string_lossy())
      .collect::<Vec<_>>();
   drop(writeln!(
      io::stderr().lock(),
      "+ {} {}",
      command.get_program().to_string_lossy(),
      words.join(" ")
   ));
   command.checked()
}

pub fn run(program: &str, args: &[&str]) -> io::Result<()> {
   execute(Command::new(program).args(args))
}

/// What `command` prints, run without echoing.
pub fn capture(command: &mut Command) -> io::Result<String> {
   Ok(String::from_utf8_lossy(&command.checked_output()?).into_owned())
}

/// `sh <script> --prefix=<prefix>` in `build`.
pub fn configure<Prefix>(build: &Path, script: &Path, prefix: Prefix) -> Command
where
   Prefix: Display,
{
   let mut command = Command::new("sh");
   command
      .arg(script)
      .arg(format!("--prefix={prefix}"))
      .current_dir(build);
   command
}

/// `make -j <cores> SHELL=<sh>` in `directory`.
pub fn make_parallel(directory: &Path) -> io::Result<Command> {
   let mut command = Command::new("make");
   command
      .arg("-j")
      .arg(cores().to_string())
      .arg(format!("SHELL={}", tool("sh")?.display()))
      .current_dir(directory);
   Ok(command)
}

/// `make <target> SHELL=<sh>` in `directory`.
pub fn make_target(directory: &Path, target: &str) -> io::Result<Command> {
   let mut command = Command::new("make");
   command
      .arg(target)
      .arg(format!("SHELL={}", tool("sh")?.display()))
      .current_dir(directory);
   Ok(command)
}

/// the absolute path of a tool on PATH, configure scripts write it into #!
/// lines.
pub fn tool(name: &str) -> io::Result<PathBuf> {
   let path = on_path(name);
   if !path.is_absolute() {
      return Err(io::Error::other(format!("{name} not on PATH")));
   }
   Ok(path)
}

/// what the pattern matches with metadata that `keeps`, sorted as text, so
/// archive member order does not depend on directory order.
fn matching(pattern: &str, keeps: fn(&Metadata) -> bool) -> io::Result<Vec<PathBuf>> {
   let mut found = glob(pattern)?
      .into_iter()
      .filter(|path| metadata(path).is_ok_and(|meta| keeps(&meta)))
      .collect::<Vec<_>>();
   found.sort_by_key(|path| path.to_string_lossy().into_owned());
   Ok(found)
}

pub fn files(pattern: &str) -> io::Result<Vec<PathBuf>> {
   matching(pattern, Metadata::is_file)
}

pub fn dirs(pattern: &str) -> io::Result<Vec<PathBuf>> {
   matching(pattern, Metadata::is_dir)
}

/// copy the files under `from` matching `pattern` to `to`, keeping relative
/// paths and dropping the store's modes.
pub fn copy_tree(from: &Path, to: &Path, pattern: &str) -> io::Result<()> {
   let pairs = files(&format!("{}/{pattern}", from.display()))?
      .into_iter()
      .map(|source| {
         let relative = source
            .strip_prefix(from)
            .map_err(io::Error::other)?
            .to_path_buf();
         Ok((source, to.join(relative)))
      })
      .collect::<io::Result<Vec<_>>>()?;
   for pair in &pairs {
      if let Some(parent) = pair.1.parent() {
         create_dir_all(parent)?;
      }
   }
   parallel_with(&pairs, 4, |pair| write_file(&pair.1, &read(&pair.0)?, None))?;
   Ok(())
}

pub fn read_list(file: &Path) -> io::Result<Vec<String>> {
   Ok(read_to_string(file)?
      .lines()
      .filter(|line| !line.is_empty())
      .map(str::to_owned)
      .collect())
}

/// what a libc tells the toolchain steps after it, as files under etc/cc/.
pub fn cc_facts(out: &Path, facts: &[(&str, &[&str])]) -> io::Result<()> {
   let dir = out.join("etc/cc");
   create_dir_all(&dir)?;
   for &(name, lines) in facts {
      let mut text = lines.join("\n");
      text.push('\n');
      write_file(&dir.join(name), text.as_bytes(), None)?;
   }
   Ok(())
}

pub fn cc_fact(libc: &Path, name: &str) -> io::Result<Option<Vec<String>>> {
   let file = libc.join("etc/cc").join(name);
   if !file.exists() {
      return Ok(None);
   }
   read_list(&file).map(Some)
}

pub struct Item {
   pub src:   PathBuf,
   pub obj:   PathBuf,
   pub flags: Vec<String>,
}

/// parallel compile with `compiler`, the object paths back.
pub fn compile(compiler: &str, common: &[String], items: &[Item]) -> io::Result<Vec<PathBuf>> {
   let failures = parallel_with(items, cores(), |item| {
      if let Some(parent) = item.obj.parent() {
         create_dir_all(parent)?;
      }
      let output = Command::new(compiler)
         .args(common)
         .args(&item.flags)
         .arg("-c")
         .arg(&item.src)
         .arg("-o")
         .arg(&item.obj)
         .output()?;
      if output.status.success() {
         return Ok(None);
      }
      Ok(Some((
         item.src.clone(),
         String::from_utf8_lossy(&output.stderr).into_owned(),
      )))
   })?;
   let failed = failures.iter().flatten().collect::<Vec<_>>();
   if !failed.is_empty() {
      for failure in failed.iter().take(3) {
         drop(writeln!(
            io::stderr().lock(),
            "--- {}\n{}",
            failure.0.display(),
            failure.1
         ));
      }
      return Err(io::Error::other(format!(
         "{} of {} compiles failed",
         failed.len(),
         items.len()
      )));
   }
   Ok(items.iter().map(|item| item.obj.clone()).collect())
}

/// One path per line, for a linker or archiver `@file`.
pub fn write_rsp(path: &Path, objs: &[PathBuf]) -> io::Result<()> {
   let mut text = objs
      .iter()
      .map(|obj| obj.to_string_lossy())
      .collect::<Vec<_>>()
      .join("\n");
   text.push('\n');
   write_file(path, text.as_bytes(), None)
}

/// a static archive through a response file, libc.a exceeds argv limits.
pub fn archive(out: &Path, objs: &[PathBuf]) -> io::Result<()> {
   let rsp = out.with_added_extension("rsp");
   write_rsp(&rsp, objs)?;
   remove_if_exists(out)?;
   execute(
      Command::new("llvm-ar")
         .arg("rcsD")
         .arg(out)
         .arg(format!("@{}", rsp.display())),
   )?;
   remove_if_exists(&rsp)
}
