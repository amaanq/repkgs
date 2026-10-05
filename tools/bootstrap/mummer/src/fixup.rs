use std::{
   collections::BTreeSet,
   fs::{
      canonicalize,
      metadata,
      read,
      read_dir,
   },
   io::{
      self,
      Write as _,
   },
   os::unix::fs::PermissionsExt as _,
   path::{
      Display,
      Path,
      PathBuf,
   },
};

use crate::{
   elf::{
      DT_NEEDED,
      DT_RPATH,
      DT_RUNPATH,
      ET_DYN,
      ET_EXEC,
      PT_INTERP,
      SHT_DYNAMIC,
      SHT_DYNSYM,
      SHT_GNU_VERNEED,
      Section,
      Segment,
      dynamic_tags,
      file_offset,
      is_elf,
      prefixed_relative,
      relative_from,
      sections,
      segments,
      shared_library,
      symbols,
   },
   files::{
      utf8,
      write_file,
   },
   image::Image,
   macho,
   process::diagnostic,
   store::{
      STORE_DIR,
      is_store_path,
      normalize,
   },
};

pub struct Context {
   pub prefix:      PathBuf,
   pub dest:        PathBuf,
   pub sdk:         Option<PathBuf>,
   pub directories: BTreeSet<PathBuf>,
   /// The dynamic loader's name and directory, the libc's.
   pub loader:      Option<(String, PathBuf)>,
}

impl Context {
   #[must_use]
   #[inline]
   pub fn final_path(&self, path: &Path) -> PathBuf {
      let normal = normalize(path);
      normal.strip_prefix(&self.prefix).map_or_else(
         |_| normal.clone(),
         |relative| normalize(&self.dest.join(relative)),
      )
   }

   #[must_use]
   #[inline]
   pub fn on_disk(&self, path: &Path) -> PathBuf {
      let normal = normalize(path);
      normal.strip_prefix(&self.dest).map_or_else(
         |_| normal.clone(),
         |relative| normalize(&self.prefix.join(relative)),
      )
   }

   #[must_use]
   #[inline]
   pub fn shown<'path>(&self, path: &'path Path) -> Display<'path> {
      path.strip_prefix(&self.prefix).unwrap_or(path).display()
   }
}

struct Needed {
   name:            String,
   value_offset:    usize,
   version_offsets: Vec<usize>,
}

struct DynamicStrings {
   strings: usize,
   runpath: usize,
   needed:  Vec<Needed>,
}

struct SearchDirectory {
   text: String,
   path: PathBuf,
   keep: bool,
}

#[inline]
pub fn run(
   prefix: &Path,
   dest: Option<&Path>,
   sdk: Option<&Path>,
   loader: Option<&Path>,
   denied_paths: &[String],
) -> io::Result<()> {
   if let Some(path) = dest
      && !is_store_path(&path.to_string_lossy())
   {
      return Err(io::Error::other(format!(
         "invalid --dest store path {}",
         path.display()
      )));
   }
   let mut context = Context {
      prefix:      normalize(prefix),
      dest:        normalize(dest.unwrap_or(prefix)),
      sdk:         sdk.map(normalize),
      directories: BTreeSet::new(),
      loader:      loader.map(loader_location).transpose()?,
   };
   let denied = denied_hashes(denied_paths)?;
   if !is_store_path(&context.dest.to_string_lossy()) {
      return Err(io::Error::other(
         "reloc-fixup output must end up under the Nix store",
      ));
   }
   let (files, directories) = walk(&context.prefix)?;
   context.directories = directories;
   let mut failures = 0_usize;
   for path in files {
      if path
         .extension()
         .is_some_and(|extension| extension == "debug")
      {
         continue;
      }
      if let Err(error) = fix_file(&context, &denied, &path) {
         diagnostic(format_args!(
            "reloc-fixup failed for {}, {error}",
            path.display()
         ));
         failures += 1;
      }
   }
   if failures != 0_usize {
      return Err(io::Error::other(format!(
         "relocation failed for {failures} files"
      )));
   }
   Ok(())
}

fn loader_location(path: &Path) -> io::Result<(String, PathBuf)> {
   let file_name = path
      .file_name()
      .ok_or_else(|| io::Error::other("the loader has no file name"))?;
   let name = utf8(Path::new(file_name))?.to_owned();
   let directory = canonicalize(
      path
         .parent()
         .ok_or_else(|| io::Error::other("the loader has no directory"))?,
   )?;
   Ok((name, directory))
}

