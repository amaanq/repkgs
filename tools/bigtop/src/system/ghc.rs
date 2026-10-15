//! Installing a GHC bindist into the output: upstream's for ghc-bootstrap,
//! hadrian's for ghc, or Debian's unpacked .deb where upstream has none.

use std::{
   collections::BTreeMap,
   fs::{
      Permissions,
      copy,
      create_dir_all,
      read_to_string,
      remove_file,
      set_permissions,
      symlink_metadata,
      write,
   },
   io::{
      self,
      Write as _,
   },
   iter::{
      self,
      Peekable,
   },
   os::unix::fs::PermissionsExt as _,
   path::{
      Path,
      PathBuf,
   },
   str::Chars,
};

use glob::glob;
use mummer::{
   files::{
      remove_if_exists,
      utf8,
   },
   store::on_path,
   tree::{
      EntryKind,
      parallel,
      walk,
   },
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      nix_store,
      text,
   },
   exports::relative,
   implant,
   launchers::entries::{
      Record,
      place,
      script_env,
   },
   system::{
      cabal::in_name_order,
      command::{
         capture,
         run,
      },
   },
};

pub const MODULE: &str = module_path!();

const LIBRARY_DIRS: [&str; 4] = [
   "library-dirs:",
   "dynamic-library-dirs:",
   "library-dirs-static:",
   "dynamic-library-dirs-static:",
];

/// configure settings that point a bindist at our toolchain.
pub fn toolchain(ctx: &Ctx) -> Vec<String> {
   let target = text(&ctx.platform, "clangTarget");
   let mut settings = [
      "CC=cc",
      "CXX=c++",
      "LD=ld",
      "AR=ar",
      "RANLIB=ranlib",
      "STRIP=llvm-strip",
      "NM=llvm-nm",
      "OBJDUMP=llvm-objdump",
      "LLC=llc",
      "OPT=opt",
      "LLVMAS=clang",
   ]
   .map(str::to_owned)
   .to_vec();

   for stage in 0..=2_u8 {
      for kind in ["CC", "CXX", "GCC_LINKER"] {
         settings.push(format!("CONF_{kind}_OPTS_STAGE{stage}=--target={target}"));
      }
   }

   settings
}

pub fn print_toolchain(ctx: &Ctx) -> io::Result<()> {
   let mut stdout = io::stdout().lock();
   for setting in toolchain(ctx) {
      writeln!(stdout, "{setting}")?;
   }
   Ok(())
}

#[derive(Clone, Copy)]
enum Kind {
   /// files, through symlinks.
   Files,
   /// files that are not symlinks.
   Regular,
   Directories,
}

fn matching(pattern: &str, kind: Kind) -> io::Result<Vec<PathBuf>> {
   let mut found = Vec::new();
   for entry in glob(pattern).map_err(io::Error::other)? {
      let path = entry.map_err(io::Error::other)?;
      let wanted = match kind {
         Kind::Files => path.is_file(),
         Kind::Regular => symlink_metadata(&path)?.is_file(),
         Kind::Directories => path.is_dir(),
      };
      if wanted {
         found.push(path);
      }
   }
   found.sort();
   Ok(found)
}

fn rewrite(file: &Path, change: impl Fn(String) -> String) -> io::Result<()> {
   let text = read_to_string(file)?;
   write(file, change(text))
}

fn replace_all(text: String, pairs: &[(&str, &str)]) -> String {
   pairs
      .iter()
      .fold(text, |edited, &(from, to)| edited.replace(from, to))
}

fn name_library_dirs(text: &str, lib: &str) -> String {
   text
      .split_inclusive('\n')
      .map(|line| {
         LIBRARY_DIRS
            .iter()
            .find(|prefix| line.starts_with(**prefix))
            .map_or_else(
               || line.to_owned(),
               |prefix| format!("{prefix} {lib}{}", &line[prefix.len()..]),
            )
      })
      .collect()
}

/// The words of one field of a package-db config, continuation lines included.
fn field_words(text: &str, field: &str) -> Vec<String> {
   let mut words = Vec::new();
   let mut inside = false;
   for line in text.lines() {
      if let Some(rest) = line
         .strip_prefix(field)
         .and_then(|rest| rest.strip_prefix(':'))
      {
         inside = true;
         words.extend(rest.split_whitespace().map(str::to_owned));
      } else if inside && line.starts_with(char::is_whitespace) {
         words.extend(line.split_whitespace().map(str::to_owned));
      } else {
         inside = false;
      }
   }
   words
}

