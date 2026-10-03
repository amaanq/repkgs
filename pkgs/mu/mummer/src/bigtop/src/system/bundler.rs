use std::{
   collections::BTreeMap,
   env::var,
   fs::{
      copy,
      create_dir_all,
      remove_dir_all,
      remove_file,
      symlink_metadata,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use mummer::{
   files::{
      glob,
      write_file,
   },
   tree::{
      note,
      shebangs,
   },
};

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
         dep_root,
         run,
         strs,
         unknown,
      },
      dispatch::System,
      syslibs,
   },
};

options! {
   Options {
      root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      deps ("deps"): PackagePath
         => "the locked dependencies, fetched (fetch.gems) from the package's own lock file",
      without ("without"): Vec<String> = vec!["development".to_owned(), "test".to_owned()]
         => "Gemfile groups to leave out",
      test ("test"): Option<Vec<String>> = None
         => "command run under `bundle exec` as the test (null runs none, test gems are usually in `without`)",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for bundle install",
   }
}

fn app_dir(ctx: &Ctx) -> io::Result<String> {
   Ok(format!("{}/lib/{}", ctx.out, ctx.spec()?.name))
}

pub struct Bundler;

impl System for Bundler {
   fn name(&self) -> &'static str {
      "bundler"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   fn workdir(&self, ctx: &Ctx) -> io::Result<PathBuf> {
      Ok(PathBuf::from(app_dir(ctx)?))
   }

   /// bundler is configured entirely through BUNDLE_* env, so there is no
   /// .bundle/config to clean up afterwards.
   fn setup(&self, ctx: &Ctx, attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      let options: Options = ctx.options("bundler")?;
      let app = app_dir(ctx)?;
      succeed(
         Command::new("cp").args([
            "-r",
            &format!("{}/.", ctx.project_dir(&options.root).display()),
            &app,
         ]),
         "cp",
      )?;
      create_dir_all("vendor")?;
      succeed(
         Command::new("cp").args([
            "-rL",
            &format!("{}/vendor/cache", options.deps),
            "vendor/cache",
         ]),
         "cp",
      )?;
      succeed(
         Command::new("cp").args([
            "-f",
            &format!("{}/Gemfile.lock", options.deps),
            "Gemfile.lock",
         ]),
         "cp",
      )?;
      succeed(
         Command::new("chmod").args(["-R", "u+w", "vendor", "Gemfile.lock"]),
         "chmod",
      )?;
      let mut env = BTreeMap::from([
         ("BUNDLE_PATH".to_owned(), format!("{app}/vendor/bundle")),
         (
            "BUNDLE_CACHE_PATH".to_owned(),
            format!("{app}/vendor/cache"),
         ),
         ("BUNDLE_FROZEN".to_owned(), "true".to_owned()),
         ("BUNDLE_WITHOUT".to_owned(), options.without.join(":")),
         ("BUNDLE_JOBS".to_owned(), ctx.njobs.to_string()),
         ("BUNDLE_RETRY".to_owned(), "0".to_owned()),
         (
            "BUNDLE_USER_HOME".to_owned(),
            format!("{}/bundle-home", ctx.build),
         ),
         ("GEM_HOME".to_owned(), format!("{}/gem-home", ctx.build)),
         ("MAKEFLAGS".to_owned(), format!("-j{}", ctx.njobs)),
      ]);
      syslibs::check(attrs, Path::new(&app), ctx.spec()?.sys.as_deref())?;
      env.extend(syslibs::active(attrs, "gems", &ctx.deps)?.env);
      gem_build_env(attrs, ctx, &mut env)?;
      cross_rbconfig(ctx, &mut env)?;
      Ok(env.into())
   }

   fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options: Options = ctx.options("bundler")?;
      match verb {
         "build" => build(&options),
         "test" => {
            let Some(ref command) = options.test else {
               return Ok(());
            };
            let mut args = vec!["exec"];
            args.extend(strs(command));
            run("bundle", &args, &[])
         },
         "install" => install(ctx, &options),
         _ => Err(unknown("bundler", verb)),
      }
   }
}