fn denied_hashes(denied_paths: &[String]) -> io::Result<Vec<Vec<u8>>> {
   denied_paths
      .iter()
      .map(|path| {
         if !is_store_path(path) {
            return Err(io::Error::other(format!(
               "invalid --deny store path {path}"
            )));
         }
         let name = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("invalid denied store path"))?;
         Ok(name
            .get(..32)
            .ok_or_else(|| io::Error::other("denied store path has no hash"))?
            .as_bytes()
            .to_vec())
      })
      .collect()
}

#[expect(
   clippy::filetype_is_file,
   reason = "Only regular files can contain binary images"
)]
fn walk(prefix: &Path) -> io::Result<(Vec<PathBuf>, BTreeSet<PathBuf>)> {
   let mut pending = vec![prefix.to_path_buf()];
   let mut files = Vec::new();
   let mut directories = BTreeSet::new();
   while let Some(directory) = pending.pop() {
      for child in read_dir(directory)? {
         let entry = child?;
         let kind = entry.file_type()?;
         let path = entry.path();
         if kind.is_dir() {
            pending.push(path);
         } else if kind.is_file() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.contains(".so") || shared_library(&name) {
               let parent = path
                  .parent()
                  .ok_or_else(|| io::Error::other("shared library has no parent"))?;
               if !parent.to_string_lossy().contains("/debug") {
                  directories.insert(parent.to_path_buf());
               }
            }
            files.push(path);
         }
      }
   }
   Ok((files, directories))
}

fn fix_file(context: &Context, denied: &[Vec<u8>], path: &Path) -> io::Result<()> {
   let bytes = read(path)?;
   let driver_config = path
      .strip_prefix(&context.prefix)
      .is_ok_and(|relative| matches!(relative.to_str(), Some("etc/mummer.json" | "etc/roots")));
   if !driver_config {
      refuse_denied(&bytes, denied)?;
   }
   let mut image = Image { bytes };
   if is_elf(&image) {
      fix_elf(context, path, &mut image)
   } else {
      macho::fix(context, path, &mut image)
   }
}

fn refuse_denied(bytes: &[u8], denied: &[Vec<u8>]) -> io::Result<()> {
   for hash in denied {
      let Some(position) = bytes.windows(hash.len()).position(|window| window == hash) else {
         continue;
      };
      let shown = &bytes[position..(position + 100).min(bytes.len())];
      let end = shown
         .iter()
         .position(|byte| *byte < 0x20 || *byte == 0x7F)
         .unwrap_or(shown.len());
      return Err(io::Error::other(format!(
         "refers to build platform {}",
         String::from_utf8_lossy(&shown[..end])
      )));
   }
   Ok(())
}

#[inline]
pub fn write_back(path: &Path, image: &Image) -> io::Result<()> {
   write_file(
      path,
      &image.bytes,
      Some(metadata(path)?.permissions().mode()),
   )
}

fn fix_elf(context: &Context, path: &Path, image: &mut Image) -> io::Result<()> {
   if !matches!(image.word(16)?, ET_EXEC | ET_DYN) {
      return Ok(());
   }
   let sections = sections(image)?;
   let segments = segments(image)?;
   let mut changed = fix_runpath(context, image, &sections, path)?;
   if let Some(address) = entry_stub(image, &sections, &segments)? {
      changed |= detach_interpreter(context, path, image, &segments, address)?;
   }
   if changed {
      write_back(path, image)?;
      writeln!(io::stdout().lock(), "relocated {}", context.shown(path))?;
   }
   if image
      .bytes
      .windows(STORE_DIR.len() + 1)
      .any(|window| window == format!("{STORE_DIR}/").as_bytes())
   {
      writeln!(
         io::stdout().lock(),
         "absolute store reference remains in {}",
         context.shown(path)
      )?;
   }
   Ok(())
}

