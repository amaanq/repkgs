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
};

use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
      build_top,
   },
   cli::SystemOperation,
   schema::{
      Descriptor,
      Field,
      Unsupported,
   },
   system::{
      autotools::Autotools,
      beam::{
         Mix,
         Rebar3,
      },
      bundler::Bundler,
      cabal::Cabal,
      cargo::Cargo,
      cmake::Cmake,
      command::Env,
      deno::Deno,
      go::Go,
      luarocks::Luarocks,
      make::Make,
      meson::Meson,
      node::{
         Bun,
         Npm,
         Pnpm,
         Yarn,
      },
      pyapp::Pyapp,
      python::Python,
      syslibs,
      vcxproj::Vcxproj,
   },
};

/// a build system as `builder/systems/<name>.nu` was, setup prints the
/// environment every later phase loads and the phases run in the workdir.
pub trait System: Sync {
   fn name(&self) -> &'static str;

   fn options(&self) -> io::Result<Vec<Field>>;

   fn phases(&self) -> &'static [&'static str] {
      &["build", "test", "install"]
   }

   fn prebuilt(&self) -> bool {
      false
   }

   fn unsupported(&self) -> Option<Unsupported> {
      None
   }

   fn workdir(&self, ctx: &Ctx) -> io::Result<PathBuf> {
      let options: Root = ctx.options(self.name())?;
      Ok(ctx.project_dir(&options.root))
   }

   fn setup(&self, _ctx: &Ctx, _attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      Ok(Env::default())
   }

   fn phase(&self, ctx: &Ctx, attrs: &Attrs, verb: &str, socket: &Path) -> io::Result<()>;
}

#[derive(Deserialize)]
struct Root {
   root: String,
}

const SYSTEMS: &[&dyn System] = &[
   &Make, &Autotools, &Cmake, &Meson, &Cargo, &Python, &Cabal, &Go, &Npm, &Pnpm, &Yarn, &Bun,
   &Rebar3, &Mix, &Luarocks, &Deno, &Pyapp, &Bundler, &Vcxproj,
];

pub fn schemas() -> io::Result<BTreeMap<String, Descriptor>> {
   SYSTEMS
      .iter()
      .map(|system| {
         let descriptor = Descriptor {
            phases:      system
               .phases()
               .iter()
               .map(|&verb| verb.to_owned())
               .collect(),
            prebuilt:    system.prebuilt(),
            unsupported: system.unsupported(),
            options:     system.options()?,
         };
         Ok((system.name().to_owned(), descriptor))
      })
      .collect()
}

fn system(name: &str) -> io::Result<&'static dyn System> {
   SYSTEMS
      .iter()
      .copied()
      .find(|system| system.name() == name)
      .ok_or_else(|| io::Error::other(format!("{name} is not a build system jig knows")))
}

/// the working directory created and entered, the setup environment back.
pub fn setup(name: &str, ctx: &Ctx, attrs: &Attrs, socket: &Path) -> io::Result<Env> {
   let system = system(name)?;
   let directory = system.workdir(ctx)?;
   create_dir_all(&directory)?;
   set_current_dir(&directory)?;
   system.setup(ctx, attrs, socket)
}

pub fn workdir(name: &str, ctx: &Ctx) -> io::Result<PathBuf> {
   system(name)?.workdir(ctx)
}

pub fn phase(name: &str, verb: &str, ctx: &Ctx, attrs: &Attrs, socket: &Path) -> io::Result<()> {
   let system = system(name)?;
   set_current_dir(system.workdir(ctx)?)?;
   system.phase(ctx, attrs, verb, socket)
}

pub fn mode(operation: SystemOperation, socket: &Path) -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   match operation {
      SystemOperation::SysEnv { ecosystem } => {
         let active = syslibs::active(&Attrs::load()?, &ecosystem, &ctx.deps)?;
         let mut stdout = io::stdout().lock();
         serde_json::to_writer(&mut stdout, &active.env)?;
         writeln!(stdout)?;
         Ok(())
      },
      SystemOperation::Setup { system: name } => {
         let env = setup(&name, &ctx, &Attrs::load()?, socket)?;
         let mut stdout = io::stdout().lock();
         serde_json::to_writer(&mut stdout, &env)?;
         writeln!(stdout)?;
         Ok(())
      },
      SystemOperation::Workdir { system: name } => {
         writeln!(io::stdout().lock(), "{}", workdir(&name, &ctx)?.display())?;
         Ok(())
      },
      SystemOperation::Phase { system: name, verb } => {
         phase(&name, &verb, &ctx, &Attrs::load()?, socket)
      },
   }
}
