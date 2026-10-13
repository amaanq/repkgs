use std::{
   collections::HashSet,
   env::var,
   ffi::OsString,
   fs::{
      File,
      copy,
      metadata,
      read,
      remove_file,
   },
   io::{
      self,
      Write as _,
   },
   iter::once,
   path::{
      Path,
      PathBuf,
   },
};

use mummer::{
   files::{
      glob,
      utf8,
      write_file,
   },
   process::loose,
   tree::{
      EntryKind,
      find,
      note,
      replace_all,
      walk,
      writable,
   },
};
use serde::Serialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   probe,
   schema::{
      Condition,
      Field,
      Unsupported,
   },
   site::site,
   system::{
      command::{
         Env,
         run,
         strs,
         unknown,
      },
      dispatch::System,
      make::{
         self,
         AutomakeTest,
         shell,
      },
   },
   trace::Tests,
};

options! {
   Options {
      root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for configure",
      make_flags ("makeFlags"): Vec<String> = Vec::new()
         => "arguments for every make invocation (build, test, install)",
      install_flags ("installFlags"): Vec<String> = Vec::new()
         => "arguments for `make install` only",
      build_target ("buildTarget"): Vec<String> = Vec::new()
         => "make goals for build (empty means the makefile's default goal)",
      test_target ("testTarget"): Vec<String> = vec!["check".to_owned()]
         => "make goals for test",
      install_target ("installTarget"): Vec<String> = vec!["install".to_owned()]
         => "make goals for install",
      configure_script ("configureScript"): String = "configure".to_owned()
         => "configure script relative to the project",
      out_of_tree ("outOfTree"): bool = true
         => "configure from a separate build directory",
   }
}

#[derive(Serialize)]
struct Extra<'options> {
   script: &'options str,
   flags:  &'options [String],
   host:   &'options [String],
}

pub struct Autotools;

impl System for Autotools {
   fn module(&self) -> &'static str {
      module_path!()
   }

   fn name(&self) -> &'static str {
      "autotools"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   fn phases(&self) -> &'static [&'static str] {
      &["configure", "build", "test", "install"]
   }

   fn unsupported(&self) -> Option<Unsupported> {
      Some(Unsupported::new(
         Condition::Libc("msvc".to_owned()),
         "configure and libtool do not know the MSVC ABI, config.sub rejects the triple",
      ))
   }

   fn workdir(&self, ctx: &Ctx) -> io::Result<PathBuf> {
      let options = ctx.options::<Options>(self.name())?;
      Ok(if options.out_of_tree {
         ctx.build.clone()
      } else {
         ctx.project_dir(&options.root)
      })
   }

   fn setup(&self, _ctx: &Ctx, _attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      Ok(shell()?.into())
   }

   fn tests(&self) -> Option<&'static dyn Tests> {
      Some(&Self)
   }

   fn phase(&self, ctx: &Ctx, attrs: &Attrs, verb: &str, socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>(self.name())?;
      match verb {
         "configure" => Self::configure(ctx, attrs, &options, socket),
         "build" => make::build(ctx, &options.make_flags, &options.build_target),
         "test" => make::test(ctx, &options.make_flags, &options.test_target),
         "install" => {
            let mut flags = options.make_flags;
            flags.extend(options.install_flags);
            make::install(ctx, &flags, &options.install_target)?;
            Self::unrecord_install_tool(ctx)
         },
         _ => Err(unknown(self.name(), verb)),
      }
   }
}

/// automake's parallel harness runs every test under a driver given
/// `--test-name`, which `make TESTS=` in the driver's directory runs alone.
impl Tests for Autotools {
   fn unit(&self, _exe: &Path, args: &[OsString], cwd: &Path) -> Option<String> {
      let at = args.iter().position(|arg| arg == "--test-name")?;
      let test = AutomakeTest {
         directory: cwd.to_str()?.to_owned(),
         name:      args.get(at + 1)?.to_str()?.to_owned(),
      };
      Some(test.to_string())
   }
}

impl Autotools {
   fn build_dir(ctx: &Ctx, options: &Options) -> PathBuf {
      if options.out_of_tree {
         ctx.build.clone()
      } else {
         ctx.project_dir(&options.root)
      }
   }

