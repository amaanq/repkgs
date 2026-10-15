use std::{
   array,
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

use mummer::tree::shebangs;
use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   prepare::succeed,
   schema::{
      Field,
      PackagePath,
   },
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

options! {
   Options {
      _root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      script ("script"): Option<String> = Some("build".to_owned())
         => "package.json script the build phase runs (null runs none)",
      deps ("deps"): PackagePath
         => "the locked dependencies, fetched from the package's own lock file",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for the package manager's install",
   }
}

options! {
   BunOptions {
      _root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      script ("script"): Option<String> = Some("build".to_owned())
         => "package.json script the build phase runs (null runs none)",
      deps ("deps"): PackagePath
         => "the locked dependencies, fetched from the package's own lock file",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for bun install",
      compile ("compile"): BTreeMap<String, String> = BTreeMap::new()
         => "bin name -> entry module, `bun build --compile` single executables instead of installing the tree",
   }
}

/// bun's `compile` is the one option the other managers lack.
fn options(manager: Manager, ctx: &Ctx) -> io::Result<(Options, BTreeMap<String, String>)> {
   if !matches!(manager, Manager::Bun) {
      return Ok((ctx.options(manager.name())?, BTreeMap::new()));
   }

   let bun = ctx.options::<BunOptions>("bun")?;
   let common = Options {
      _root:  bun._root,
      script: bun.script,
      deps:   bun.deps,
      flags:  bun.flags,
   };
   Ok((common, bun.compile))
}

#[derive(Deserialize)]
struct PackageJson {
   name:    String,
   #[serde(default)]
   bin:     Option<Bin>,
   #[serde(default)]
   scripts: BTreeMap<String, String>,
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
            fn module(&self) -> &'static str {
               module_path!()
            }

            fn name(&self) -> &'static str {
               $manager.name()
            }

            fn options(&self) -> io::Result<Vec<Field>> {
               if matches!($manager, Manager::Bun) {
                  BunOptions::schema(self.name())
               } else {
                  Options::schema(self.name())
               }
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

#[derive(Deserialize)]
struct BunIndexed {
   id:  String,
   dir: String,
}

/// Links `<cache>/<folder>` to each package unpacked under `<deps>/<dir>` so
/// `bun install --offline` finds it.
fn fill_bun_cache(deps: &str, cache: &str) -> io::Result<()> {
   let index =
      serde_json::from_str::<Vec<BunIndexed>>(&read_to_string(format!("{deps}/index.json"))?)?;
   for entry in index {
      let link = Path::new(cache).join(bun_cache_folder(&entry.id));
      if let Some(parent) = link.parent() {
         create_dir_all(parent)?;
      }
      symlink(Path::new(deps).join(&entry.dir), link)?;
   }
   Ok(())
}

/// bun 1.4 names a cached package `<name>@<major>.<minor>.<patch>`, then the
/// pre-release as `-<hash>` and the build as `+<HASH>`, then `@@@1`
/// (`cached_npm_package_folder_print_basename` in
/// src/install/PackageManager/PackageManagerDirectories.rs).
fn bun_cache_folder(id: &str) -> String {
   let Some((name, version)) = id.rsplit_once('@').filter(|&(head, _)| !head.is_empty()) else {
      return format!("{id}@@@1");
   };
   let (core, build) = version
      .split_once('+')
      .map_or((version, None), |(head, tail)| (head, Some(tail)));
   let release = match core.split_once('-') {
      Some((numbers, pre)) => format!("{numbers}-{:016x}", wyhash11(pre.as_bytes())),
      None => core.to_owned(),
   };
   let suffix = build.map_or_else(String::new, |tag| {
      format!("+{:016X}", wyhash11(tag.as_bytes()))
   });
   format!("{name}@{release}{suffix}@@@1")
}

const WYHASH11_PRIMES: [u64; 5] = [
   0xA076_1D64_78BD_642F,
   0xE703_7ED1_A0B4_28DB,
   0x8EBC_6AF0_9C88_C6E3,
   0x5899_65CC_7537_4CC3,
   0x1D8E_4E27_C47D_124F,
];

/// bun's `Wyhash11::hash(0, input)` (src/wyhash/lib.rs), the legacy wyhash
/// with 32-byte rounds that bun hashes semver tags with.
fn wyhash11(input: &[u8]) -> u64 {
   let [p0, p1, p2, p3, p4] = WYHASH11_PRIMES;
   let mum = |left: u64, right: u64| {
      let product = u128::from(left) * u128::from(right);
      u64::try_from((product ^ (product >> 64_u32)) & u128::from(u64::MAX)).unwrap_or_default()
   };
   let little = |bytes: &[u8]| {
      bytes
         .iter()
         .rev()
         .fold(0_u64, |value, &byte| (value << 8_u32) | u64::from(byte))
   };
   // a tail word reads its 8 bytes as two halves, the first one high, and a
   // shorter one as 4, 2 and 1 byte pieces in that order, the first one high
   let tail_word = |bytes: &[u8]| {
      if bytes.len() == 8 {
         return (little(&bytes[..4]) << 32) | little(&bytes[4..]);
      }
      let mut value = 0_u64;
      let mut at = 0_usize;
      for piece in [4_usize, 2, 1] {
         if bytes.len() & piece != 0 {
            value = (value << (8 * piece)) | little(&bytes[at..at + piece]);
            at += piece;
         }
      }
      value
   };

   let mut seed = 0_u64;
   let (blocks, rest) = input.as_chunks::<32>();
   for block in blocks {
      let [w0, w1, w2, w3] = array::from_fn(|lane| little(&block[8 * lane..8 * lane + 8]));
      seed = mum(w0 ^ seed ^ p0, w1 ^ seed ^ p1) ^ mum(w2 ^ seed ^ p2, w3 ^ seed ^ p3);
   }

   let mut words = rest.chunks(8).map(tail_word);
   let mut word = || words.next().unwrap_or(p4);
   if !rest.is_empty() {
      let head = mum(word() ^ seed ^ p0, word() ^ seed ^ p1);
      seed = if rest.len() > 16 {
         head ^ mum(word() ^ seed ^ p2, word() ^ seed ^ p3)
      } else {
         head
      };
   }
   mum(seed ^ u64::try_from(input.len()).unwrap_or(u64::MAX), p4)
}

/// What the install reads beyond the tree, npm's lock file and bun's cache.
fn prepare_install(manager: Manager, ctx: &Ctx, options: &Options) -> io::Result<()> {
   match manager {
      Manager::Npm => {
         copy(
            format!("{}/package-lock.json", options.deps),
            "package-lock.json",
         )?;
         set_permissions("package-lock.json", PermissionsExt::from_mode(0o644))
      },
      Manager::Bun => fill_bun_cache(&options.deps, &format!("{}/bun-cache", ctx.build.display())),
      Manager::Pnpm | Manager::Yarn => Ok(()),
   }
}

const fn install_args(manager: Manager) -> &'static [&'static str] {
   match manager {
      Manager::Npm => &["ci", "--ignore-scripts"],
      Manager::Pnpm => {
         &[
            "install",
            "--offline",
            "--frozen-lockfile",
            "--ignore-scripts",
         ]
      },
      Manager::Yarn => {
         &[
            "install",
            "--offline",
            "--frozen-lockfile",
            "--ignore-scripts",
            "--non-interactive",
            "--no-progress",
         ]
      },
      Manager::Bun => {
         &[
            "install",
            "--frozen-lockfile",
            "--offline",
            "--ignore-scripts",
         ]
      },
   }
}

