use std::{
   collections::BTreeMap,
   env::{
      current_dir,
      var,
   },
   fs::{
      copy,
      create_dir_all,
      read_to_string,
      set_permissions,
   },
   io,
   os::unix::fs::{
      PermissionsExt,
      symlink,
   },
   path::Path,
   process::Command,
};

use jig::tree::shebangs;
use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   prepare::succeed,
   system::{
      command::{
         Env,
         run,
         strs,
         tool_root,
         unknown,
      },
      dispatch::System,
   },
};

#[derive(Clone, Copy)]
enum Manager {
   Npm,
   Pnpm,
   Yarn,
   Bun,
}

impl Manager {
   const fn name(self) -> &'static str {
      match self {
         Self::Npm => "npm",
         Self::Pnpm => "pnpm",
         Self::Yarn => "yarn",
         Self::Bun => "bun",
      }
   }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   #[serde(default)]
   script:  Option<String>,
   deps:    String,
   flags:   Vec<String>,
   #[serde(default)]
   compile: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct PackageJson {
   name: String,
   #[serde(default)]
   bin:  Option<Bin>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Bin {
   One(String),
   Many(BTreeMap<String, String>),
}

macro_rules! managers {
   ($($name:ident => $manager:expr),* $(,)?) => {
      $(
         pub struct $name;

         impl System for $name {
            fn name(&self) -> &'static str {
               $manager.name()
            }

            fn setup(&self, ctx: &Ctx, attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
               setup($manager, ctx, attrs)
            }

            fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
               phase($manager, ctx, verb)
            }
         }
      )*
   };
}

managers! {
   Npm => Manager::Npm,
   Pnpm => Manager::Pnpm,
   Yarn => Manager::Yarn,
   Bun => Manager::Bun,
}

fn text(pairs: &[(&str, String)]) -> Vec<(String, String)> {
   pairs
      .iter()
      .map(|pair| (pair.0.to_owned(), pair.1.clone()))
      .collect()
}

/// pnpm 10 reads `npm_config_*`, 11 only `pnpm_config_*`.
fn pnpm_conf(settings: &[(&str, String)]) -> Vec<(String, String)> {
   settings
      .iter()
      .flat_map(|pair| {
         [
            (format!("npm_config_{}", pair.0), pair.1.clone()),
            (format!("pnpm_config_{}", pair.0), pair.1.clone()),
         ]
      })
      .collect()
}

fn install_command(
   manager: Manager,
   ctx: &Ctx,
   attrs: &Attrs,
   options: &Options,
) -> io::Result<Vec<&'static str>> {
   let args = match manager {
      Manager::Npm => {
         copy(
            format!("{}/package-lock.json", options.deps),
            "package-lock.json",
         )?;
         set_permissions("package-lock.json", PermissionsExt::from_mode(0o644))?;
         vec!["ci", "--ignore-scripts"]
      },
      Manager::Pnpm => {
         vec![
            "install",
            "--offline",
            "--frozen-lockfile",
            "--ignore-scripts",
         ]
      },
      Manager::Yarn => {
         vec![
            "install",
            "--offline",
            "--frozen-lockfile",
            "--ignore-scripts",
            "--non-interactive",
            "--no-progress",
         ]
      },
      Manager::Bun => {
         let builder = attrs
            .probe_inputs
            .first()
            .ok_or_else(|| io::Error::other("the builder tree is not among probeInputs"))?;
         let script = format!("{builder}/systems/bun-cache.ts");
         let cache = format!("{}/bun-cache", ctx.build);
         run("bun", &[&script, &options.deps, &cache], &[])?;
         vec![
            "install",
            "--frozen-lockfile",
            "--offline",
            "--ignore-scripts",
         ]
      },
   };
   Ok(args)
}

/// pnpm 11 wants a host part in the registry URL and opens tarballs at the
/// URL minus "file:", so "file:/" plus the store path.
fn manager_env(manager: Manager, ctx: &Ctx, options: &Options) -> Vec<(String, String)> {
   let mut env = match manager {
      Manager::Npm => {
         text(&[
            ("npm_config_cache", format!("{}/npm-cache", ctx.build)),
            ("npm_config_offline", "true".to_owned()),
            ("npm_config_progress", "false".to_owned()),
            ("npm_config_audit", "false".to_owned()),
            ("npm_config_fund", "false".to_owned()),
            ("npm_config_update_notifier", "false".to_owned()),
            ("npm_config_loglevel", "warn".to_owned()),
         ])
      },
      Manager::Pnpm => {
         pnpm_conf(&[
            ("registry", format!("file:/{}/registry/", options.deps)),
            ("store_dir", format!("{}/pnpm-store", ctx.build)),
            ("offline", "true".to_owned()),
            ("pm_on_fail", "ignore".to_owned()),
            ("minimum_release_age", "0".to_owned()),
            ("side_effects_cache", "false".to_owned()),
            ("update_notifier", "false".to_owned()),
            ("loglevel", "warn".to_owned()),
         ])
      },
      Manager::Yarn => {
         text(&[
            ("YARN_CACHE_FOLDER", format!("{}/yarn-cache", ctx.build)),
            ("YARN_YARN_OFFLINE_MIRROR", options.deps.clone()),
            ("YARN_ENABLE_PROGRESS_BARS", "false".to_owned()),
            ("YARN_DISABLE_SELF_UPDATE_CHECK", "true".to_owned()),
         ])
      },
      Manager::Bun => {
         text(&[
            ("BUN_INSTALL_CACHE_DIR", format!("{}/bun-cache", ctx.build)),
            ("BUN_INSTALL", format!("{}/bun-home", ctx.build)),
            ("DO_NOT_TRACK", "1".to_owned()),
         ])
      },
   };
   if !matches!(manager, Manager::Bun) {
      env.push(("NODE_OPTIONS".to_owned(), "--no-deprecation".to_owned()));
   }
   env
}