/// Names the lib dir of each dependency shipping a library a unit's
/// `extra-libraries` asks for, which the package db otherwise leaves to an
/// ambient search path.
fn close_db(ctx: &Ctx, db: &Path) -> io::Result<()> {
   for file in matching(&format!("{}/*.conf", db.display()), Kind::Files)? {
      let mut libs = Vec::new();
      for library in field_words(&read_to_string(&file)?, "extra-libraries") {
         let shipping = ctx
            .deps
            .iter()
            .map(|dep| format!("{}/lib", dep.root))
            .find(|lib| {
               ["so", "a"]
                  .iter()
                  .any(|kind| Path::new(lib).join(format!("lib{library}.{kind}")).exists())
            });
         if let Some(lib) = shipping
            && !libs.contains(&lib)
         {
            libs.push(lib);
         }
      }
      if !libs.is_empty() {
         rewrite(&file, |text| {
            libs
               .iter()
               .fold(text, |edited, lib| name_library_dirs(&edited, lib))
         })?;
      }
   }

   in_name_order(db)?;
   run(
      &format!("{}/bin/ghc-pkg", ctx.out.display()),
      &["recache"],
      &[],
   )
}

/// Every `"word"` (no quote inside) that `rename` maps, scanned left to right
/// the way a regex over the whole file does.
fn rename_quoted(text: &str, rename: impl Fn(&str) -> Option<String>) -> String {
   let mut renamed = String::with_capacity(text.len());
   let mut rest = text;

   while let Some(open) = rest.find('"') {
      renamed.push_str(&rest[..open]);
      let after = &rest[open + 1..];
      let Some(close) = after.find('"') else {
         rest = &rest[open..];
         break;
      };
      renamed.push('"');
      if let Some(word) = rename(&after[..close]) {
         renamed.push_str(&word);
         renamed.push('"');
         rest = &after[close + 1..];
      } else {
         renamed.push_str(&after[..close]);
         rest = &after[close..];
      }
   }

   renamed.push_str(rest);
   renamed
}

/// Debian's tool names in GHC's settings (x86_64-linux-gnu-gcc, llc-21) as
/// whichever of their unprefixed, unversioned names is on PATH.
fn our_tools(text: &str) -> String {
   let found = |name: &str| on_path(name) != Path::new(name);
   rename_quoted(text, |word| {
      if word.is_empty() || word.contains(char::is_whitespace) || found(word) {
         return None;
      }
      let unversioned = word
         .rsplit_once('-')
         .filter(|&(_, version)| {
            !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
         })
         .map_or(word, |(stem, _)| stem);
      let mut candidates = vec![unversioned];
      candidates.extend(
         unversioned
            .match_indices('-')
            .map(|(at, _)| &unversioned[at + 1..]),
      );
      candidates
         .into_iter()
         .find(|&candidate| !candidate.is_empty() && found(candidate))
         .map(str::to_owned)
   })
}

fn wrappers(ctx: &Ctx, attrs: &Attrs, moved: &[(&str, &str)]) -> io::Result<()> {
   let out = utf8(&ctx.out)?;
   let store = nix_store();
   let env = script_env(ctx, &attrs.dependencies)?;
   for file in matching(&format!("{out}/bin/*"), Kind::Regular)? {
      let text = replace_all(read_to_string(&file)?, moved);
      if !text.starts_with("#!") {
         continue;
      }
      let wrapper = Wrapper::read(&text)
         .map_err(|error| io::Error::other(format!("{}: {error}", file.display())))?;
      let name = file
         .file_name()
         .and_then(|name| name.to_str())
         .ok_or_else(|| io::Error::other(format!("{} is not UTF-8", file.display())))?;
      let rel = |paths: Vec<String>| {
         paths
            .iter()
            .map(|path| relative(path, out, &store))
            .collect::<Vec<_>>()
      };
      remove_file(&file)?;
      place(ctx, name, &Record {
         env:     &env,
         program: relative(&wrapper.program, out, &store),
         args:    Some(rel(wrapper.args)),
         after:   rel(wrapper.after),
         argv0:   None,
      })?;
   }
   Ok(())
}

