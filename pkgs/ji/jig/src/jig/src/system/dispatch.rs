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
   str::FromStr,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      build_top,
   },
   cli::SystemOperation,
   system::{
      autotools,
      cabal,
      cargo,
      cmake,
      go,
      make,
      meson,
      python,
   },
};

pub enum System {
   Make,
   Autotools,
   Cmake,
   Meson,
   Cargo,
   Python,
   Cabal,
   Go,
}

impl FromStr for System {
   type Err = io::Error;

   fn from_str(name: &str) -> io::Result<Self> {
      match name {
         "make" => Ok(Self::Make),
         "autotools" => Ok(Self::Autotools),
         "cmake" => Ok(Self::Cmake),
         "meson" => Ok(Self::Meson),
         "cargo" => Ok(Self::Cargo),
         "python" => Ok(Self::Python),
         "cabal" => Ok(Self::Cabal),
         "go" => Ok(Self::Go),
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
      System::Cmake | System::Meson => BTreeMap::<String, String>::new(),
      System::Cargo => cargo::setup(&ctx, &Attrs::load()?)?,
      System::Python => python::setup(&ctx)?,
      System::Cabal => cabal::setup(&ctx)?,
      System::Go => go::setup(&ctx, &Attrs::load()?)?,
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
      System::Meson => Ok(meson::workdir(ctx)),
      System::Cargo => cargo::workdir(ctx),
      System::Python => python::workdir(ctx),
      System::Cabal => cabal::workdir(ctx),
      System::Go => go::workdir(ctx),
   }
}

fn phase(name: &str, verb: &str, socket: &Path) -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   set_current_dir(workdir_of(&ctx, name)?)?;
   match name.parse::<System>()? {
      System::Make => make::phase(&ctx, verb),
      System::Autotools => autotools::phase(&ctx, verb, socket),
      System::Cmake => cmake::phase(&ctx, verb, socket),
      System::Meson => meson::phase(&ctx, verb),
      System::Cargo => cargo::phase(&ctx, verb),
      System::Python => python::phase(&ctx, verb, &Attrs::load()?),
      System::Cabal => cabal::phase(&ctx, verb, &Attrs::load()?, socket),
      System::Go => go::phase(&ctx, verb, &Attrs::load()?),
   }
}