/// The entry point when `RELOCSTB` marks it as the stub already, else
/// `__reloc_start`.
fn entry_stub(
   image: &Image,
   sections: &[Section],
   segments: &[Segment],
) -> io::Result<Option<u64>> {
   let entry = image.quad(24)?;
   if let Some(offset) = entry
      .checked_sub(16)
      .and_then(|address| file_offset(segments, address))
      && image.array::<8>(offset)? == *b"RELOCSTB"
   {
      return Ok(Some(entry));
   }
   let Some(table) = sections.iter().find(|section| section.kind == SHT_DYNSYM) else {
      return Ok(None);
   };
   for symbol in symbols(sections, table)? {
      if symbol.name(image)? == "__reloc_start" {
         return Ok(Some(image.quad(symbol.entry + 8)?));
      }
   }
   Ok(None)
}

fn detach_interpreter(
   context: &Context,
   path: &Path,
   image: &mut Image,
   segments: &[Segment],
   address: u64,
) -> io::Result<bool> {
   let mut changed = false;
   for segment in segments.iter().filter(|segment| segment.kind == PT_INTERP) {
      let interpreter = image.string(segment.offset)?;
      if is_store_path(&interpreter) {
         let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("ELF has no parent directory"))?;
         let replacement = relative_from(&context.final_path(parent), Path::new(&interpreter));
         image.padded(
            segment.offset,
            segment.size,
            replacement.as_os_str().as_encoded_bytes(),
         )?;
      }
      image.write(segment.header, &0_u32.to_le_bytes())?;
      image.write(24, &address.to_le_bytes())?;
      changed = true;
   }
   Ok(changed)
}

fn origin_relative(from: &Path, to: &Path) -> String {
   prefixed_relative("$ORIGIN", from, to)
}

/// Of `DT_RPATH` and `DT_RUNPATH` the last one counts.
fn dynamic_strings(image: &Image, sections: &[Section]) -> io::Result<Option<DynamicStrings>> {
   let Some(dynamic) = sections.iter().find(|section| section.kind == SHT_DYNAMIC) else {
      return Ok(None);
   };
   let strings = sections
      .get(dynamic.link)
      .ok_or_else(|| io::Error::other("ELF dynamic section has no string table"))?
      .offset;
   let mut runpath = None;
   let mut needed = Vec::new();
   for tag in dynamic_tags(image, dynamic.offset, dynamic.size) {
      let (entry, kind) = tag?;
      if !matches!(kind, DT_NEEDED | DT_RPATH | DT_RUNPATH) {
         continue;
      }
      let string_offset = strings
         .checked_add(image.size(entry + 8)?)
         .ok_or_else(|| io::Error::other("ELF dynamic string offset overflows"))?;
      if kind == DT_NEEDED {
         needed.push(Needed {
            name:            image.string(string_offset)?,
            value_offset:    entry + 8,
            version_offsets: Vec::new(),
         });
      } else {
         runpath = Some(string_offset);
      }
   }
   Ok(runpath.map(|start| {
      DynamicStrings {
         strings,
         runpath: start,
         needed,
      }
   }))
}

fn version_offsets(
   image: &Image,
   sections: &[Section],
   strings: usize,
   needed: &mut [Needed],
) -> io::Result<()> {
   let Some(versions) = sections
      .iter()
      .find(|section| section.kind == SHT_GNU_VERNEED)
   else {
      return Ok(());
   };
   let mut offset = versions.offset;
   let end = versions.offset + versions.size;
   while offset < end {
      image.array::<16>(offset)?;
      let string_offset = strings
         .checked_add(image.long_usize(offset + 4)?)
         .ok_or_else(|| io::Error::other("ELF version name offset overflows"))?;
      let name = image.string(string_offset)?;
      for library in needed.iter_mut().filter(|library| library.name == name) {
         library.version_offsets.push(offset + 4);
      }
      let next = image.long_usize(offset + 12)?;
      if next == 0 {
         break;
      }
      if next < 16 || next > end - offset {
         return Err(io::Error::other("invalid ELF version entry offset"));
      }
      offset += next;
   }
   Ok(())
}