struct Wrapper {
   program: String,
   args:    Vec<String>,
   after:   Vec<String>,
}

enum Field {
   Text(String),
   Callers,
}

impl Wrapper {
   /// Evaluates the few sh forms the wrappers use, assignments, loops over
   /// words, and the exec. The hsc2hs wrapper's loop over its arguments only
   /// drops its default --template when the caller gives one, which hsc2hs,
   /// taking the last --template, already does, so that loop and the
   /// function it calls are skipped.
   fn read(text: &str) -> Result<Self, String> {
      let mut vars = BTreeMap::new();
      let mut lines = text.lines().skip(1).map(str::trim);
      while let Some(line) = lines.next() {
         if line.is_empty() || line.starts_with('#') {
            continue;
         }
         if line == "for arg do" {
            lines.by_ref().find(|inner| *inner == "done");
            continue;
         }
         if line.ends_with("() {") {
            lines.by_ref().find(|inner| *inner == "}");
            continue;
         }
         if let Some(header) = line.strip_prefix("for ") {
            let (name, words) = header
               .strip_suffix("; do")
               .and_then(|list| list.split_once(" in "))
               .ok_or_else(|| format!("unexpected loop {line:?}"))?;
            let body = lines
               .by_ref()
               .take_while(|inner| *inner != "done")
               .map(str::to_owned)
               .collect::<Vec<_>>();
            for field in Self::fields(words, &vars)? {
               let Field::Text(word) = field else {
                  return Err(format!("loop over the caller's arguments {line:?}"));
               };
               vars.insert(name.to_owned(), word);
               for assignment in &body {
                  Self::assign(assignment, &mut vars)?;
               }
            }
            continue;
         }
         if let Some(command) = line.strip_prefix("exec ") {
            return Self::exec(command, &vars);
         }
         Self::assign(line, &mut vars)?;
      }
      Err("no exec".to_owned())
   }

   fn assign(line: &str, vars: &mut BTreeMap<String, String>) -> Result<(), String> {
      let (name, raw) = line
         .split_once('=')
         .filter(|&(left, _)| is_name(left))
         .ok_or_else(|| format!("unexpected line {line:?}"))?;
      let value = Self::expand(&mut raw.chars().peekable(), vars, false)?
         .into_iter()
         .map(|field| {
            match field {
               Field::Text(text) => Ok(text),
               Field::Callers => Err(format!("{name} takes the caller's arguments")),
            }
         })
         .collect::<Result<Vec<_>, _>>()?
         .join(" ");
      vars.insert(name.to_owned(), value);
      Ok(())
   }

   fn exec(command: &str, vars: &BTreeMap<String, String>) -> Result<Self, String> {
      let mut fields = Self::fields(command, vars)?.into_iter();
      let Some(Field::Text(program)) = fields.next() else {
         return Err(format!("exec of no program {command:?}"));
      };
      let mut args = Vec::new();
      let mut behind = None;
      for field in fields {
         match (field, behind.as_mut()) {
            (Field::Callers, None) => behind = Some(Vec::new()),
            (Field::Text(text), None) => args.push(text),
            (Field::Text(text), Some(trailing)) => trailing.push(text),
            (Field::Callers, Some(_)) => return Err(format!("two \"$@\" in {command:?}")),
         }
      }
      let after = behind.ok_or_else(|| format!("exec without \"$@\" {command:?}"))?;
      Ok(Self {
         program,
         args,
         after,
      })
   }

   fn fields(line: &str, vars: &BTreeMap<String, String>) -> Result<Vec<Field>, String> {
      Self::expand(&mut line.chars().peekable(), vars, true)
   }

