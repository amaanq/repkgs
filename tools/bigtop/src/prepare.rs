use std::{
   collections::BTreeMap,
   env::var,
   fs::{
      File,
      create_dir_all,
      set_permissions,
   },
   io::{
      self,
      Write as _,
   },
   os::unix::fs::PermissionsExt,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
   },
   thread::available_parallelism,
};

use mummer::{
   process::{
      Checked as _,
      socket,
   },
   store::normalize,
   tree::{
      note,
      shebangs,
   },
};
use serde::{
   Deserialize as _,
   Serialize,
};
use serde_json::{
   Value,
   json,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      Platform,
      build_top,
   },
   env::compute,
   exports::closure,
};

#[derive(Serialize)]
pub struct Prepared {
   pub env: BTreeMap<String, Value>,
   pub cwd: String,
   pub ctx: Ctx,
}

#[derive(Clone, Copy)]
pub enum Source<'path> {
   Unpack,
   Archive(&'path Path),
   Existing,
}

pub fn run(from_tree: Option<&Path>) -> io::Result<()> {
   let prepared = prepare(from_tree.map_or(Source::Unpack, Source::Archive))?;
   let mut stdout = io::stdout().lock();
   serde_json::to_writer(&mut stdout, &prepared)?;
   writeln!(stdout)?;
   Ok(())
}

pub fn prepare(source: Source<'_>) -> io::Result<Prepared> {
   let attrs = Attrs::load()?;
   let spec = attrs.spec()?;
   let top = build_top()?;
   let out = match source {
      Source::Unpack | Source::Existing => format!("{top}/prefix"),
      Source::Archive(_) => attrs.package.clone(),
   };
   let njobs = match var("NIX_BUILD_CORES")
      .ok()
      .and_then(|count| count.parse::<usize>().ok())
      .filter(|count| *count > 0)
   {
      Some(count) => count,
      None => available_parallelism()?.get(),
   };
   let deps = closure(&attrs.dependencies)?;
   let cache = socket().try_exists()?;
   let mut env = compute(&attrs, &spec, &deps, Path::new(&out), cache)?;
   env.insert(
      "PKGS_RESULT".to_owned(),
      Value::String(attrs.outputs.out.clone()),
   );
   let platform = resolve_platform(&attrs.platform, &top, &mut env)?;
   let src = format!("{top}/source");
   let build = format!("{top}/build");
   if matches!(source, Source::Existing) && !Path::new(&src).is_dir() {
      return Err(io::Error::other(
         "the development source directory is missing",
      ));
   }
   if !matches!(source, Source::Archive(_)) {
      create_dir_all(&src)?;
      create_dir_all(&build)?;
   }

   match source {
      Source::Unpack => unpack(&attrs, &src)?,
      Source::Archive(tree) => restore(tree, &top)?,
      Source::Existing => {},
   }
   let cwd = normalize(&Path::new(&src).join(spec.root.as_deref().unwrap_or(".")));
   let facts = Platform::deserialize(&platform)?;
   let restored = matches!(source, Source::Archive(_));
   if !restored && spec.tests.run.unwrap_or(true) && facts.cross && !facts.transparent {
      note(
         "untested",
         &format!("{}: no binfmt on this builder", facts.name),
      );
   }
   let tests_run =
      restored || (spec.tests.run.unwrap_or(true) && (!facts.cross || facts.transparent));
   let roots = env
      .get("MUMMER_STORE_ROOTS")
      .and_then(Value::as_str)
      .unwrap_or_default()
      .split_whitespace()
      .map(str::to_owned)
      .collect();
   let ctx = Ctx {
      spec: attrs.spec.clone(),
      out: PathBuf::from(out),
      dest: PathBuf::from(&attrs.outputs.out),
      deps,
      roots,
      njobs,
      src: cwd.clone(),
      build: PathBuf::from(build),
      platform,
      tests_run,
      cache,
   };
   ctx.save(&top)?;
   Ok(Prepared {
      env,
      cwd: cwd.to_string_lossy().into_owned(),
      ctx,
   })
}