fn search_directories(context: &Context, final_here: &Path, old: &str) -> Vec<SearchDirectory> {
   let mut search = Vec::new();
   for entry in old.split(':').filter(|entry| !entry.is_empty()) {
      let final_directory = if entry == "$ORIGIN" || entry.starts_with("$ORIGIN/") {
         normalize(&final_here.join(&entry[entry.len().min(8)..]))
      } else {
         context.final_path(Path::new(entry))
      };
      if is_store_path(&final_directory.to_string_lossy()) {
         search.push(SearchDirectory {
            text: origin_relative(final_here, &final_directory),
            path: context.on_disk(&final_directory),
            keep: true,
         });
      }
   }
   for directory in &context.directories {
      search.push(SearchDirectory {
         text: origin_relative(final_here, &context.final_path(directory)),
         path: directory.clone(),
         keep: false,
      });
   }
   search
}

/// The libc's libraries stay found by name beside the loader.
fn direct_libraries(
   context: &Context,
   needed: &[Needed],
   search: &mut [SearchDirectory],
) -> io::Result<Vec<(usize, usize)>> {
   let mut direct = Vec::new();
   for (index, library) in needed.iter().enumerate() {
      let loads = context.loader.as_ref();
      if library.name.contains('/') || loads.is_some_and(|loader| loader.0 == library.name) {
         continue;
      }
      let found = search
         .iter()
         .position(|directory| directory.path.join(&library.name).exists())
         .ok_or_else(|| {
            io::Error::other(format!(
               "NEEDED {} was not found in RUNPATH or the output",
               library.name
            ))
         })?;
      let libc = loads.is_some_and(|loader| {
         canonicalize(&search[found].path).is_ok_and(|directory| directory == loader.1)
      });
      if !libc {
         search[found].keep = false;
         direct.push((index, found));
      }
   }
   Ok(direct)
}

fn render(directories: &[SearchDirectory]) -> String {
   let mut entries = Vec::new();
   for directory in directories {
      if directory.keep && !entries.contains(&directory.text.as_str()) {
         entries.push(directory.text.as_str());
      }
   }
   entries.join(":")
}

fn refuse_shared_runpath(
   image: &Image,
   sections: &[Section],
   start: usize,
   length: usize,
) -> io::Result<()> {
   let Some(table) = sections.iter().find(|section| section.kind == SHT_DYNSYM) else {
      return Ok(());
   };
   for symbol in symbols(sections, table)? {
      let name_offset = symbol.name_offset(image)?;
      if name_offset > start && name_offset < start + length {
         return Err(io::Error::other("ELF symbol shares bytes with RUNPATH"));
      }
   }
   Ok(())
}

fn fix_runpath(
   context: &Context,
   image: &mut Image,
   sections: &[Section],
   path: &Path,
) -> io::Result<bool> {
   let Some(DynamicStrings {
      strings,
      runpath: start,
      mut needed,
   }) = dynamic_strings(image, sections)?
   else {
      return Ok(false);
   };
   version_offsets(image, sections, strings, &mut needed)?;
   let old = image.string(start)?;
   let parent = path
      .parent()
      .ok_or_else(|| io::Error::other("ELF has no parent directory"))?;
   let final_here = context.final_path(parent);
   let mut search = search_directories(context, &final_here, &old);
   let mut direct = direct_libraries(context, &needed, &mut search)?;
   let mut runpath = render(&search);
   let mut blob = runpath.as_bytes().to_vec();
   for &(library, directory) in &direct {
      blob.push(0);
      blob.extend(format!("{}/{}", search[directory].text, needed[library].name).as_bytes());
   }
   if blob.len() > old.len() {
      for &(_, directory) in &direct {
         search[directory].keep = true;
      }
      direct.clear();
      runpath = render(&search);
      blob = runpath.as_bytes().to_vec();
   }
   if blob == old.as_bytes() {
      return Ok(false);
   }
   refuse_shared_runpath(image, sections, start, old.len())?;
   image.padded(start, old.len() + 1, &blob)?;
   let mut string_offset = start
      .checked_sub(strings)
      .ok_or_else(|| io::Error::other("RUNPATH precedes its string table"))?
      + runpath.len()
      + 1;
   for (library, directory) in direct {
      image.write(
         needed[library].value_offset,
         &u64::try_from(string_offset)
            .map_err(io::Error::other)?
            .to_le_bytes(),
      )?;
      for &offset in &needed[library].version_offsets {
         image.write(
            offset,
            &u32::try_from(string_offset)
               .map_err(io::Error::other)?
               .to_le_bytes(),
         )?;
      }
      string_offset += search[directory].text.len() + 1 + needed[library].name.len() + 1;
   }
   Ok(true)
}
