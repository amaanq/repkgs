//! Installing a GHC bindist into the output: upstream's for ghc-bootstrap,
//! hadrian's for ghc, or Debian's unpacked .deb where upstream has none.

use std::{
   fs::{
      create_dir_all,
      read_to_string,
      symlink_metadata,
      write,
   },
   io::{
      self,
      Write as _,
   },
   path::{
      Path,
      PathBuf,
   },
};

use glob::glob;
use mummer::{
   files::{
      remove_if_exists,
      utf8,
   },
   store::on_path,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      text,
   },
   implant,
   system::command::run,
};

/// One shell line the bin/ wrappers start with, so they can derive the
/// prefix from their own location, sh builtins only since PATH may be empty.
const LOCATE_TOP: &str = "#!/bin/sh\ntop=$(cd \"${0%/*}/..\" && pwd)";

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

fn wrappers(ctx: &Ctx, pairs: &[(&str, &str)]) -> io::Result<()> {
   for file in matching(&format!("{}/bin/*", ctx.out.display()), Kind::Regular)? {
      rewrite(&file, |text| replace_all(text, pairs))?;
   }
   Ok(())
}

/// from the unpacked bindist in the working directory.
pub fn install(ctx: &Ctx) -> io::Result<()> {
   let prefix = format!("--prefix={}", ctx.out.display());
   let settings = toolchain(ctx);
   let mut configure = vec!["./configure", prefix.as_str()];
   configure.extend(settings.iter().map(String::as_str));
   run("sh", &configure, &[])?;
   run("make", &["install"], &[])?;

   wrappers(ctx, &[("#!/bin/sh", LOCATE_TOP), (utf8(&ctx.out)?, "$top")])?;
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
   wrappers(ctx, &[
      ("#!/bin/bash", LOCATE_TOP),
      ("/usr/lib/ghc", "$top/lib/ghc"),
      ("\"/usr/bin\"", "\"$top/bin\""),
   ])?;
   close_db(ctx, Path::new(&format!("{lib}/package.conf.d")))
}