fn resolve_platform(
   platform: &Value,
   top: &str,
   env: &mut BTreeMap<String, Value>,
) -> io::Result<Value> {
   let mut resolved = platform.clone();
   let facts = Platform::deserialize(platform)?;
   let cross = facts.cross;
   // qemu-user can map V8's code range above 2^48, where arm64 PAC stores
   // its signature, so signed return addresses stop authenticating.
   // Reserve 16 TiB to leave room for V8 while fitting a 47-bit host.
   // binfmt-registered qemu reads QEMU_RESERVED_VA too.
   let reserved_va = (cross && facts.cpu == "aarch64").then_some("0x100000000000");
   if let Some(limit) = reserved_va {
      env.insert(
         "QEMU_RESERVED_VA".to_owned(),
         Value::String(limit.to_owned()),
      );
   }
   if facts.os == "windows" {
      env.extend([
         (
            "WINEPREFIX".to_owned(),
            Value::String(format!("{top}/wine")),
         ),
         (
            "WINEDEBUG".to_owned(),
            Value::String("-all,err+module".to_owned()),
         ),
         (
            "WINEDLLOVERRIDES".to_owned(),
            Value::String("mscoree,mshtml=".to_owned()),
         ),
      ]);
   }

   let transparent = if cross {
      match Command::new(&facts.probe)
         .arg("--version")
         .envs(reserved_va.map(|limit| ("QEMU_RESERVED_VA", limit)))
         .output()
      {
         Ok(output) => {
            if !output.status.success() {
               return Err(io::Error::other(format!(
                  "{} execution probe failed with {}\n{}",
                  facts.name,
                  output.status,
                  String::from_utf8_lossy(&output.stderr)
               )));
            }
            true
         },
         Err(_) => false,
      }
   } else {
      false
   };
   if cross {
      note("platform", &format!("{} binfmt={transparent}", facts.name));
   }
   if transparent {
      resolved["emulator"] = json!([]);
   }
   resolved["transparent"] = json!(transparent);
   for (name, value) in build_machine_tools(&facts.build_rust_triple, top, cross)? {
      env.insert(name, Value::String(value));
   }
   let emulator = if transparent {
      String::new()
   } else {
      facts.emulator.join(" ")
   };
   env.insert("PKGS_EMULATOR".to_owned(), Value::String(emulator));
   Ok(resolved)
}

fn build_machine_tools(
   build_rust_triple: &str,
   top: &str,
   cross: bool,
) -> io::Result<Vec<(String, String)>> {
   if !cross {
      return Ok(vec![
         ("CC_FOR_BUILD".to_owned(), "cc".to_owned()),
         ("CXX_FOR_BUILD".to_owned(), "c++".to_owned()),
         ("CPP_FOR_BUILD".to_owned(), "cc -E".to_owned()),
         ("PKG_CONFIG_FOR_BUILD".to_owned(), "pkg-config".to_owned()),
      ]);
   }
   let dir = format!("{top}/for-build");
   create_dir_all(format!("{dir}/no-pc"))?;
   let script = PathBuf::from(format!("{dir}/pkg-config"));
   File::create(&script)?.write_all(
      format!("#!/bin/sh\nPKG_CONFIG_PATH= PKG_CONFIG_LIBDIR={dir}/no-pc exec pkg-config \"$@\"\n")
         .as_bytes(),
   )?;
   set_permissions(&script, PermissionsExt::from_mode(0o755))?;
   let triple = build_rust_triple.replace('-', "_");
   Ok(vec![
      ("CC_FOR_BUILD".to_owned(), "cc-build".to_owned()),
      ("CXX_FOR_BUILD".to_owned(), "c++-build".to_owned()),
      ("CPP_FOR_BUILD".to_owned(), "cc-build -E".to_owned()),
      (
         "PKG_CONFIG_FOR_BUILD".to_owned(),
         script.to_string_lossy().into_owned(),
      ),
      ("CFLAGS_FOR_BUILD".to_owned(), String::new()),
      ("CXXFLAGS_FOR_BUILD".to_owned(), String::new()),
      ("CPPFLAGS_FOR_BUILD".to_owned(), String::new()),
      ("LDFLAGS_FOR_BUILD".to_owned(), String::new()),
      (format!("CC_{triple}"), "cc-build".to_owned()),
      (format!("CXX_{triple}"), "c++-build".to_owned()),
      (
         format!("PKG_CONFIG_{triple}"),
         script.to_string_lossy().into_owned(),
      ),
   ])
}

// cp -p keeps the store's uniform mtimes so generated files stay newer than
// their inputs for make, patch -F0 refuses a hunk whose context moved
fn unpack(attrs: &Attrs, src: &str) -> io::Result<()> {
   note("unpack", &attrs.src);
   succeed(
      Command::new("cp")
         .arg("-rp")
         .arg(format!("{}/.", attrs.src))
         .arg(src),
   )?;
   succeed(Command::new("chmod").arg("-R").arg("u+w").arg(src))?;
   for patch in &attrs.patches {
      note("patch", patch);
      succeed(
         Command::new("patch")
            .args(["-p1", "-F0", "-i"])
            .arg(patch)
            .current_dir(src),
      )?;
   }
   shebangs(Path::new(src), false)
}

fn restore(tree: &Path, top: &str) -> io::Result<()> {
   note("restore", &tree.to_string_lossy());

   for directory in ["source", "build"] {
      succeed(
         Command::new("cp")
            .arg("-a")
            .arg(tree.join(directory))
            .arg(top),
      )?;
   }

   succeed(
      Command::new("chmod")
         .args(["-R", "u+w"])
         .arg(Path::new(top).join("source"))
         .arg(Path::new(top).join("build")),
   )
}

pub fn succeed(command: &mut Command) -> io::Result<()> {
   command.stdout(Stdio::from(io::stderr())).checked()
}