/// the prebuilt .node addons in `node_modules/.bin` (rollup, esbuild) want
/// `libgcc_s.so.1` findable.
fn setup(manager: Manager, ctx: &Ctx, attrs: &Attrs) -> io::Result<Env> {
   let options: Options = ctx.options(manager.name())?;
   let vars = manager_env(manager, ctx, &options);
   let env_vars: Vec<(&str, &str)> = vars
      .iter()
      .map(|pair| (pair.0.as_str(), pair.1.as_str()))
      .collect();
   let mut args: Vec<&str> = install_command(manager, ctx, attrs, &options)?
      .into_iter()
      .collect();
   args.extend(strs(&options.flags));
   run(manager.name(), &args, &env_vars)?;
   let modules = Path::new("node_modules");
   if modules.try_exists()? {
      shebangs(modules, false)?;
   }

   let mut path: Vec<String> = vec![format!("{}/node_modules/.bin", current_dir()?.display())];
   path.extend(
      var("PATH")
         .unwrap_or_default()
         .split(':')
         .map(str::to_owned),
   );
   let mut env = Env::from(vars.into_iter().collect::<BTreeMap<String, String>>());
   env.set_list("PATH", path);
   env.set(
      "LD_LIBRARY_PATH",
      format!("{}/lib", tool_root(attrs, "libgcc-shim")?),
   );
   Ok(env)
}

fn phase(manager: Manager, ctx: &Ctx, verb: &str) -> io::Result<()> {
   let options: Options = ctx.options(manager.name())?;
   let name = manager.name();
   match verb {
      "build" => {
         let Some(ref script) = options.script else {
            return Ok(());
         };
         let mut args = vec!["run"];
         if matches!(manager, Manager::Yarn) {
            args.push("--offline");
         }
         args.push(script.as_str());
         run(name, &args, &[])
      },
      "test" => {
         let mut args = vec!["test"];
         if matches!(manager, Manager::Yarn) {
            args.push("--offline");
         }
         run(name, &args, &[])
      },
      "install" => install(manager, ctx, &options),
      _ => Err(unknown(name, verb)),
   }
}

fn install(manager: Manager, ctx: &Ctx, options: &Options) -> io::Result<()> {
   match manager {
      Manager::Npm => {
         run("npm", &["prune", "--omit=dev", "--ignore-scripts"], &[])?;
      },
      Manager::Pnpm => {
         run("pnpm", &["prune", "--prod", "--ignore-scripts"], &[])?;
      },
      Manager::Yarn => {
         let mut args = vec![
            "install",
            "--offline",
            "--frozen-lockfile",
            "--ignore-scripts",
            "--production",
            "--non-interactive",
            "--no-progress",
         ];
         args.extend(strs(&options.flags));
         run("yarn", &args, &[])?;
      },
      Manager::Bun => {
         if !options.compile.is_empty() {
            let bin = format!("{}/bin", ctx.out);
            create_dir_all(&bin)?;
            for (exe, entry) in &options.compile {
               let outfile = format!("{bin}/{exe}");
               run(
                  "bun",
                  &[
                     "build",
                     "--compile",
                     "--minify",
                     entry.as_str(),
                     "--outfile",
                     outfile.as_str(),
                  ],
                  &[],
               )?;
            }
            return Ok(());
         }
      },
   }
   install_tree(ctx)
}

/// `bin` as a string means one executable named like the package, npm strips
/// a scope.
fn install_tree(ctx: &Ctx) -> io::Result<()> {
   let tree = format!("{}/lib/node_modules/{}", ctx.out, ctx.spec()?.name);
   create_dir_all(format!("{}/lib/node_modules", ctx.out))?;
   create_dir_all(format!("{}/bin", ctx.out))?;
   succeed(Command::new("cp").args(["-r", ".", &tree]), "cp")?;
   let package: PackageJson = serde_json::from_str(&read_to_string("package.json")?)?;
   let bins = match package.bin {
      Some(Bin::One(path)) => {
         BTreeMap::from([(
            package
               .name
               .rsplit('/')
               .next()
               .unwrap_or_default()
               .to_owned(),
            path,
         )])
      },
      Some(Bin::Many(bins)) => bins,
      None => BTreeMap::new(),
   };
   for (name, path) in &bins {
      symlink(format!("{tree}/{path}"), format!("{}/bin/{name}", ctx.out))?;
   }
   Ok(())
}
