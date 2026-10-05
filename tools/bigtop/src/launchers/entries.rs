use std::{
   collections::BTreeMap,
   fs::{
      File,
      canonicalize,
      copy,
      create_dir_all,
      metadata,
      read,
      read_dir,
      read_link,
      remove_file,
      rename,
      symlink_metadata,
   },
   io::{
      self,
      Read as _,
      Write as _,
   },
   iter::once,
   os::unix::fs::symlink,
   path::{
      Path,
      PathBuf,
   },
};

use mummer::{
   config::BinaryFormat,
   elf::{
      PT_INTERP,
      has_segment,
      is_elf,
      segments,
   },
   image::Image,
   store::normalize,
   tree::{
      note,
      writable,
   },
};
use serde::Serialize;

use crate::{
   attrs::{
      Ctx,
      nix_store,
   },
   exports::{
      exports_of,
      lib_dirs,
      relative,
   },
};

#[derive(Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Variable {
   Prepend(Vec<String>),
   Default(String),
   Set(String),
}

#[derive(Serialize)]
pub struct Record<'env> {
   pub env:     &'env BTreeMap<String, Variable>,
   pub program: String,
   #[serde(skip_serializing_if = "Option::is_none")]
   pub args:    Option<Vec<String>>,
   #[serde(skip_serializing_if = "Option::is_none")]
   pub argv0:   Option<&'static str>,
}

struct Context<'run> {
   out:          String,
   store:        String,
   bin:          PathBuf,
   launch:       &'run Path,
   interp:       &'run Path,
   ldso:         bool,
   owners:       Vec<String>,
   deps:         &'run [String],
   scripts_env:  BTreeMap<String, Variable>,
   binaries_env: BTreeMap<String, Variable>,
}

impl Context<'_> {
   fn storerel(&self, path: &str) -> String {
      relative(path, &self.out, &self.store)
   }
}

/// bin/<name> as a launch record for entry points that are not files with a
/// #! line (deno modules, escripts), paths made package-relative.
pub fn write(
   ctx: &Ctx,
   name: &str,
   program: &str,
   args: &[String],
   vars: &BTreeMap<String, String>,
) -> io::Result<()> {
   let store_dir = nix_store();
   let rel = |path: &str| relative(path, &ctx.out, &store_dir);
   let bin = Path::new(&ctx.out).join("bin");
   create_dir_all(&bin)?;
   let platform = ctx.platform()?;
   let executable = format!("{name}{}", platform.ext.exe);
   let env = vars
      .iter()
      .map(|(key, value)| (key.clone(), Variable::Set(rel(value))))
      .collect::<BTreeMap<String, Variable>>();
   let record = Record {
      env:     &env,
      program: rel(program),
      args:    Some(args.iter().map(|arg| rel(arg)).collect()),
      argv0:   None,
   };
   let mut text = serde_json::to_string(&record)?;
   text.push('\n');
   File::create(bin.join(format!(".{executable}.launch")))?.write_all(text.as_bytes())?;
   let link = bin.join(&executable);
   if symlink_metadata(&link).is_ok() {
      remove_file(&link)?;
   }
   if platform.binfmt == BinaryFormat::Coff {
      copy(&platform.launch, &link)?;
   } else {
      symlink(&platform.launch, &link)?;
   }

   note("launcher", &format!("bin/{name} -> {program}"));
   Ok(())
}

pub fn run(
   output: &Path,
   launch: &Path,
   interp: &Path,
   ldso: bool,
   deps: &[String],
) -> io::Result<()> {
   let bin = output.join("bin");
   if !bin.is_dir() {
      return Ok(());
   }
   let out = output.to_string_lossy().into_owned();
   let mut context = Context {
      store: nix_store(),
      bin,
      launch,
      interp,
      ldso,
      owners: once(out.clone()).chain(deps.iter().cloned()).collect(),
      deps,
      scripts_env: BTreeMap::new(),
      binaries_env: BTreeMap::new(),
      out,
   };
   (context.scripts_env, context.binaries_env) =
      runtime_env(context.deps, &context.out, &context.store)?;
   let mut names = read_dir(&context.bin)?
      .map(|entry| entry.map(|found| found.file_name().to_string_lossy().into_owned()))
      .collect::<io::Result<Vec<String>>>()?;
   names.sort();
   for name in names {
      let path = context.bin.join(&name);
      if name.starts_with('.')
         || !path.exists()
         || context.bin.join(format!(".{name}.launch")).exists()
         || sibling(&context.bin, &path)?
      {
         continue;
      }
      let Some(target) = target(&context, &path, &name)? else {
         continue;
      };
      // bin/.<name> stays in the same directory
      let hidden = context.bin.join(format!(".{name}"));
      rename(&path, &hidden)?;
      let record = Record {
         env:     target.env,
         program: target.program.clone(),
         args:    target.args,
         argv0:   target.argv0,
      };
      let mut text = serde_json::to_string(&record)?;
      text.push('\n');
      File::create(context.bin.join(format!(".{name}.launch")))?.write_all(text.as_bytes())?;
      symlink(context.launch, &path)?;
      note("launcher", &format!("bin/{name} -> {}", target.program));
   }
   Ok(())
}

pub type Env = BTreeMap<String, Variable>;

