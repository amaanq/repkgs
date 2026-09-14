use std::{
   collections::BTreeMap,
   env::set_current_dir,
   fs::create_dir_all,
   io::{
      self,
      Write as _,
   },
   path::{
      Path,
      PathBuf,
   },
   process::Command,
   str::FromStr,
};

use crate::{
   attrs::{
      Ctx,
      build_top,
   },
   autotools,
   cli::SystemOperation,
   cmake,
   make,
   store::{
      normalize,
      on_path,
   },
};

pub enum System {
   Make,
   Autotools,
   Cmake,
}

impl FromStr for System {
   type Err = io::Error;

   fn from_str(name: &str) -> io::Result<Self> {
      match name {
         "make" => Ok(Self::Make),
         "autotools" => Ok(Self::Autotools),
         "cmake" => Ok(Self::Cmake),
         _ => {
            Err(io::Error::other(format!(
               "{name} is not a build system jig knows"
            )))
         },
      }
   }
}

pub fn mode(operation: SystemOperation, socket: &Path) -> io::Result<()> {
   match operation {
      SystemOperation::Setup { system } => setup(&system),
      SystemOperation::Workdir { system } => workdir(&system),
      SystemOperation::Phase { system, verb } => phase(&system, &verb, socket),
   }
}

fn setup(name: &str) -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   let directory = workdir_of(&ctx, name)?;
   create_dir_all(&directory)?;
   set_current_dir(&directory)?;
   let env = match name.parse::<System>()? {
      System::Make | System::Autotools => make::setup()?,
      System::Cmake => BTreeMap::<String, String>::new(),
   };
   let mut stdout = io::stdout().lock();
   serde_json::to_writer(&mut stdout, &env)?;
   writeln!(stdout)?;
   Ok(())
}

fn workdir(name: &str) -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   writeln!(io::stdout().lock(), "{}", workdir_of(&ctx, name)?.display())?;
   Ok(())
}

fn workdir_of(ctx: &Ctx, name: &str) -> io::Result<PathBuf> {
   match name.parse::<System>()? {
      System::Make => make::workdir(ctx),
      System::Autotools => autotools::workdir(ctx),
      System::Cmake => Ok(cmake::workdir(ctx)),
   }
}

fn phase(name: &str, verb: &str, socket: &Path) -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   set_current_dir(workdir_of(&ctx, name)?)?;
   match name.parse::<System>()? {
      System::Make => make::phase(&ctx, verb),
      System::Autotools => autotools::phase(&ctx, verb, socket),
      System::Cmake => cmake::phase(&ctx, verb, socket),
   }
}

pub fn project_dir(ctx: &Ctx, root: &str) -> PathBuf {
   normalize(&Path::new(&ctx.src).join(root))
}

pub fn tool(name: &str) -> io::Result<PathBuf> {
   let path = on_path(name);
   if !path.is_absolute() {
      return Err(io::Error::other(format!("{name} not on PATH")));
   }
   Ok(path)
}

pub fn run(program: &str, args: &[String], env: &[(&str, String)]) -> io::Result<()> {
   drop(writeln!(
      io::stderr().lock(),
      "+ {program} {}",
      args.join(" ")
   ));
   let status = Command::new(program)
      .args(args)
      .envs(env.iter().cloned())
      .status()?;
   if !status.success() {
      return Err(io::Error::other(format!(
         "{program} failed with {}",
         status.code().unwrap_or(-1)
      )));
   }
   Ok(())
}

pub fn unknown(name: &str, verb: &str) -> io::Error {
   io::Error::other(format!("{name} has no {verb} phase"))
}
