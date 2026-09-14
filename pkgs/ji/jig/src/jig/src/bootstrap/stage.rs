use std::{
   env::var,
   fs::{
      create_dir_all,
      metadata,
      read,
      read_to_string,
      remove_file,
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

use jig::{
   files::{
      glob,
      write_file,
   },
   store::on_path,
   tree::parallel_with,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Outputs {
   pub out: String,
}

/// the structured attrs of one bootstrap stage, see bootstrap/default.nix
/// mkStage.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stage {
   pub outputs:       Outputs,
   pub clang_target:  String,
   pub build_triple:  String,
   pub cpu:           String,
   pub libc:          String,
   pub os:            String,
   pub binfmt:        String,
   pub interp:        String,
   pub karch:         String,
   pub flags:         String,
   pub seed:          String,
   #[serde(default)]
   pub src:           Option<String>,
   #[serde(default)]
   pub patches:       Vec<String>,
   #[serde(default)]
   pub linux_headers: Option<String>,
   #[serde(default, rename = "compiler-rt")]
   pub compiler_rt:   Option<String>,
   #[serde(default)]
   pub libc_headers:  Option<String>,
   #[serde(default)]
   pub list:          Option<String>,
   #[serde(default)]
   pub headers_only:  Option<String>,
   #[serde(default)]
   pub locale:        Option<bool>,
   #[serde(default)]
   pub parts:         Vec<String>,
   #[serde(default)]
   pub resource:      Option<String>,
   #[serde(default)]
   pub sysroot:       Option<String>,
   #[serde(default)]
   pub prebuilt:      Option<String>,
   #[serde(default)]
   pub native:        Option<String>,
   #[serde(default, rename = "crt_interp")]
   pub crt_interp:    Option<String>,
   #[serde(default)]
   pub reloc:         Option<String>,
   #[serde(default, rename = "json_hpp")]
   pub json_hpp:      Option<String>,
   #[serde(default)]
   pub launch:        Option<String>,
   #[serde(default)]
   pub dlaudit:       Option<String>,
   #[serde(skip)]
   pub build_top:     String,
}

impl Stage {
   pub fn load() -> io::Result<Self> {
      let path = var("NIX_ATTRS_JSON_FILE")
         .map_err(|_unset| io::Error::other("NIX_ATTRS_JSON_FILE is not set"))?;
      let mut stage: Self = serde_json::from_str(&read_to_string(path)?)?;
      stage.build_top =
         var("NIX_BUILD_TOP").map_err(|_unset| io::Error::other("NIX_BUILD_TOP is not set"))?;
      Ok(stage)
   }

   pub fn out(&self) -> &str {
      &self.outputs.out
   }

   pub const fn headers_only(&self) -> bool {
      self.headers_only.is_some()
   }

   pub fn src(&self) -> io::Result<&str> {
      self
         .src
         .as_deref()
         .ok_or_else(|| io::Error::other("this stage has no src"))
   }

   /// --target plus the platform's -march and hardening flags.
   pub fn target(&self) -> Vec<String> {
      let mut args = vec![format!("--target={}", self.clang_target)];
      args.extend(self.flags.split_whitespace().map(str::to_owned));
      args
   }

   /// compile and link against the sysroot with the raw seed clang, which
   /// defaults to libunwind, built last. Plain C needs no unwinder.
   pub fn ccflags(&self) -> io::Result<Vec<String>> {
      let sysroot = wanted(self.sysroot.as_deref(), "sysroot")?;
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

   /// a writable copy of the unpacked source under the build top, `only`
   /// limits it to subdirectories. -p keeps the store's uniform mtimes so
   /// generated files stay newer than their inputs for make.
   pub fn unpack(&self, name: &str, only: &[&str]) -> io::Result<PathBuf> {
      let src = self.src()?;
      let dest = PathBuf::from(&self.build_top).join(name);
      create_dir_all(&dest)?;
      let dest_text = dest.to_string_lossy().into_owned();
      if only.is_empty() {
         run("cp", &["-rp", &format!("{src}/."), &dest_text])?;
      }
      for sub in only {
         let target = dest.join(sub);
         if let Some(parent) = target.parent() {
            create_dir_all(parent)?;
         }
         run("cp", &[
            "-rp",
            &format!("{src}/{sub}"),
            &target.to_string_lossy(),
         ])?;
      }
      run("chmod", &["-R", "u+w", &dest_text])?;
      Ok(dest)
   }
}

/// a stage attr a recipe cannot do without.
pub fn wanted<'field>(field: Option<&'field str>, name: &str) -> io::Result<&'field str> {
   field.ok_or_else(|| io::Error::other(format!("this stage has no {name}")))
}

pub fn say(message: &str) {
   drop(writeln!(
      io::stderr().lock(),
      "\u{1b}[32m==\u{1b}[0m {message}"
   ));
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

fn echo(program: &str, args: &[&str]) {
   drop(writeln!(
      io::stderr().lock(),
      "+ {program} {}",
      args.join(" ")
   ));
}

pub fn run(program: &str, args: &[&str]) -> io::Result<()> {
   run_in_env(None, program, args, &[])
}

pub fn run_in(directory: &Path, program: &str, args: &[&str]) -> io::Result<()> {
   run_in_env(Some(directory), program, args, &[])
}

pub fn run_with(program: &str, args: &[&str], env: &[(&str, &str)]) -> io::Result<()> {
   run_in_env(None, program, args, env)
}

pub fn run_in_env(
   directory: Option<&Path>,
   program: &str,
   args: &[&str],
   env: &[(&str, &str)],
) -> io::Result<()> {
   echo(program, args);
   let mut command = Command::new(program);
   command.args(args).envs(env.iter().copied());
   if let Some(path) = directory {
      command.current_dir(path);
   }
   let status = command.status()?;
   if !status.success() {
      return Err(io::Error::other(format!(
         "{program} failed with {}",
         status.code().unwrap_or(-1)
      )));
   }
   Ok(())
}

pub fn capture(program: &str, args: &[&str]) -> io::Result<String> {
   let output = Command::new(program).args(args).output()?;
   if !output.status.success() {
      return Err(io::Error::other(format!(
         "{program} failed with {}",
         output.status.code().unwrap_or(-1)
      )));
   }
   Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

pub fn strs(values: &[String]) -> Vec<&str> {
   values.iter().map(String::as_str).collect()
}

/// regular files matching the pattern, sorted, so archive member order does
/// not depend on directory order.
pub fn files(pattern: &str) -> io::Result<Vec<PathBuf>> {
   let mut found: Vec<PathBuf> = glob(pattern)?
      .into_iter()
      .filter(|path| metadata(path).is_ok_and(|meta| meta.is_file()))
      .collect();
   found.sort_by_key(|path| path.to_string_lossy().into_owned());
   Ok(found)
}

pub fn dirs(pattern: &str) -> io::Result<Vec<PathBuf>> {
   let mut found: Vec<PathBuf> = glob(pattern)?
      .into_iter()
      .filter(|path| metadata(path).is_ok_and(|meta| meta.is_dir()))
      .collect();
   found.sort_by_key(|path| path.to_string_lossy().into_owned());
   Ok(found)
}

/// copy the files under `from` matching `pattern` to `to`, keeping relative
/// paths and dropping the store's modes.
pub fn copy_tree(from: &Path, to: &Path, pattern: &str) -> io::Result<()> {
   let sources = files(&format!("{}/{pattern}", from.to_string_lossy()))?;
   let pairs: Vec<(PathBuf, PathBuf)> = sources
      .into_iter()
      .map(|source| {
         let rel = source.strip_prefix(from).unwrap_or(&source).to_path_buf();
         (source, to.join(rel))
      })
      .collect();
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
pub fn cc_facts(out: &str, facts: &[(&str, &[&str])]) -> io::Result<()> {
   let dir = format!("{out}/etc/cc");
   create_dir_all(&dir)?;
   for &(name, lines) in facts {
      let mut text = lines.join("\n");
      text.push('\n');
      write_file(Path::new(&format!("{dir}/{name}")), text.as_bytes(), None)?;
   }
   Ok(())
}

pub fn cc_fact(libc: &str, name: &str) -> io::Result<Option<Vec<String>>> {
   let file = PathBuf::from(format!("{libc}/etc/cc/{name}"));
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

/// parallel compile with the seed clang, the object paths back.
pub fn compile(common: &[String], items: &[Item]) -> io::Result<Vec<PathBuf>> {
   let failures: Vec<Option<(PathBuf, String)>> = parallel_with(items, cores(), |item| {
      if let Some(parent) = item.obj.parent() {
         create_dir_all(parent)?;
      }
      let output = Command::new("clang")
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
   let failed: Vec<&(PathBuf, String)> = failures.iter().flatten().collect();
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

/// a static archive through a response file, libc.a exceeds argv limits.
pub fn archive(out: &Path, objs: &[PathBuf]) -> io::Result<()> {
   let rsp = PathBuf::from(format!("{}.rsp", out.to_string_lossy()));
   let mut text = objs
      .iter()
      .map(|obj| obj.to_string_lossy().into_owned())
      .collect::<Vec<String>>()
      .join("\n");
   text.push('\n');
   write_file(&rsp, text.as_bytes(), None)?;
   if out.exists() {
      remove_file(out)?;
   }
   run("llvm-ar", &[
      "rcsD",
      &out.to_string_lossy(),
      &format!("@{}", rsp.to_string_lossy()),
   ])?;
   remove_file(&rsp)
}