pub fn runtime_env(deps: &[String], out: &str, nix_store: &str) -> io::Result<(Env, Env)> {
   let mut binaries = BTreeMap::new();
   let mut path = Vec::new();

   for dep in deps {
      if Path::new(dep).join("bin").exists() {
         path.push(relative(&format!("{dep}/bin"), out, nix_store));
      }
      for (name, value) in exports_of(Path::new(dep))?.env {
         binaries.insert(name, Variable::Default(relative(&value, out, nix_store)));
      }
   }
   let mut scripts = BTreeMap::new();
   if !path.is_empty() {
      scripts.insert("PATH".to_owned(), Variable::Prepend(path));
   }
   scripts.extend(
      binaries
         .iter()
         .map(|(name, value)| (name.clone(), value.clone())),
   );
   Ok((scripts, binaries))
}

fn sibling(bin: &Path, path: &Path) -> io::Result<bool> {
   if !symlink_metadata(path)?.is_symlink() {
      return Ok(false);
   }
   let target = normalize(&bin.join(read_link(path)?));
   Ok(target.parent() == Some(bin))
}

struct Target<'env> {
   env:     &'env Env,
   program: String,
   args:    Option<Vec<String>>,
   argv0:   Option<&'static str>,
}

fn target<'env>(
   context: &'env Context<'_>,
   path: &Path,
   name: &str,
) -> io::Result<Option<Target<'env>>> {
   let mut head = Vec::with_capacity(256);
   File::open(path)?.take(256).read_to_end(&mut head)?;
   let real = format!("{{root}}/bin/.{name}");
   if head.starts_with(b"#!") {
      let Some(interpreter) = script_interpreter(context, path, name, &head)? else {
         return Ok(None);
      };
      let script = if symlink_metadata(path)?.is_symlink() {
         context.storerel(&canonicalize(path)?.to_string_lossy())
      } else {
         real
      };
      let mut args = interpreter.args;
      args.push(script);
      return Ok(Some(Target {
         env:     &context.scripts_env,
         program: context.storerel(&interpreter.program),
         args:    Some(args),
         argv0:   None,
      }));
   }
   let image = Image { bytes: read(path)? };
   if !is_elf(&image) {
      return Ok(None);
   }
   if context.ldso && has_segment(&segments(&image)?, PT_INTERP) {
      let libpath = lib_dirs(context.interp, context.deps)?
         .iter()
         .map(|dir| context.storerel(dir))
         .collect::<Vec<String>>();
      return Ok(Some(Target {
         env:     &context.binaries_env,
         program: context.storerel(&context.interp.to_string_lossy()),
         args:    Some(vec![
            "--argv0".to_owned(),
            "{self}".to_owned(),
            "--library-path".to_owned(),
            libpath.join(":"),
            real,
         ]),
         argv0:   None,
      }));
   }
   // argv[0] stays what the user ran
   if !context.binaries_env.is_empty() {
      return Ok(Some(Target {
         env:     &context.binaries_env,
         program: real,
         args:    None,
         argv0:   Some("{argv0}"),
      }));
   }
   Ok(None)
}

struct Interpreter {
   program: String,
   args:    Vec<String>,
}

fn script_interpreter(
   context: &Context<'_>,
   path: &Path,
   name: &str,
   head: &[u8],
) -> io::Result<Option<Interpreter>> {
   let text = String::from_utf8_lossy(head);
   let line = text
      .lines()
      .next()
      .unwrap_or_default()
      .get(2..)
      .unwrap_or_default()
      .split_whitespace()
      .collect::<Vec<&str>>();
   let Some(&first) = line.first() else {
      return Ok(None);
   };
   if first == "/bin/sh" && context.scripts_env.is_empty() {
      return Ok(None);
   }
   let words = if basename(first) == "env" {
      &line[1..]
   } else {
      line.as_slice()
   };
   let Some(&command) = words.first() else {
      return Ok(None);
   };
   let store = format!("{}/", context.store);
   let owned = context
      .owners
      .iter()
      .any(|owner| command.starts_with(&format!("{owner}/")));

   // pip writes the build python into console scripts and xz's configure the
   // build sh into POSIX_SHELL, a cross build cannot even run those, so
   // whoever puts the script on PATH brings the interpreter
   if command.starts_with(&store) && !owned {
      note(
         "script",
         &format!("bin/{name}: #!{command} is a build tool, not a dependency"),
      );
      rewrite_shebang(path, basename(command))?;
      return Ok(None);
   }

   let program = if command.starts_with(&store) || command == "/bin/sh" {
      command.to_owned()
   } else {
      let Some(found) = context
         .owners
         .iter()
         .map(|owner| format!("{owner}/bin/{}", basename(command)))
         .find(|candidate| Path::new(candidate).exists())
      else {
         return Ok(None);
      };
      found
   };
   if command.starts_with(&format!("{}/", context.out)) {
      rewrite_shebang(path, basename(command))?;
   }
   Ok(Some(Interpreter {
      program,
      args: words[1..].iter().map(|&word| word.to_owned()).collect(),
   }))
}

fn rewrite_shebang(path: &Path, program: &str) -> io::Result<()> {
   let bytes = read(path)?;
   let body = bytes
      .iter()
      .position(|byte| *byte == b'\n')
      .map_or(&[][..], |newline| &bytes[newline..]);
   let mut text = format!("#!/usr/bin/env {program}").into_bytes();
   text.extend_from_slice(body);
   let meta = metadata(path)?;
   writable(path)?;
   let mut file = File::create(path)?;
   file.write_all(&text)?;
   file.set_permissions(meta.permissions())
}

fn basename(path: &str) -> &str {
   path.rsplit('/').next().unwrap_or(path)
}
