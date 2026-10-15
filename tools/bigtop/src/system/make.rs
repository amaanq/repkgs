use std::{
   collections::BTreeMap,
   env::var,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   fs::{
      copy,
      read_to_string,
      rename,
   },
   io,
   iter,
   path::{
      Path,
      PathBuf,
   },
   str::FromStr,
};

use mummer::{
   files::{
      glob,
      utf8,
   },
   store::on_path,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   schema::Field,
   system::{
      command::{
         Env,
         run,
         strs,
         tool,
         unknown,
      },
      dispatch::System,
   },
   test_cache,
};

options! {
   Options {
      root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      flags ("flags"): Vec<String> = Vec::new()
         => "arguments for every make invocation (build, test, install)",
      install_flags ("installFlags"): Vec<String> = Vec::new()
         => "arguments for `make install` only",
      build_target ("buildTarget"): Vec<String> = Vec::new()
         => "make goals for build (empty means the makefile's default goal)",
      test_target ("testTarget"): Vec<String> = vec!["check".to_owned()]
         => "make goals for test",
      install_target ("installTarget"): Vec<String> = vec!["install".to_owned()]
         => "make goals for install",
      configure_script ("configureScript"): String = "configure".to_owned()
         => "hand-written configure script relative to the project, run with --prefix when it exists",
      configure_flags ("configureFlags"): Vec<String> = Vec::new()
         => "extra arguments for make.configureScript",
      programs ("programs"): Vec<String> = Vec::new()
         => "programs the Makefile installs without the platform executable suffix",
   }
}

pub struct Make;

/// #!/bin/sh would be the sandbox's.
fn sh_shebang(script: &str) -> io::Result<bool> {
   let head = read_to_string(script)?;
   let first = head.lines().next().unwrap_or_default();
   Ok(first
      .strip_prefix("#!")
      .is_some_and(|rest| rest.trim_start() == "/bin/sh"))
}

impl System for Make {
   fn module(&self) -> &'static str {
      module_path!()
   }

   fn name(&self) -> &'static str {
      "make"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   fn phases(&self) -> &'static [&'static str] {
      &["configure", "build", "test", "install"]
   }

   fn workdir(&self, ctx: &Ctx) -> io::Result<PathBuf> {
      let options = ctx.options::<Options>(self.name())?;
      Ok(ctx.project_dir(&options.root))
   }

   fn setup(&self, _ctx: &Ctx, _attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      Ok(shell()?.into())
   }

   fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>(self.name())?;
      // PREFIX/prefix for hand-written Makefiles, configure'd ones ignore them.
      let mut flags = vec![
         format!("PREFIX={}", ctx.out.display()),
         format!("prefix={}", ctx.out.display()),
      ];
      let platform = ctx.platform()?;

      if platform.cross {
         flags.push(format!("UNAME={}", platform.os_names.uname));
      }

      flags.extend(options.flags);

      match verb {
         "configure" => {
            let script = format!("./{}", options.configure_script);
            if !Path::new(&script).exists() {
               return Ok(());
            }
            let via_shell = sh_shebang(&script)?;
            let shell = var("CONFIG_SHELL").unwrap_or_default();
            let prefix = format!("--prefix={}", ctx.out.display());
            let mut args = Vec::new();
            if via_shell {
               args.push(script.as_str());
            }
            args.push(&prefix);
            args.extend(strs(&options.configure_flags));
            let jobs = format!("-j{}", ctx.njobs);
            run(if via_shell { &shell } else { &script }, &args, &[(
               "MAKEFLAGS",
               jobs.as_str(),
            )])
         },
         "build" => build(ctx, &flags, &options.build_target),
         "test" => test(ctx, &flags, &options.test_target),
         "install" => {
            flags.extend(options.install_flags);
            with_programs(ctx, &options.programs, || {
               install(ctx, &flags, &options.install_target)
            })
         },
         _ => Err(unknown(self.name(), verb)),
      }
   }
}

pub fn shell() -> io::Result<[(&'static str, String); 2]> {
   let bash = on_path("bash");
   let shell = if bash.is_absolute() {
      bash
   } else {
      tool("sh")?
   };
   let text = utf8(&shell)?.to_owned();
   Ok([("CONFIG_SHELL", text.clone()), ("SHELL", text)])
}

fn with_programs(
   ctx: &Ctx,
   programs: &[String],
   install: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
   let exe = ctx.platform()?.ext.exe;
   if exe.is_empty() {
      return install();
   }
   for program in programs {
      for path in glob(&format!("**/{program}{exe}"))? {
         if !path.is_file() {
            continue;
         }

         let plain = utf8(&path)?
            .strip_suffix(exe.as_str())
            .ok_or_else(|| io::Error::other(format!("{} has no {exe} suffix", path.display())))?
            .to_owned();
         copy(&path, plain)?;
      }
   }
   install()?;
   for program in programs {
      let plain = ctx.out.join("bin").join(program);
      if plain.exists() {
         rename(&plain, format!("{}{exe}", plain.display()))?;
      }
   }
   Ok(())
}

pub fn build(ctx: &Ctx, flags: &[String], targets: &[String]) -> io::Result<()> {
   let jobs = format!("-j{}", ctx.njobs);
   let args = iter::once(jobs.as_str())
      .chain(strs(targets))
      .chain(strs(flags))
      .collect::<Vec<_>>();
   run("make", &args, &[])
}

/// A test automake's parallel harness ran, named by the directory its
/// driver ran in and the name the driver was given.
pub struct AutomakeTest {
   pub directory: String,
   pub name:      String,
}

impl Display for AutomakeTest {
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      write!(f, "{}\t{}", self.directory, self.name)
   }
}

impl FromStr for AutomakeTest {
   type Err = String;

   fn from_str(text: &str) -> Result<Self, Self::Err> {
      let (directory, name) = text
         .split_once('\t')
         .ok_or_else(|| format!("{text} names no automake test"))?;
      Ok(Self {
         directory: directory.to_owned(),
         name:      name.to_owned(),
      })
   }
}

pub fn test(ctx: &Ctx, flags: &[String], targets: &[String]) -> io::Result<()> {
   let jobs = format!("-j{}", ctx.test_jobs()?);
   let Some(selected) = test_cache::selected::<AutomakeTest>()? else {
      let mut args = vec![jobs.as_str(), "VERBOSE=1"];
      args.extend(strs(targets));
      args.extend(strs(flags));
      return run("make", &args, &[]);
   };
   let mut directories = BTreeMap::<String, Vec<String>>::new();
   for test in selected {
      directories
         .entry(test.directory)
         .or_default()
         .push(test.name);
   }
   for (directory, names) in directories {
      let tests = format!("TESTS={}", names.join(" "));
      let mut args = vec!["-C", directory.as_str(), jobs.as_str(), "VERBOSE=1"];
      args.extend(strs(targets));
      args.push(&tests);
      args.extend(strs(flags));
      run("make", &args, &[])?;
   }
   Ok(())
}

pub fn install(ctx: &Ctx, flags: &[String], targets: &[String]) -> io::Result<()> {
   build(ctx, flags, targets)
}
