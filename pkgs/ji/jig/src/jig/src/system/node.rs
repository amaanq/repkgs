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
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   prepare::succeed,
   system::command::{
      Env,
      Setting,
      run,
      tool_root,
      unknown,
   },
   tree::shebangs,
};

#[derive(Clone, Copy)]
pub enum Manager {
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
   root:    String,
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

pub fn workdir(manager: Manager, ctx: &Ctx) -> io::Result<PathBuf> {
   let options: Options = ctx.options(manager.name())?;
   Ok(ctx.project_dir(&options.root))
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
) -> io::Result<Vec<String>> {
   let mut args: Vec<String> = match manager {
      Manager::Npm => {
         copy(
            format!("{}/package-lock.json", options.deps),
            "package-lock.json",
         )?;
         set_permissions("package-lock.json", PermissionsExt::from_mode(0o644))?;
         vec!["ci".to_owned(), "--ignore-scripts".to_owned()]
      },
      Manager::Pnpm => {
         vec![
            "install".to_owned(),
            "--offline".to_owned(),
            "--frozen-lockfile".to_owned(),
            "--ignore-scripts".to_owned(),
         ]
      },
      Manager::Yarn => {
         vec![
            "install".to_owned(),
            "--offline".to_owned(),
            "--frozen-lockfile".to_owned(),
            "--ignore-scripts".to_owned(),
            "--non-interactive".to_owned(),
            "--no-progress".to_owned(),
         ]
      },
      Manager::Bun => {
         let builder = attrs
            .probe_inputs
            .first()
            .ok_or_else(|| io::Error::other("the builder tree is not among probeInputs"))?;
         run(
            "bun",
            &[
               format!("{builder}/systems/bun-cache.ts"),
               options.deps.clone(),
               format!("{}/bun-cache", ctx.build),
            ],
            &[],
         )?;
         vec![
            "install".to_owned(),
            "--frozen-lockfile".to_owned(),
            "--offline".to_owned(),
            "--ignore-scripts".to_owned(),
         ]
      },
   };
   args.extend(options.flags.iter().cloned());
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
pub fn setup(manager: Manager, ctx: &Ctx, attrs: &Attrs) -> io::Result<Env> {
   let options: Options = ctx.options(manager.name())?;
   let vars = manager_env(manager, ctx, &options);
   let borrowed: Vec<(&str, String)> = vars
      .iter()
      .map(|pair| (pair.0.as_str(), pair.1.clone()))
      .collect();
   run(
      manager.name(),
      &install_command(manager, ctx, attrs, &options)?,
      &borrowed,
   )?;
   shebangs(Path::new("node_modules"), false)?;
   let mut path: Vec<String> = vec![format!("{}/node_modules/.bin", current_dir()?.display())];
   path.extend(
      var("PATH")
         .unwrap_or_default()
         .split(':')
         .map(str::to_owned),
   );
   let mut env: Env = vars
      .into_iter()
      .map(|pair| (pair.0, Setting::Text(pair.1)))
      .collect();
   env.insert("PATH".to_owned(), Setting::List(path));
   env.insert(
      "LD_LIBRARY_PATH".to_owned(),
      Setting::Text(format!("{}/lib", tool_root(attrs, "libgcc-shim")?)),
   );
   Ok(env)
}

pub fn phase(manager: Manager, ctx: &Ctx, verb: &str) -> io::Result<()> {
   let options: Options = ctx.options(manager.name())?;
   let name = manager.name();
   match verb {
      "build" => {
         let Some(ref script) = options.script else {
            return Ok(());
         };
         let mut args = vec!["run".to_owned()];
         if matches!(manager, Manager::Yarn) {
            args.push("--offline".to_owned());
         }
         args.push(script.clone());
         run(name, &args, &[])
      },
      "test" => {
         let mut args = vec!["test".to_owned()];
         if matches!(manager, Manager::Yarn) {
            args.push("--offline".to_owned());
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
         run(
            "npm",
            &[
               "prune".to_owned(),
               "--omit=dev".to_owned(),
               "--ignore-scripts".to_owned(),
            ],
            &[],
         )?;
      },
      Manager::Pnpm => {
         run(
            "pnpm",
            &[
               "prune".to_owned(),
               "--prod".to_owned(),
               "--ignore-scripts".to_owned(),
            ],
            &[],
         )?;
      },
      Manager::Yarn => {
         let mut args = vec![
            "install".to_owned(),
            "--offline".to_owned(),
            "--frozen-lockfile".to_owned(),
            "--ignore-scripts".to_owned(),
            "--production".to_owned(),
            "--non-interactive".to_owned(),
            "--no-progress".to_owned(),
         ];
         args.extend(options.flags.iter().cloned());
         run("yarn", &args, &[])?;
      },
      Manager::Bun => {
         if !options.compile.is_empty() {
            let bin = format!("{}/bin", ctx.out);
            create_dir_all(&bin)?;
            for (exe, entry) in &options.compile {
               run(
                  "bun",
                  &[
                     "build".to_owned(),
                     "--compile".to_owned(),
                     "--minify".to_owned(),
                     entry.clone(),
                     "--outfile".to_owned(),
                     format!("{bin}/{exe}"),
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
