use std::{
   collections::BTreeMap,
   env::var,
   ffi::OsString,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
      Write as _,
   },
   fs::read,
   io,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
   sync::OnceLock,
};

use mummer::{
   files::{
      utf8,
      write_file,
   },
   process::Checked as _,
   tree::note,
};
use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      Def,
      Platform,
      build_top,
   },
   schema::Field,
   system::{
      command::{
         run,
         strs,
         unknown,
         upsert,
      },
      dispatch::System,
   },
   test_cache,
   trace::{
      Commands,
      TestNames,
      Tests,
   },
};

options! {
   Options {
      root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      defs ("defs"): BTreeMap<String, Def> = BTreeMap::new()
         => "-D options, merged over prefix/libdir/buildtype defaults",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for meson setup",
      skip_tests ("skipTests"): Vec<String> = Vec::new()
         => "substrings of `meson test --list` names",
   }
}

enum Literal {
   Text(String),
   List(Vec<String>),
   Flag(bool),
   Int(u8),
}

impl Display for Literal {
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      match *self {
         Self::Text(ref text) => write!(f, "'{text}'"),
         Self::List(ref items) => {
            f.write_str("[")?;
            for (index, item) in items.iter().enumerate() {
               if index > 0 {
                  f.write_str(", ")?;
               }
               write!(f, "'{item}'")?;
            }
            f.write_str("]")
         },
         Self::Flag(flag) => write!(f, "{flag}"),
         Self::Int(int) => write!(f, "{int}"),
      }
   }
}

pub struct Meson;

impl System for Meson {
   fn module(&self) -> &'static str {
      module_path!()
   }

   fn name(&self) -> &'static str {
      "meson"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   fn phases(&self) -> &'static [&'static str] {
      &["configure", "build", "test", "install"]
   }

   fn workdir(&self, ctx: &Ctx) -> io::Result<PathBuf> {
      Ok(ctx.build.clone())
   }

   fn tests(&self) -> Option<&'static dyn Tests> {
      Some(&Self)
   }

   fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>(self.name())?;
      match verb {
         "configure" => Self::configure(ctx, &options),
         "build" => run("ninja", &[&format!("-j{}", ctx.njobs)], &[]),
         "test-prepare" => Self::list_tests(ctx),
         "test" => Self::test(ctx, &options),
         "install" => run("meson", &["install", "--no-rebuild"], &[]),
         _ => Err(unknown(self.name(), verb)),
      }
   }
}

impl Meson {
   fn text(value: &str) -> Literal {
      Literal::Text(value.to_owned())
   }

   fn machine_file(path: &str, sections: &[(&str, Vec<(&str, Literal)>)]) -> io::Result<String> {
      let mut contents = String::new();
      for (index, section) in sections.iter().enumerate() {
         if index > 0 {
            contents.push('\n');
         }
         writeln!(contents, "[{}]", section.0).map_err(io::Error::other)?;
         for entry in &section.1 {
            writeln!(contents, "{} = {}", entry.0, entry.1).map_err(io::Error::other)?;
         }
      }
      write_file(Path::new(path), contents.trim_end().as_bytes(), None)?;
      Ok(path.to_owned())
   }

   /// the build machine's pkg-config finds nothing so build-time tools never
   /// link target libraries, and sizeof/alignment properties spare
   /// `cc.sizeof()` an exe wrapper.
   fn cross_files(ctx: &Ctx, platform: &Platform) -> io::Result<Vec<String>> {
      let word = platform.bits / 8;
      let long = if platform.os == "windows" { 4 } else { word };
      let mut binaries = vec![
         ("c", Self::text("cc")),
         ("cpp", Self::text("c++")),
         ("ar", Self::text("llvm-ar")),
         ("nm", Self::text("llvm-nm")),
         ("strip", Self::text("llvm-strip")),
         ("objcopy", Self::text("llvm-objcopy")),
         ("pkg-config", Self::text("pkg-config")),
         ("cmake", Self::text("cmake")),
      ];

      if platform.os == "macos" {
         binaries.extend([("objc", Self::text("cc")), ("objcpp", Self::text("c++"))]);
      }

      if !platform.emulator.is_empty() {
         binaries.push(("exe_wrapper", Literal::List(platform.emulator.clone())));
      }

      let mut machine = vec![
         ("system", Self::text(&platform.os_names.meson)),
         ("kernel", Self::text(&platform.os_names.meson_kernel)),
         ("cpu_family", Self::text(&platform.names.meson)),
         ("cpu", Self::text(&platform.cpu)),
         ("endian", Self::text(&platform.endian)),
      ];

      if platform.os == "macos" {
         machine.push(("subsystem", Self::text("macos")));
      }

      let host = Self::machine_file(&format!("{}/cross.ini", ctx.build.display()), &[
         ("binaries", binaries),
         ("properties", vec![
            ("needs_exe_wrapper", Literal::Flag(true)),
            ("sizeof_void_p", Literal::Int(word)),
            ("sizeof_long", Literal::Int(long)),
            ("sizeof_size_t", Literal::Int(word)),
            ("alignment_void_p", Literal::Int(word)),
            ("alignment_double", Literal::Int(8)),
            (
               "sys_root",
               Literal::Text(var("PKGS_SYSROOT").unwrap_or_default()),
            ),
            (
               "pkg_config_libdir",
               Literal::Text(var("PKG_CONFIG_PATH").unwrap_or_default()),
            ),
         ]),
         ("host_machine", machine),
      ])?;
      let native = Self::machine_file(&format!("{}/native.ini", ctx.build.display()), &[(
         "binaries",
         vec![
            ("c", Literal::Text(var("CC_FOR_BUILD").unwrap_or_default())),
            (
               "cpp",
               Literal::Text(var("CXX_FOR_BUILD").unwrap_or_default()),
            ),
            ("ar", Self::text("llvm-ar")),
            ("strip", Self::text("llvm-strip")),
            (
               "pkg-config",
               Literal::Text(var("PKG_CONFIG_FOR_BUILD").unwrap_or_default()),
            ),
         ],
      )])?;
      Ok(vec![
         "--cross-file".to_owned(),
         host,
         "--native-file".to_owned(),
         native,
      ])
   }

