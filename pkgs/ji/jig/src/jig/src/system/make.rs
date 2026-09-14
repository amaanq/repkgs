use std::{
   collections::BTreeMap,
   env::var,
   fs::read_to_string,
   io,
   path::{
      Path,
      PathBuf,
   },
};

use serde::Deserialize;

use crate::{
   attrs::Ctx,
   store::on_path,
   system::command::{
      run,
      tool,
      unknown,
   },
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   root:             String,
   flags:            Vec<String>,
   install_flags:    Vec<String>,
   build_target:     Vec<String>,
   test_target:      Vec<String>,
   install_target:   Vec<String>,
   configure_script: String,
   configure_flags:  Vec<String>,
}

pub fn setup() -> io::Result<BTreeMap<String, String>> {
   let bash = on_path("bash");
   let shell = if bash.is_absolute() {
      bash
   } else {
      tool("sh")?
   };
   let shell_text = shell.to_string_lossy().into_owned();
   Ok(BTreeMap::from([
      ("CONFIG_SHELL".to_owned(), shell_text.clone()),
      ("SHELL".to_owned(), shell_text),
   ]))
}

pub fn workdir(ctx: &Ctx) -> io::Result<PathBuf> {
   let options: Options = ctx.options("make")?;
   Ok(ctx.project_dir(&options.root))
}

/// #!/bin/sh would be the sandbox's.
fn sh_shebang(script: &str) -> io::Result<bool> {
   let head = read_to_string(script)?;
   let first = head.lines().next().unwrap_or_default();
   Ok(first
      .strip_prefix("#!")
      .is_some_and(|rest| rest.trim_start() == "/bin/sh"))
}

pub fn phase(ctx: &Ctx, verb: &str) -> io::Result<()> {
   let options: Options = ctx.options("make")?;
   match verb {
      "configure" => {
         let script = format!("./{}", options.configure_script);
         if !Path::new(&script).exists() {
            return Ok(());
         }
         let mut args = vec![format!("--prefix={}", ctx.out)];
         args.extend(options.configure_flags);
         let makeflags = [("MAKEFLAGS", format!("-j{}", ctx.njobs))];
         if sh_shebang(&script)? {
            args.insert(0, script);
            run(&var("CONFIG_SHELL").unwrap_or_default(), &args, &makeflags)
         } else {
            run(&script, &args, &makeflags)
         }
      },
      "build" => build(ctx, &options.flags, &options.build_target),
      "test" => test(ctx, &options.flags, &options.test_target),
      "install" => {
         let mut flags = options.flags;
         flags.extend(options.install_flags);
         install(ctx, &flags, &options.install_target)
      },
      _ => Err(unknown("make", verb)),
   }
}

pub fn build(ctx: &Ctx, flags: &[String], targets: &[String]) -> io::Result<()> {
   let mut args = vec![format!("-j{}", ctx.njobs)];
   args.extend(targets.iter().cloned());
   args.extend(flags.iter().cloned());
   run("make", &args, &[])
}

pub fn test(ctx: &Ctx, flags: &[String], targets: &[String]) -> io::Result<()> {
   let mut args = vec![format!("-j{}", ctx.test_jobs()?)];
   args.extend(targets.iter().cloned());
   args.extend(flags.iter().cloned());
   run("make", &args, &[])
}

/// PREFIX/prefix for hand-written Makefiles, configure'd ones ignore them.
pub fn install(ctx: &Ctx, flags: &[String], targets: &[String]) -> io::Result<()> {
   let mut args = targets.to_vec();
   args.push(format!("PREFIX={}", ctx.out));
   args.push(format!("prefix={}", ctx.out));
   args.extend(flags.iter().cloned());
   run("make", &args, &[])
}