   fn configure(ctx: &Ctx, attrs: &Attrs, options: &Options, socket: &Path) -> io::Result<()> {
      let located = ctx
         .project_dir(&options.root)
         .join(&options.configure_script);
      let script = utf8(&located)?.to_owned();
      let platform = ctx.platform()?;
      // natively both name the target, so configure still runs its test
      // programs. The bootstrap's build triple differs from every
      // target's on purpose and would read as cross
      let build = if platform.cross {
         &platform.build_triple
      } else {
         &platform.gnu_triple
      };
      let host_flags = vec![
         format!("--host={}", platform.gnu_triple),
         format!("--build={build}"),
      ];
      let cache = ctx.build.join("config.cache");
      let sites = var("CONFIG_SITE").unwrap_or_default();
      let scripts = once(script.as_str())
         .chain(sites.split_whitespace())
         .collect::<Vec<&str>>();
      let key = probe::key(
         "autotools",
         &scripts,
         Some(&Extra {
            script: &options.configure_script,
            flags:  &options.flags,
            host:   &host_flags,
         }),
      )?;
      let restored = probe::restore(&key, &cache, socket)?;
      note("config.cache", if restored { "restored" } else { "cold" });
      Self::backports(&ctx.src)?;
      copy(
         Path::new(&attrs.peanut).join("bin/install"),
         Self::install_tool(ctx),
      )?;
      let mut args = vec![
         script.as_str(),
         "--disable-nls",
         "--disable-dependency-tracking",
         "--disable-static",
         "--enable-shared",
      ];
      args.extend(strs(&host_flags));
      args.extend(strs(&options.flags));
      let install = format!("{} -c", Self::install_tool(ctx).display());
      let config_site = site(ctx, Path::new(&script))?;
      let env = [
         ("PKGS_PREFIX", utf8(&ctx.out)?),
         ("PKGS_CONFIG_CACHE", utf8(&cache)?),
         ("INSTALL", &install),
         ("CONFIG_SITE", &config_site),
      ];
      let shell = var("CONFIG_SHELL").unwrap_or_default();
      let workdir = Self::build_dir(ctx, options);
      run(&shell, &args, &env)?;

      let status = workdir.join("config.status");
      let reusable = restored
         || !ctx.cache
         || match read(&status) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => return Err(error),
            Ok(first) => {
               run(&shell, &args, &env)?;
               let second = read(&status)?;
               Self::note_lost_lines(&first, &second);
               second == first
            },
         };
      if reusable {
         probe::store(&key, &cache, socket)?;
      } else {
         note(
            "config.cache",
            "answers change what configure writes, not cached",
         );
         remove_file(&cache)?;
         run(&shell, &args, &env)?;
      }

      if !platform.posix {
         Self::stub_gnulib_tests(&workdir, &platform.os)?;
      }
      Ok(())
   }

   /// Names the config.status lines a cached run lost, which point at the
   /// check whose answer replays without its side effect.
   fn note_lost_lines(first: &[u8], second: &[u8]) {
      let fresh = loose(first);
      let replayed = loose(second);
      let kept = replayed.lines().collect::<HashSet<_>>();
      for line in fresh.lines().filter(|line| !kept.contains(line)).take(3) {
         note(
            "config.cache",
            &format!(
               "cached run lost {}",
               line.chars().take(160).collect::<String>()
            ),
         );
      }
   }

   const BACKPORTS: &[(&str, &[u8], &[u8])] = &[
      // gnulib's gettext.h without NLS is `((void) d, gettext (s))`, clang sees no
      // string literal behind the comma operator and -Werror=format-security
      // rejects every _("...")
      (
         "**/gettext.h",
         b"((void) (Domainname), gettext (Msgid))",
         b"gettext (Msgid)",
      ),
      (
         "**/gettext.h",
         b"((void) (Category), dgettext (Domainname, Msgid))",
         b"dgettext (Domainname, Msgid)",
      ),
      (
         "**/configure",
         b"    -L* | -R* | -l*)\n       # Some compilers place",
         b"    -L* | -R* | -l* | */libclang_rt.*.a)\n       # Some compilers place",
      ),
      (
         "**/ltmain.sh",
         b"\t    # Linking convenience modules into shared libraries is allowed,\n",
         b"\t    case $deplib in */libgcc*.$libext | */libclang_rt*.$libext) deplibs=\"$deplib $deplibs\"; continue ;; esac\n\t    # Linking convenience modules into shared libraries is allowed,\n",
      ),
      (
         "**/config.sub",
         b"| riscv | riscv32 | riscv64 \\",
         b"| loongarch32 | loongarch64 | riscv | riscv32 | riscv64 \\",
      ),
   ];

   fn backports(src: &Path) -> io::Result<()> {
      for &(files, old, new) in Self::BACKPORTS {
         for path in glob(&format!("{}/{files}", src.display()))? {
            if !path.is_file() {
               continue;
            }
            let bytes = read(&path)?;
            if find(&bytes, old).is_none() {
               continue;
            }
            let mtime = metadata(&path)?.modified()?;
            writable(&path)?;
            let mut file = File::create(&path)?;
            file.write_all(&replace_all(&bytes, old, new))?;
            file.set_modified(mtime)?;
         }
      }
      Ok(())
   }

   fn stub_gnulib_tests(build: &Path, os: &str) -> io::Result<()> {
      const STUB: &[u8] =
         b"all install check clean distclean:\n\t@:\n.PHONY: all install check clean distclean\n";
      for path in glob(&format!(
         "{}/{{gnulib-tests,tests}}/Makefile",
         build.display()
      ))? {
         if !path.is_file() || find(&read(&path)?, b"test-nanosleep").is_none() {
            continue;
         }
         writable(&path)?;
         write_file(&path, STUB, None)?;
         let dir = path
            .strip_prefix(build)
            .map_err(io::Error::other)?
            .parent()
            .map(|parent| parent.display().to_string())
            .unwrap_or_default();
         note("gnulib-tests", &format!("{dir} skipped on {os}"));
      }
      Ok(())
   }

   /// configure's INSTALL.
   fn install_tool(ctx: &Ctx) -> PathBuf {
      ctx.build.join("install")
   }

   fn unrecord_install_tool(ctx: &Ctx) -> io::Result<()> {
      let recorded = Self::install_tool(ctx);
      let needle = recorded.as_os_str().as_encoded_bytes();
      let out = &ctx.out;
      if !out.exists() {
         return Ok(());
      }
      for entry in walk(out)? {
         if entry.kind != EntryKind::File {
            continue;
         }
         let path = Path::new(&entry.path);
         let bytes = read(path)?;
         if bytes.contains(&0) || find(&bytes, needle).is_none() {
            continue;
         }
         writable(path)?;
         write_file(path, &replace_all(&bytes, needle, b"install"), None)?;
      }
      Ok(())
   }
}