/// pnpm 10 reads `npm_config_*` and 11 only `pnpm_config_*`. pnpm 11 wants a
/// host part in the registry URL and opens tarballs at the URL minus "file:",
/// so "file:/" plus the store path.
fn manager_env(manager: Manager, ctx: &Ctx, options: &Options) -> Vec<(String, String)> {
   let settings = match manager {
      Manager::Npm => {
         vec![
            (
               "npm_config_cache",
               format!("{}/npm-cache", ctx.build.display()),
            ),
            ("npm_config_offline", "true".to_owned()),
            ("npm_config_progress", "false".to_owned()),
            ("npm_config_audit", "false".to_owned()),
            ("npm_config_fund", "false".to_owned()),
            ("npm_config_update_notifier", "false".to_owned()),
            ("npm_config_loglevel", "warn".to_owned()),
         ]
      },
      Manager::Pnpm => {
         vec![
            ("registry", format!("file:/{}/registry/", options.deps)),
            ("store_dir", format!("{}/pnpm-store", ctx.build.display())),
            ("offline", "true".to_owned()),
            ("pm_on_fail", "ignore".to_owned()),
            ("minimum_release_age", "0".to_owned()),
            ("side_effects_cache", "false".to_owned()),
            ("update_notifier", "false".to_owned()),
            ("loglevel", "warn".to_owned()),
         ]
      },
      Manager::Yarn => {
         vec![
            (
               "YARN_CACHE_FOLDER",
               format!("{}/yarn-cache", ctx.build.display()),
            ),
            ("YARN_YARN_OFFLINE_MIRROR", options.deps.to_string()),
            ("YARN_ENABLE_PROGRESS_BARS", "false".to_owned()),
            ("YARN_DISABLE_SELF_UPDATE_CHECK", "true".to_owned()),
         ]
      },
      Manager::Bun => {
         vec![
            (
               "BUN_INSTALL_CACHE_DIR",
               format!("{}/bun-cache", ctx.build.display()),
            ),
            ("BUN_INSTALL", format!("{}/bun-home", ctx.build.display())),
            ("DO_NOT_TRACK", "1".to_owned()),
         ]
      },
   };
   let mut env = match manager {
      Manager::Pnpm => {
         settings
            .into_iter()
            .flat_map(|(name, value)| {
               [
                  (format!("npm_config_{name}"), value.clone()),
                  (format!("pnpm_config_{name}"), value),
               ]
            })
            .collect::<Vec<_>>()
      },
      Manager::Npm | Manager::Yarn | Manager::Bun => {
         settings
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect()
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
   let (options, _compile) = options(manager, ctx)?;
   let vars = manager_env(manager, ctx, &options);
   let env_vars = vars
      .iter()
      .map(|pair| (pair.0.as_str(), pair.1.as_str()))
      .collect::<Vec<(&str, &str)>>();
   prepare_install(manager, ctx, &options)?;
   let mut args = install_args(manager).to_vec();
   args.extend(strs(&options.flags));
   run(manager.name(), &args, &env_vars)?;
   let modules = Path::new("node_modules");
   if modules.try_exists()? {
      shebangs(modules, false)?;
   }

   let mut path = vec![format!("{}/node_modules/.bin", current_dir()?.display())];
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
   let (options, compile) = options(manager, ctx)?;
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
         match manager {
            Manager::Yarn => args.push("--offline"),
            Manager::Bun if own_test_script()? => args.insert(0, "run"),
            Manager::Npm | Manager::Pnpm | Manager::Bun => {},
         }
         run(name, &args, &[])
      },
      "install" => install(manager, ctx, &options, &compile),
      _ => Err(unknown(name, verb)),
   }
}

