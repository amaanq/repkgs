use std::{
   collections::BTreeMap,
   env::var,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
      Write as _,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
   },
};

use jig::{
   files::write_file,
   tree::note,
};
use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
      Def,
      Platform,
      Profile,
   },
   system::{
      command::{
         run,
         strs,
         unknown,
      },
      dispatch::System,
   },
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   root:       String,
   defs:       BTreeMap<String, Def>,
   flags:      Vec<String>,
   skip_tests: Vec<String>,
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
   fn name(&self) -> &'static str {
      "meson"
   }

   fn workdir(&self, ctx: &Ctx) -> io::Result<PathBuf> {
      Ok(PathBuf::from(&ctx.build))
   }

   fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options: Options = ctx.options("meson")?;
      match verb {
         "configure" => Self::configure(ctx, &options),
         "build" => run("ninja", &[&format!("-j{}", ctx.njobs)], &[]),
         "test" => Self::test(ctx, &options),
         "install" => run("meson", &["install", "--no-rebuild"], &[]),
         _ => Err(unknown("meson", verb)),
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
      if !platform.emulator.is_empty() {
         binaries.push(("exe_wrapper", Literal::List(platform.emulator.clone())));
      }
      let host = Self::machine_file(&format!("{}/cross.ini", ctx.build), &[
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
         ("host_machine", vec![
            ("system", Self::text(&platform.os_names.meson)),
            ("kernel", Self::text(&platform.os_names.meson_kernel)),
            ("cpu_family", Self::text(&platform.names.meson)),
            ("cpu", Self::text(&platform.cpu)),
            ("endian", Self::text(&platform.endian)),
         ]),
      ])?;
      let native =
         Self::machine_file(&format!("{}/native.ini", ctx.build), &[("binaries", vec![
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
         ])])?;
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
      let buildtype = match ctx.spec()?.profile {
         Profile::Debug => "debug",
         Profile::Release => "plain",
      };
      let mut defs = vec![
         ("prefix".to_owned(), Def::Text(ctx.out.clone())),
         ("libdir".to_owned(), Def::Text("lib".to_owned())),
         ("buildtype".to_owned(), Def::Text(buildtype.to_owned())),
         ("default_library".to_owned(), Def::Text("shared".to_owned())),
         ("wrap_mode".to_owned(), Def::Text("nodownload".to_owned())),
         ("auto_features".to_owned(), Def::Text("disabled".to_owned())),
      ];
      for (name, value) in &options.defs {
         match defs.iter_mut().find(|entry| entry.0 == *name) {
            Some(entry) => entry.1 = value.clone(),
            None => defs.push((name.clone(), value.clone())),
         }
      }
      let source = ctx.project_dir(&options.root);
      let machines = if platform.cross {
         Self::cross_files(ctx, &platform)?
      } else {
         Vec::new()
      };
      let rendered: Vec<String> = defs
         .iter()
         .map(|entry| format!("-D{}={}", entry.0, Self::render(&entry.1)))
         .collect();
      let source_text = source.to_string_lossy();
      let mut args = vec!["setup", ".", &source_text];
      args.extend(strs(&machines));
      args.extend(strs(&rendered));
      args.extend(strs(&options.flags));
      run("meson", &args, &[])
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
      if !options.skip_tests.is_empty() {
         let output = Command::new("meson")
            .args(["test", "--list"])
            .stderr(Stdio::inherit())
            .output()?;
         if !output.status.success() {
            return Err(io::Error::other("meson test --list failed"));
         }
         kept.extend(
            String::from_utf8_lossy(&output.stdout)
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
