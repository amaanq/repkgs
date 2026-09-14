use std::{
   collections::BTreeMap,
   fs::{
      DirBuilder,
      metadata,
      read,
   },
   io,
   os::unix::fs::DirBuilderExt as _,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use crate::{
   elf::{
      is_elf,
      names,
      notes,
      sections,
   },
   image::Image,
   tree::{
      Entry,
      note,
      parallel,
      walk,
      writable,
   },
};

const PACKAGE_NOTE: u32 = 0xCAFE_1A7E;
const BUILD_ID_NOTE: u32 = 3;
const SMALL_ELF: u64 = 3072;
const HEX: &[u8; 16] = b"0123456789abcdef";

struct Elf {
   path:  String,
   rel:   String,
   id:    Option<String>,
   ours:  bool,
   dwarf: bool,
   split: bool,
}

pub fn run(output: &Path, debug: &Path) -> io::Result<()> {
   let files = walk(output)?
      .into_iter()
      .filter(|entry| entry.kind == 'f')
      .collect::<Vec<Entry>>();
   strip_archives(&files)?;
   let candidates = files
      .iter()
      .filter(|entry| entry.size > SMALL_ELF && !has_extension(&entry.rel, &["a", "o", "rlib"]))
      .collect::<Vec<&Entry>>();
   let elfs = inspect(&candidates)?;
   let stripped = elfs
      .iter()
      .filter(|elf| elf.ours && !elf.dwarf && !elf.split)
      .map(|elf| elf.rel.as_str())
      .take(3)
      .collect::<Vec<&str>>();
   if !stripped.is_empty() {
      return Err(io::Error::other(format!(
         "debug: linked here but no DWARF: {}. The build strips or drops -g. Fix that, or debug = \
          false",
         stripped.join(" ")
      )));
   }
   let foreign = elfs
      .iter()
      .filter(|elf| !elf.ours || elf.split)
      .collect::<Vec<&Elf>>();
   if !foreign.is_empty() {
      let shown = foreign
         .iter()
         .take(2)
         .map(|elf| basename(&elf.rel))
         .collect::<Vec<&str>>();
      note(
         "debug",
         &format!(
            "{} ELF files not linked here left as they are ({}…)",
            foreign.len(),
            shown.join(" ")
         ),
      );
   }
   let mut groups = BTreeMap::<&str, Vec<&str>>::new();
   for elf in elfs.iter().filter(|elf| elf.ours && !elf.split) {
      let Some(id) = elf.id.as_deref() else {
         return Err(io::Error::other(format!(
            "debug: linked here but no build-id: {}",
            elf.rel
         )));
      };
      writable(Path::new(&elf.path))?;
      groups.entry(id).or_default().push(&elf.path);
   }
   if groups.is_empty() {
      return Ok(());
   }
   let grouped = groups.into_iter().collect::<Vec<(&str, Vec<&str>)>>();
   parallel(&grouped, |group| split(debug, group.0, &group.1))?;
   let size = walk(debug)?
      .iter()
      .filter(|entry| entry.kind != 'l')
      .map(|entry| entry.size)
      .sum::<u64>()
      + metadata(debug)?.len();
   note(
      "debug",
      &format!("{} files, {}", grouped.len(), human(size)),
   );
   Ok(())
}

fn strip_archives(files: &[Entry]) -> io::Result<()> {
   for entry in files
      .iter()
      .filter(|entry| has_extension(&entry.rel, &["a", "o"]))
   {
      writable(Path::new(&entry.path))?;
      let done = Command::new("llvm-objcopy")
         .arg("--strip-debug")
         .arg(&entry.path)
         .output()?;
      if !done.status.success() {
         note("debug", &format!("DWARF left in {}", entry.rel));
      }
   }
   Ok(())
}

fn inspect(candidates: &[&Entry]) -> io::Result<Vec<Elf>> {
   let mut elfs = Vec::with_capacity(candidates.len());
   for batch in parallel(candidates, |entry| examine(entry))? {
      elfs.extend(batch);
   }
   Ok(elfs)
}

fn examine(entry: &Entry) -> io::Result<Option<Elf>> {
   let image = Image {
      bytes: read(&entry.path)?,
   };
   if !is_elf(&image) {
      return Ok(None);
   }
   let sections = sections(&image)?;
   let names = names(&image, &sections)?;
   let notes = notes(&image, &sections)?;
   Ok(Some(Elf {
      path:  entry.path.clone(),
      rel:   entry.rel.clone(),
      id:    notes
         .iter()
         .find(|found| found.owner == "GNU" && found.kind == BUILD_ID_NOTE)
         .map(|found| {
            found
               .description
               .iter()
               .flat_map(|byte| {
                  [
                     char::from(HEX[usize::from(byte >> 4_u8)]),
                     char::from(HEX[usize::from(byte & 15)]),
                  ]
               })
               .collect()
         }),
      ours:  notes.iter().any(|found| found.kind == PACKAGE_NOTE),
      dwarf: names.iter().any(|name| name == ".debug_info"),
      split: names.iter().any(|name| name == ".gnu_debuglink"),
   }))
}

fn split(debug: &Path, id: &str, paths: &[&str]) -> io::Result<()> {
   let file = debug
      .join("lib/debug/.build-id")
      .join(&id[..2])
      .join(format!("{}.debug", &id[2..]));
   DirBuilder::new()
      .recursive(true)
      .mode(0o755)
      .create(file.parent().unwrap_or(debug))?;
   let Some(first) = paths.first() else {
      return Ok(());
   };
   objcopy(&["--only-keep-debug", first], Some(&file))?;
   let link = format!("--add-gnu-debuglink={}", file.display());
   for path in paths {
      objcopy(&["--strip-debug", &link, path], None)?;
   }
   Ok(())
}

fn objcopy(args: &[&str], output: Option<&PathBuf>) -> io::Result<()> {
   let mut command = Command::new("llvm-objcopy");
   command.args(args);
   if let Some(path) = output {
      command.arg(path);
   }
   if !command.status()?.success() {
      return Err(io::Error::other(format!(
         "llvm-objcopy {} failed",
         args.join(" ")
      )));
   }
   Ok(())
}

fn has_extension(rel: &str, extensions: &[&str]) -> bool {
   Path::new(rel)
      .extension()
      .is_some_and(|found| extensions.iter().any(|extension| found == *extension))
}

fn basename(rel: &str) -> &str {
   rel.rsplit('/').next().unwrap_or(rel)
}

fn human(bytes: u64) -> String {
   const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
   let mut unit = None;
   let mut scale = 1_u64;
   for name in UNITS {
      if bytes < scale * 1000 {
         break;
      }
      scale *= 1000;
      unit = Some(name);
   }
   unit.map_or_else(
      || format!("{bytes} B"),
      |name| {
         let hundredths = (bytes * 100 + scale / 2) / scale;
         format!("{}.{:02} {name}", hundredths / 100, hundredths % 100)
      },
   )
}