fn install(
   manager: Manager,
   ctx: &Ctx,
   options: &Options,
   compile: &BTreeMap<String, String>,
) -> io::Result<()> {
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
         if !compile.is_empty() {
            let bin = format!("{}/bin", ctx.out.display());
            create_dir_all(&bin)?;
            for (exe, entry) in compile {
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

/// `bun test` is bun's own runner where the other managers run the package's
/// `test` script, so a package with one gets `bun run test`.
fn own_test_script() -> io::Result<bool> {
   let package = serde_json::from_str::<PackageJson>(&read_to_string("package.json")?)?;
   Ok(package.scripts.contains_key("test"))
}

/// `bin` as a string means one executable named like the package, npm strips
/// a scope.
fn install_tree(ctx: &Ctx) -> io::Result<()> {
   let tree = format!(
      "{}/lib/node_modules/{}",
      ctx.out.display(),
      ctx.spec()?.name
   );
   create_dir_all(format!("{}/lib/node_modules", ctx.out.display()))?;
   create_dir_all(format!("{}/bin", ctx.out.display()))?;
   succeed(Command::new("cp").args(["-r", ".", &tree]))?;
   let package = serde_json::from_str::<PackageJson>(&read_to_string("package.json")?)?;
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
      symlink(
         format!("{tree}/{path}"),
         format!("{}/bin/{name}", ctx.out.display()),
      )?;
   }
   Ok(())
}
