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
      beam::{
         self,
         Tool,
      },
      bundler,
      cabal,
      cargo,
      cmake,
      command::{
         Env,
         Setting,
      },
      deno,
      go,
      luarocks,
      make,
      meson,
      node::{
         self,
         Manager,
      },
      pyapp,
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
   Npm,
   Pnpm,
   Yarn,
   Bun,
   Rebar3,
   Mix,
   Luarocks,
   Deno,
   Pyapp,
   Bundler,
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
         "npm" => Ok(Self::Npm),
         "pnpm" => Ok(Self::Pnpm),
         "yarn" => Ok(Self::Yarn),
         "bun" => Ok(Self::Bun),
         "rebar3" => Ok(Self::Rebar3),
         "mix" => Ok(Self::Mix),
         "luarocks" => Ok(Self::Luarocks),
         "deno" => Ok(Self::Deno),
         "pyapp" => Ok(Self::Pyapp),
         "bundler" => Ok(Self::Bundler),
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
      SystemOperation::Setup { system } => setup(&system, socket),
      SystemOperation::Workdir { system } => workdir(&system),
      SystemOperation::Phase { system, verb } => phase(&system, &verb, socket),
   }
}

fn text(vars: BTreeMap<String, String>) -> Env {
   vars
      .into_iter()
      .map(|(name, value)| (name, Setting::Text(value)))
      .collect()
}

fn setup(name: &str, socket: &Path) -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   let directory = workdir_of(&ctx, name)?;
   create_dir_all(&directory)?;
   set_current_dir(&directory)?;
   let env = match name.parse::<System>()? {
      System::Make | System::Autotools => text(make::setup()?),
      System::Cmake | System::Meson | System::Luarocks => Env::new(),
      System::Cargo => text(cargo::setup(&ctx, &Attrs::load()?)?),
      System::Python => text(python::setup(&ctx)?),
      System::Cabal => text(cabal::setup(&ctx)?),
      System::Go => text(go::setup(&ctx, &Attrs::load()?)?),
      System::Npm => node::setup(Manager::Npm, &ctx, &Attrs::load()?)?,
      System::Pnpm => node::setup(Manager::Pnpm, &ctx, &Attrs::load()?)?,
      System::Yarn => node::setup(Manager::Yarn, &ctx, &Attrs::load()?)?,
      System::Bun => node::setup(Manager::Bun, &ctx, &Attrs::load()?)?,
      System::Rebar3 => text(beam::setup(Tool::Rebar3, &ctx, &Attrs::load()?, socket)?),
      System::Mix => text(beam::setup(Tool::Mix, &ctx, &Attrs::load()?, socket)?),
      System::Deno => text(deno::setup(&ctx)?),
      System::Pyapp => text(pyapp::setup(&ctx, &Attrs::load()?)?),
      System::Bundler => text(bundler::setup(&ctx, &Attrs::load()?)?),
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
      System::Npm => node::workdir(Manager::Npm, ctx),
      System::Pnpm => node::workdir(Manager::Pnpm, ctx),
      System::Yarn => node::workdir(Manager::Yarn, ctx),
      System::Bun => node::workdir(Manager::Bun, ctx),
      System::Rebar3 => beam::workdir(Tool::Rebar3, ctx),
      System::Mix => beam::workdir(Tool::Mix, ctx),
      System::Luarocks => luarocks::workdir(ctx),
      System::Deno => deno::workdir(ctx),
      System::Pyapp => pyapp::workdir(ctx),
      System::Bundler => bundler::workdir(ctx),
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
      System::Npm => node::phase(Manager::Npm, &ctx, verb),
      System::Pnpm => node::phase(Manager::Pnpm, &ctx, verb),
      System::Yarn => node::phase(Manager::Yarn, &ctx, verb),
      System::Bun => node::phase(Manager::Bun, &ctx, verb),
      System::Rebar3 => beam::phase(Tool::Rebar3, &ctx, verb, socket),
      System::Mix => beam::phase(Tool::Mix, &ctx, verb, socket),
      System::Luarocks => luarocks::phase(&ctx, verb),
      System::Deno => deno::phase(&ctx, verb),
      System::Pyapp => pyapp::phase(&ctx, verb, &Attrs::load()?),
      System::Bundler => bundler::phase(&ctx, verb),
   }
}