/// mkmf runs on the host ruby but must describe the target, so the target's
/// rbconfig.rb goes first on RUBYLIB, copied out so the host ruby does not pick
/// up the target's .so files beside it.
fn cross_rbconfig(ctx: &Ctx, env: &mut BTreeMap<String, String>) -> io::Result<()> {
   if !ctx.platform()?.cross {
      return Ok(());
   }
   let ruby = dep_root(ctx, "ruby", "extensions compile against the target ruby")?;
   let target = glob(&format!("{ruby}/lib/ruby/*/*/rbconfig.rb"))?
      .into_iter()
      .next()
      .ok_or_else(|| io::Error::other("the target ruby has no rbconfig.rb"))?;
   let dir = format!("{}/cross-rbconfig", ctx.build);
   create_dir_all(&dir)?;
   copy(&target, format!("{dir}/rbconfig.rb"))?;
   let mut rubylib = vec![dir];
   rubylib.extend(
      var("RUBYLIB")
         .unwrap_or_default()
         .split(':')
         .filter(|entry| !entry.is_empty())
         .map(str::to_owned),
   );
   env.insert("RUBYLIB".to_owned(), rubylib.join(":"));
   Ok(())
}

/// `BUNDLE_BUILD__<GEM>` is how `bundle config build.<gem> <flags>` reaches
/// `gem install` without a config file.
fn gem_build_env(attrs: &Attrs, ctx: &Ctx, env: &mut BTreeMap<String, String>) -> io::Result<()> {
   let flags = syslibs::active(attrs, "gems", &ctx.deps)?.flags;
   if flags.is_empty() {
      return Ok(());
   }
   note(
      "sys-libs",
      &flags.keys().cloned().collect::<Vec<_>>().join(" "),
   );
   for (gem, value) in flags {
      env.insert(
         format!("BUNDLE_BUILD__{}", gem.to_uppercase().replace('-', "___")),
         value,
      );
   }
   Ok(())
}

/// the .gem archives, bundler's download cache, the extension build logs and
/// the mkmf Makefiles, which embed the build dir, all go.
fn build(options: &Options) -> io::Result<()> {
   let mut args = vec!["install", "--local", "--no-cache"];
   args.extend(strs(&options.flags));
   run("bundle", &args, &[])?;
   let mut doomed = vec![PathBuf::from("vendor/cache")];
   doomed.extend(glob("vendor/bundle/ruby/*/cache")?);
   doomed.extend(glob(
      "vendor/bundle/ruby/*/extensions/**/{gem_make.out,mkmf.log}",
   )?);
   doomed.extend(glob("vendor/bundle/ruby/*/gems/*/ext/**/Makefile")?);
   for path in doomed {
      let Ok(found) = symlink_metadata(&path) else {
         continue;
      };
      if found.is_dir() {
         remove_dir_all(&path)?;
      } else {
         remove_file(&path)?;
      }
   }
   shebangs(Path::new("vendor/bundle"), false)
}

fn install(ctx: &Ctx, options: &Options) -> io::Result<()> {
   let app = app_dir(ctx)?;
   let ruby = dep_root(ctx, "ruby", "the bin stubs run the target ruby")?;
   let bin = format!("{}/bin", ctx.out);
   create_dir_all(&bin)?;
   let spec = ctx.spec()?;
   for name in spec.bin.iter().flatten() {
      let stub = bin_stub(ctx, options, name, &app, &ruby)?;
      write_file(
         Path::new(&format!("{bin}/{name}")),
         stub.as_bytes(),
         Some(0o755),
      )?;
   }
   Ok(())
}

/// a ruby script that activates the bundle and loads the application's own
/// executable, the app dir found from the stub's own location.
fn bin_stub(
   ctx: &Ctx,
   options: &Options,
   name: &str,
   root: &str,
   ruby: &str,
) -> io::Result<String> {
   let exe = ["exe", "bin"]
      .iter()
      .map(|dir| format!("{dir}/{name}"))
      .find(|rel| Path::new(root).join(rel).exists())
      .ok_or_else(|| io::Error::other(format!("{name} is neither exe/ nor bin/ of the app")))?;
   let rel_app = root
      .strip_prefix(&ctx.out)
      .unwrap_or(root)
      .trim_start_matches('/');
   Ok([
      format!("#!{ruby}/bin/ruby"),
      format!("app = File.expand_path('../{rel_app}', __dir__)"),
      "ENV['BUNDLE_GEMFILE'] = \"#{app}/Gemfile\"".to_owned(),
      "ENV['BUNDLE_PATH'] = \"#{app}/vendor/bundle\"".to_owned(),
      format!("ENV['BUNDLE_WITHOUT'] = '{}'", options.without.join(":")),
      "ENV['BUNDLE_FROZEN'] = 'true'".to_owned(),
      "require 'bundler/setup'".to_owned(),
      format!("load \"#{{app}}/{exe}\""),
   ]
   .join("\n"))
}
