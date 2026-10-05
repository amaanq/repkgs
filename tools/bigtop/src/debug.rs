use std::{
   collections::BTreeMap,
   fs::{
      DirBuilder,
      create_dir_all,
      metadata,
      read,
      remove_dir_all,
   },
   io,
   os::unix::fs::DirBuilderExt as _,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use mummer::{
   elf::{
      is_elf,
      names,
      notes,
      sections,
   },
   image::Image,
   process::Checked as _,
   tree::{
      Entry,
      EntryKind,
      note,
      parallel,
      walk,
      writable,
   },
};

use crate::prepare::succeed;

const PACKAGE_NOTE: u32 = 0xCAFE_1A7E;
const BUILD_ID_NOTE: u32 = 3;
const SMALL_ELF: u64 = 3072;
const HEX: &[u8; 16] = b"0123456789abcdef";

struct Elf {
   path:      String,
   rel:       String,
   id:        Option<String>,
   ours:      bool,
   dwarf:     bool,
   split:     bool,
   text_size: usize,
}

pub fn run(output: &Path, debug: &Path) -> io::Result<()> {
   let files = walk(output)?
      .into_iter()
      .filter(|entry| entry.kind == EntryKind::File)
      .collect::<Vec<Entry>>();
   strip_archives(&files, &["a", "o"])?;
   let candidates = files
      .iter()
      .filter(|entry| entry.size > SMALL_ELF && !has_extension(&entry.rel, &["a", "o", "rlib"]))
      .collect::<Vec<&Entry>>();
   let elfs = inspect(&candidates)?;
   let stripped = elfs
      .iter()
      .filter(|elf| elf.ours && !elf.dwarf && !elf.split && elf.text_size >= 0x100)
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
   for elf in elfs
      .iter()
      .filter(|elf| elf.ours && elf.dwarf && !elf.split)
   {
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
   report(debug, grouped.len())
}

pub fn macho(output: &Path, debug: &Path) -> io::Result<()> {
   let files: Vec<Entry> = walk(output)?
      .into_iter()
      .filter(|entry| entry.kind == EntryKind::File)
      .collect();
   strip_archives(&files, &["a", "o"])?;

   let candidates: Vec<&Entry> = files
      .iter()
      .filter(|entry| entry.size > SMALL_ELF && !has_extension(&entry.rel, &["a", "o", "rlib"]))
      .collect();
   let inspected = parallel(&candidates, |entry| {
      Ok(macho_uuid(entry)?.map(|uuid| (entry.path.as_str(), uuid)))
   })?;
   let mut groups = BTreeMap::<String, Vec<&str>>::new();

   for (path, uuid) in inspected.into_iter().flatten() {
      writable(Path::new(path))?;
      groups.entry(uuid).or_default().push(path);
   }

   if groups.is_empty() {
      return Ok(());
   }

   let grouped: Vec<(String, Vec<&str>)> = groups.into_iter().collect();
   create_dir_all(debug.join("lib/debug"))?;

   parallel(&grouped, |group| {
      let uuid = &group.0;
      let paths = &group.1;
      let Some(first) = paths.first() else {
         return Ok(());
      };

      let bundle = debug.join("lib/debug").join(format!("{uuid}.dSYM"));
      succeed(Command::new("dsymutil").arg(first).arg("-o").arg(&bundle))?;

      match remove_dir_all(bundle.join("Contents/Resources/Relocations")) {
         Ok(()) => {},
         Err(error) if error.kind() == io::ErrorKind::NotFound => {},
         Err(error) => return Err(error),
      }

      for path in paths {
         succeed(Command::new("llvm-strip").arg("-S").arg(path))?;
      }

      Ok(())
   })?;

   report(debug, grouped.len())
}

fn macho_uuid(entry: &Entry) -> io::Result<Option<String>> {
   let bytes = read(&entry.path)?;

   if !bytes.starts_with(&[0xCF, 0xFA, 0xED, 0xFE]) && !bytes.starts_with(&[0xCA, 0xFE, 0xBA, 0xBE])
   {
      return Ok(None);
   }

   let symbols = Command::new("llvm-nm")
      .arg("-ap")
      .arg(&entry.path)
      .output()?;

   if !String::from_utf8_lossy(&symbols.stdout).contains(" OSO ") {
      return Ok(None);
   }

   let headers = Command::new("llvm-objdump")
      .args(["--macho", "--private-headers"])
      .arg(&entry.path)
      .output()?;

   if !headers.status.success() {
      return Err(io::Error::other(format!(
         "llvm-objdump failed for {}",
         entry.path
      )));
   }

   Ok(String::from_utf8_lossy(&headers.stdout)
      .lines()
      .filter_map(|line| line.trim().strip_prefix("uuid "))
      .find(|uuid| {
         !uuid.is_empty()
            && uuid
               .bytes()
               .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
      })
      .map(str::to_owned))
}

fn report(debug: &Path, count: usize) -> io::Result<()> {
   let size = walk(debug)?
      .iter()
      .filter(|entry| entry.kind != EntryKind::Link)
      .map(|entry| entry.size)
      .sum::<u64>()
      + metadata(debug)?.len();
   note("debug", &format!("{count} files, {}", human(size)));
   Ok(())
}

pub fn strip_archives(files: &[Entry], extensions: &[&str]) -> io::Result<()> {
   for entry in files
      .iter()
      .filter(|entry| entry.kind == EntryKind::File && has_extension(&entry.rel, extensions))
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
      path:      entry.path.clone(),
      rel:       entry.rel.clone(),
      id:        notes
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
      ours:      notes.iter().any(|found| found.kind == PACKAGE_NOTE),
      dwarf:     names.iter().any(|name| name == ".debug_info"),
      split:     names.iter().any(|name| name == ".gnu_debuglink"),
      text_size: names
         .iter()
         .zip(&sections)
         .filter(|&(name, section)| name == ".text" && section.kind == 1)
         .map(|(_name, section)| section.size)
         .max()
         .unwrap_or_default(),
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
   command.checked()
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
