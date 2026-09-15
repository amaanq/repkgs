use std::{
   env::var,
   fs::{
      copy,
      read_to_string,
      rename,
   },
   io,
   path::Path,
};

use jig::{
   files::glob,
   store::on_path,
};
use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
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
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   flags:            Vec<String>,
   install_flags:    Vec<String>,
   build_target:     Vec<String>,
   test_target:      Vec<String>,
   install_target:   Vec<String>,
   configure_script: String,
   configure_flags:  Vec<String>,
   programs:         Vec<String>,
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
   fn name(&self) -> &'static str {
      "make"
   }

   fn setup(&self, _ctx: &Ctx, _attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      Ok(shell()?.into())
   }

   fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options: Options = ctx.options("make")?;
      match verb {
         "configure" => {
            let script = format!("./{}", options.configure_script);
            if !Path::new(&script).exists() {
               return Ok(());
            }
            let via_shell = sh_shebang(&script)?;
            let shell = var("CONFIG_SHELL").unwrap_or_default();
            let prefix = format!("--prefix={}", ctx.out);
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
         "build" => build(ctx, &options.flags, &options.build_target),
         "test" => test(ctx, &options.flags, &options.test_target),
         "install" => {
            let mut flags = options.flags;
            flags.extend(options.install_flags);
            with_programs(ctx, &options.programs, || {
               install(ctx, &flags, &options.install_target)
            })
         },
         _ => Err(unknown("make", verb)),
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
   let text = shell.to_string_lossy().into_owned();
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
         let plain = path
            .to_string_lossy()
            .strip_suffix(exe.as_str())
            .ok_or_else(|| io::Error::other(format!("{} has no {exe} suffix", path.display())))?
            .to_owned();
         copy(&path, plain)?;
      }
   }
   install()?;
   for program in programs {
      let plain = Path::new(&ctx.out).join("bin").join(program);
      if plain.exists() {
         rename(&plain, format!("{}{exe}", plain.display()))?;
      }
   }
   Ok(())
}

pub fn build(ctx: &Ctx, flags: &[String], targets: &[String]) -> io::Result<()> {
   let jobs = format!("-j{}", ctx.njobs);
   let mut args = vec![jobs.as_str()];
   args.extend(strs(targets));
   args.extend(strs(flags));
   run("make", &args, &[])
}

pub fn test(ctx: &Ctx, flags: &[String], targets: &[String]) -> io::Result<()> {
   let jobs = format!("-j{}", ctx.test_jobs()?);
   let mut args = vec![jobs.as_str()];
   args.extend(strs(targets));
   args.extend(strs(flags));
   run("make", &args, &[])
}

/// PREFIX/prefix for hand-written Makefiles, configure'd ones ignore them.
pub fn install(ctx: &Ctx, flags: &[String], targets: &[String]) -> io::Result<()> {
   let upper = format!("PREFIX={}", ctx.out);
   let lower = format!("prefix={}", ctx.out);
   let mut args: Vec<&str> = strs(targets).collect();
   args.push(&upper);
   args.push(&lower);
   args.extend(strs(flags));
   run("make", &args, &[])
}
