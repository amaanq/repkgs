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

use jig::{
   store::normalize,
   tree::{
      note,
      shebangs,
   },
};
use serde::Serialize;
use serde_json::{
   Value,
   json,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      build_top,
      flag,
      text,
      words,
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

pub fn run(from_tree: Option<&Path>) -> io::Result<()> {
   let prepared = prepare(from_tree)?;
   let mut stdout = io::stdout().lock();
   serde_json::to_writer(&mut stdout, &prepared)?;
   writeln!(stdout)?;
   Ok(())
}

pub fn prepare(from_tree: Option<&Path>) -> io::Result<Prepared> {
   let attrs = Attrs::load()?;
   let spec = attrs.spec()?;
   let top = build_top()?;
   let store = var("NIX_STORE").map_err(|_unset| io::Error::other("NIX_STORE is not set"))?;
   let out = match from_tree {
      None => format!("{top}/prefix"),
      Some(_) => attrs.package.clone(),
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
   let cache = Path::new(&store)
      .parent()
      .unwrap_or_else(|| Path::new("/"))
      .join("var/nix/jigd/socket")
      .exists();
   let mut env = compute(&attrs, &spec, &deps, Path::new(&out), cache)?;
   env.insert(
      "PKGS_RESULT".to_owned(),
      Value::String(attrs.outputs.out.clone()),
   );
   let platform = resolve_platform(&attrs.platform, &top, &mut env)?;
   let src = format!("{top}/source");
   let build = format!("{top}/build");
   create_dir_all(&src)?;
   create_dir_all(&build)?;
   match from_tree {
      None => unpack(&attrs, &src)?,
      Some(tree) => restore(tree, &top)?,
   }
   let cwd = normalize(&Path::new(&src).join(spec.root.as_deref().unwrap_or(".")));
   let cross = flag(&platform, "cross");
   let transparent = flag(&platform, "transparent");
   if from_tree.is_none() && spec.tests.run && cross && !transparent {
      note(
         "untested",
         &format!("{}: no binfmt on this builder", text(&platform, "name")),
      );
   }
   let tests_run = from_tree.is_some() || (spec.tests.run && (!cross || transparent));
   let roots = env
      .get("JIG_STORE_ROOTS")
      .and_then(Value::as_str)
      .unwrap_or_default()
      .split_whitespace()
      .map(str::to_owned)
      .collect();
   let ctx = Ctx {
      spec: attrs.spec.clone(),
      out,
      dest: attrs.outputs.out.clone(),
      deps,
      roots,
      njobs,
      src: cwd.to_string_lossy().into_owned(),
      build,
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
   let cross = flag(platform, "cross");
   // qemu-user can map V8's code range above 2^48, where arm64 PAC stores
   // its signature, so signed return addresses stop authenticating.
   // Reserve 16 TiB to leave room for V8 while fitting a 47-bit host.
   // binfmt-registered qemu reads QEMU_RESERVED_VA too.
   let reserved_va = (cross && text(platform, "cpu") == "aarch64").then_some("0x100000000000");
   if let Some(limit) = reserved_va {
      env.insert(
         "QEMU_RESERVED_VA".to_owned(),
         Value::String(limit.to_owned()),
      );
   }
   let transparent = if cross {
      match Command::new(text(platform, "probe"))
         .arg("--version")
         .envs(reserved_va.map(|limit| ("QEMU_RESERVED_VA", limit)))
         .output()
      {
         Ok(output) => {
            if !output.status.success() {
               return Err(io::Error::other(format!(
                  "{} execution probe failed with {}\n{}",
                  text(platform, "name"),
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
      note(
         "platform",
         &format!("{} binfmt={transparent}", text(platform, "name")),
      );
   }
   if transparent {
      resolved["emulator"] = json!([]);
   }
   resolved["transparent"] = json!(transparent);
   for (name, value) in build_machine_tools(&resolved, top, cross)? {
      env.insert(name, Value::String(value));
   }
   env.insert(
      "PKGS_EMULATOR".to_owned(),
      Value::String(words(&resolved, "emulator").join(" ")),
   );
   Ok(resolved)
}

fn build_machine_tools(
   platform: &Value,
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
   let triple = text(platform, "buildRustTriple").replace('-', "_");
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
      "cp",
   )?;
   succeed(Command::new("chmod").arg("-R").arg("u+w").arg(src), "chmod")?;
   for patch in &attrs.patches {
      note("patch", patch);
      succeed(
         Command::new("patch")
            .args(["-p1", "-F0", "-i"])
            .arg(patch)
            .current_dir(src),
         "patch",
      )?;
   }
   shebangs(Path::new(src), false)
}

fn restore(tree: &Path, top: &str) -> io::Result<()> {
   note("restore", &tree.to_string_lossy());
   succeed(
      Command::new("bsdtar")
         .arg("-xf")
         .arg(tree.join("tree.tar.zst"))
         .arg("-C")
         .arg(top),
      "bsdtar",
   )
}

pub fn succeed(command: &mut Command, name: &str) -> io::Result<()> {
   let output = command.stderr(Stdio::inherit()).output()?;
   io::stderr().lock().write_all(&output.stdout)?;
   if !output.status.success() {
      return Err(io::Error::other(format!("{name} failed")));
   }
   Ok(())
}