   fn expand(
      chars: &mut Peekable<Chars<'_>>,
      vars: &BTreeMap<String, String>,
      split: bool,
   ) -> Result<Vec<Field>, String> {
      let mut fields = Vec::new();
      let mut current: Option<String> = None;
      let mut quoted = false;
      while let Some(chr) = chars.next() {
         match chr {
            '"' if !quoted && chars.clone().take(3).eq("$@\"".chars()) => {
               chars.nth(2);
               fields.extend(current.take().map(Field::Text));
               fields.push(Field::Callers);
            },
            '"' => {
               quoted = !quoted;
               current.get_or_insert_default();
            },
            ' ' | '\t' if split && !quoted => {
               if let Some(text) = current.take() {
                  fields.push(Field::Text(text));
               }
            },
            '$' => {
               let expansion = Self::expansion(chars)?;
               if expansion == "1+\"$@\"" {
                  fields.extend(current.take().map(Field::Text));
                  fields.push(Field::Callers);
                  continue;
               }
               let value = if let Some((name, word)) = expansion.split_once(":+") {
                  if vars.get(name).is_some_and(|value| !value.is_empty()) {
                     let inner = Self::expand(&mut word.chars().peekable(), vars, false)?;
                     inner
                        .into_iter()
                        .filter_map(|field| {
                           match field {
                              Field::Text(text) => Some(text),
                              Field::Callers => None,
                           }
                        })
                        .collect::<String>()
                  } else {
                     String::new()
                  }
               } else if is_name(&expansion) {
                  vars.get(&expansion).cloned().unwrap_or_default()
               } else {
                  return Err(format!("unsupported expansion ${{{expansion}}}"));
               };
               if quoted || !split {
                  current.get_or_insert_default().push_str(&value);
                  continue;
               }
               let mut pieces = value.split_whitespace();
               if let Some(first) = pieces.next() {
                  current.get_or_insert_default().push_str(first);
               }
               for piece in pieces {
                  if let Some(text) = current.take() {
                     fields.push(Field::Text(text));
                  }
                  current = Some(piece.to_owned());
               }
            },
            other => current.get_or_insert_default().push(other),
         }
      }
      if quoted {
         return Err("unterminated quote".to_owned());
      }
      if let Some(text) = current {
         fields.push(Field::Text(text));
      }
      Ok(fields)
   }

   fn expansion(chars: &mut Peekable<Chars<'_>>) -> Result<String, String> {
      if chars.next_if_eq(&'{').is_none() {
         let mut name = String::new();
         while let Some(chr) = chars.next_if(|chr| chr.is_ascii_alphanumeric() || *chr == '_') {
            name.push(chr);
         }
         return Ok(name);
      }
      let mut inner = String::new();
      let mut depth = 1_u32;
      for chr in chars.by_ref() {
         match chr {
            '{' => depth += 1,
            '}' => {
               depth -= 1;
               if depth == 0 {
                  return Ok(inner);
               }
            },
            _ => {},
         }
         inner.push(chr);
      }
      Err("unterminated ${".to_owned())
   }
}

fn is_name(text: &str) -> bool {
   text
      .chars()
      .next()
      .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
      && text
         .chars()
         .all(|chr| chr.is_ascii_alphanumeric() || chr == '_')
}

/// A make variable as the bindist's `show` target prints it, `NAME="value"`.
fn make_value(name: &str) -> io::Result<String> {
   let printed = capture("make", &["-s", "show", &format!("VALUE={name}")])?;
   printed
      .trim()
      .strip_prefix(&format!("{name}=\""))
      .and_then(|rest| rest.strip_suffix('"'))
      .map(str::to_owned)
      .ok_or_else(|| io::Error::other(format!("make show printed {printed:?} for {name}")))
}

/// What one of the bindist's `find . -type f` install loops does to the tree
/// `from`, every regular file placed under `to` with the mode `mode` picks.
fn install_files(
   from: &str,
   to: &Path,
   mode: impl Fn(&str) -> u32 + Sync,
) -> io::Result<Vec<PathBuf>> {
   if !Path::new(from).is_dir() {
      return Ok(Vec::new());
   }

   let files = walk(Path::new(from))?
      .into_iter()
      .filter(|entry| entry.kind == EntryKind::File)
      .collect::<Vec<_>>();

   create_dir_all(to)?;
   parallel(&files, |entry| {
      let placed = to.join(&entry.rel);
      if let Some(parent) = placed.parent() {
         create_dir_all(parent)?;
      }
      copy(&entry.path, &placed)?;
      set_permissions(&placed, Permissions::from_mode(mode(&entry.rel)))?;
      Ok(placed)
   })
}