   fn render(def: &Def) -> String {
      match *def {
         Def::Flag(flag) => flag.to_string(),
         Def::Text(ref text) => text.clone(),
         Def::Number(number) => number.to_string(),
      }
   }

   /// buildtype plain because cc brings -O2 -g itself, `auto_features` disabled
   /// so packages enable what they declare.
   fn configure(ctx: &Ctx, options: &Options) -> io::Result<()> {
      let platform = ctx.platform()?;
      let mut defs = vec![
         ("prefix".to_owned(), Def::Text(utf8(&ctx.out)?.to_owned())),
         ("libdir".to_owned(), Def::Text("lib".to_owned())),
         ("sbindir".to_owned(), Def::Text("bin".to_owned())),
         ("buildtype".to_owned(), Def::Text("plain".to_owned())),
         ("default_library".to_owned(), Def::Text("shared".to_owned())),
         ("wrap_mode".to_owned(), Def::Text("nodownload".to_owned())),
         ("auto_features".to_owned(), Def::Text("disabled".to_owned())),
      ];
      for (name, value) in &options.defs {
         upsert(&mut defs, name, value.clone());
      }
      let source = ctx.project_dir(&options.root);
      let machines = if platform.cross {
         Self::cross_files(ctx, &platform)?
      } else {
         Vec::new()
      };
      let rendered = defs
         .iter()
         .map(|entry| format!("-D{}={}", entry.0, Self::render(&entry.1)))
         .collect::<Vec<String>>();
      let source_text = utf8(&source)?;
      let mut args = vec!["setup", ".", source_text];
      args.extend(strs(&machines));
      args.extend(strs(&rendered));
      args.extend(strs(&options.flags));
      run("meson", &args, &[])
   }

   /// Each test's command and working directory, a test without one running
   /// in the build directory, written where the test phase's tracer reads it.
   fn list_tests(ctx: &Ctx) -> io::Result<()> {
      let output = Command::new("meson")
         .args(["introspect", "--tests"])
         .checked_output()?;
      let root = utf8(&ctx.build)?;
      let listed = serde_json::from_slice::<Vec<Introspected>>(&output)?
         .into_iter()
         .map(|test| {
            Listed {
               cwd:  test.workdir.unwrap_or_else(|| root.to_owned()),
               name: test.name,
               cmd:  test.cmd,
            }
         })
         .collect::<Vec<_>>();
      write_file(
         &Path::new(&build_top()?).join(LISTING),
         &serde_json::to_vec(&listed)?,
         None,
      )
   }

   fn test(ctx: &Ctx, options: &Options) -> io::Result<()> {
      let jobs = ctx.test_jobs()?.to_string();
      let mut args = vec![
         "test",
         "--no-rebuild",
         "--print-errorlogs",
         "--num-processes",
         &jobs,
      ];
      let mut kept = Vec::new();
      if let Some(selected) = test_cache::selected::<TestNames>()? {
         kept.extend(selected.into_iter().flat_map(|unit| unit.names));
      } else if !options.skip_tests.is_empty() {
         let listed = Command::new("meson")
            .args(["test", "--list"])
            .checked_output()?;
         kept.extend(
            String::from_utf8_lossy(&listed)
               .lines()
               .filter(|name| !options.skip_tests.iter().any(|skip| name.contains(skip)))
               .map(str::to_owned),
         );
         if kept.is_empty() {
            note("meson-test", "every test matches meson.skipTests");
            return Ok(());
         }
      }
      args.extend(strs(&kept));
      run("meson", &args, &[])
   }
}

/// Where `test-prepare` leaves meson's tests.
const LISTING: &str = ".bigtop-meson-tests.json";

#[derive(Deserialize)]
struct Introspected {
   name:    String,
   cmd:     Vec<String>,
   workdir: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct Listed {
   name: String,
   cmd:  Vec<String>,
   cwd:  String,
}

fn commands() -> Option<&'static Commands> {
   static COMMANDS: OnceLock<Option<Commands>> = OnceLock::new();
   COMMANDS
      .get_or_init(|| {
         let bytes = read(Path::new(&build_top().ok()?).join(LISTING)).ok()?;
         let mut found = Commands::default();
         for test in serde_json::from_slice::<Vec<Listed>>(&bytes).ok()? {
            found.add(test.name, test.cmd, test.cwd);
         }
         Some(found)
      })
      .as_ref()
}

/// meson execs a test's command as introspected, so a test's process runs
/// it, or runs its interpreter when the command is a script.
impl Tests for Meson {
   fn unit(&self, _exe: &Path, args: &[OsString], cwd: &Path) -> Option<String> {
      commands()?.unit(args, cwd)
   }

   fn prepares(&self) -> bool {
      true
   }

   /// The logs every meson test run rewrites.
   fn state(&self, path: &Path) -> bool {
      path
         .ancestors()
         .any(|directory| directory.ends_with("meson-logs"))
   }
}