/// from the unpacked bindist in the working directory.
pub fn install(ctx: &Ctx, attrs: &Attrs) -> io::Result<()> {
   let prefix = format!("--prefix={}", ctx.out.display());
   let settings = toolchain(ctx);
   let mut configure = vec!["./configure", prefix.as_str()];
   configure.extend(settings.iter().map(String::as_str));
   run("sh", &configure, &[])?;

   run("make", &["lib/settings"], &[])?;
   let placed = install_files("lib", Path::new(&make_value("ActualLibsDir")?), |rel| {
      if [".so", ".dylib", ".dll", ".mjs"]
         .iter()
         .any(|suffix| rel.ends_with(suffix))
      {
         0o755
      } else {
         0o644
      }
   })?;

   for (suffix, variable) in [(".a", "RANLIB_CMD"), (".dll", "STRIP_CMD")] {
      let matched = placed
         .iter()
         .map(|path| utf8(path))
         .filter(|path| path.as_ref().map_or(true, |text| text.ends_with(suffix)))
         .collect::<io::Result<Vec<&str>>>()?;
      if matched.is_empty() {
         continue;
      }

      let command = make_value(variable)?;
      let mut words = command.split_whitespace();
      let program = words
         .next()
         .ok_or_else(|| io::Error::other(format!("the bindist's {variable} is empty")))?;
      run(program, &words.chain(matched).collect::<Vec<_>>(), &[])?;
   }
   install_files("doc", Path::new(&make_value("docdir")?), |_| 0o644)?;
   install_files("share", Path::new(&make_value("datadir")?), |_| 0o644)?;

   let skipped = ["install_lib", "install_docs", "install_data"];
   let jobs = format!("-j{}", ctx.njobs);

   let first = iter::once(jobs.as_str())
      .chain(skipped)
      .chain(["FIND=true"])
      .collect::<Vec<_>>();
   run("make", &first, &[])?;

   let rest = iter::once(jobs.as_str())
      .chain(skipped.iter().flat_map(|&target| ["-o", target]))
      .chain(["install"])
      .collect::<Vec<_>>();
   run("make", &rest, &[])?;

   wrappers(ctx, attrs, &[])?;
   let db = matching(
      &format!("{}/lib/ghc-*/lib/package.conf.d", ctx.out.display()),
      Kind::Directories,
   )?
   .into_iter()
   .next()
   .ok_or_else(|| io::Error::other("the bindist installed no package.conf.d"))?;

   close_db(ctx, &db)
}

/// from Debian's unpacked .deb in the working directory.
pub fn install_deb(ctx: &Ctx, attrs: &Attrs) -> io::Result<()> {
   // a symlink into /var
   remove_if_exists(Path::new("usr/lib/ghc/lib/package.conf.d"))?;
   // ghc-pkg runs below, before finish: interpreter and RUNPATH now, the stub
   // when finish repeats it
   implant::run(
      &ctx.src.join("usr"),
      Path::new(text(&ctx.platform, "interp")),
      None,
      &attrs.dependencies,
   )?;

   let lib = format!("{}/lib/ghc/lib", ctx.out.display());
   create_dir_all(ctx.out.join("lib"))?;
   run(
      "cp",
      &[
         "-r",
         "usr/lib/ghc",
         &format!("{}/lib/ghc", ctx.out.display()),
      ],
      &[],
   )?;
   run(
      "cp",
      &["-r", "usr/bin", &format!("{}/bin", ctx.out.display())],
      &[],
   )?;
   run(
      "cp",
      &[
         "-r",
         "var/lib/ghc/package.conf.d",
         &format!("{lib}/package.conf.d"),
      ],
      &[],
   )?;

   for file in matching(&format!("{lib}/package.conf.d/*.conf"), Kind::Files)? {
      rewrite(&file, |text| text.replace("/usr/lib/ghc/lib", "${pkgroot}"))?;
   }
   rewrite(Path::new(&format!("{lib}/settings")), |text| {
      our_tools(&text)
   })?;
   let out = utf8(&ctx.out)?;
   wrappers(ctx, attrs, &[
      ("/usr/lib/ghc", &format!("{out}/lib/ghc")),
      ("\"/usr/bin\"", &format!("\"{out}/bin\"")),
   ])?;
   close_db(ctx, Path::new(&format!("{lib}/package.conf.d")))
}
