use std::{
   collections::BTreeSet,
   fs::{
      metadata,
      read,
      read_dir,
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

use crate::{
   driver::shared_library,
   elf::{
      Section,
      is_elf,
      relative_from,
      sections,
      segments,
   },
   files::write_file,
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
   pub directories: BTreeSet<PathBuf>,
}

impl Context {
   pub fn final_path(&self, path: &Path) -> PathBuf {
      let normal = normalize(path);
      normal.strip_prefix(&self.prefix).map_or_else(
         |_| normal.clone(),
         |relative| normalize(&self.dest.join(relative)),
      )
   }

   pub fn on_disk(&self, path: &Path) -> PathBuf {
      let normal = normalize(path);
      normal.strip_prefix(&self.dest).map_or_else(
         |_| normal.clone(),
         |relative| normalize(&self.prefix.join(relative)),
      )
   }

   pub fn shown(&self, path: &Path) -> String {
      path
         .strip_prefix(&self.prefix)
         .unwrap_or(path)
         .display()
         .to_string()
   }
}

struct Needed {
   name:            String,
   value_offset:    usize,
   version_offsets: Vec<usize>,
}
struct SearchDirectory {
   text: String,
   path: PathBuf,
   keep: bool,
}

#[expect(
   clippy::filetype_is_file,
   reason = "Only regular files can contain binary images"
)]
#[expect(
   clippy::too_many_lines,
   reason = "single-pass directory walk and file handling, splitting would duplicate policy"
)]
pub fn run(prefix: &Path, dest: Option<&str>, denied_paths: &[String]) -> io::Result<()> {
   if let Some(path) = dest
      && !is_store_path(path)
   {
      return Err(io::Error::other(format!(
         "invalid --dest store path {path}"
      )));
   }
   let mut context = Context {
      prefix:      normalize(prefix),
      dest:        normalize(dest.map_or(prefix, Path::new)),
      directories: BTreeSet::new(),
   };
   let mut denied = Vec::new();
   for path in denied_paths {
      if !is_store_path(path) {
         return Err(io::Error::other(format!(
            "invalid --deny store path {path}"
         )));
      }
      let name = Path::new(path)
         .file_name()
         .and_then(|name| name.to_str())
         .ok_or_else(|| io::Error::other("invalid denied store path"))?;
      denied.push(
         name
            .get(..32)
            .ok_or_else(|| io::Error::other("denied store path has no hash"))?
            .as_bytes()
            .to_vec(),
      );
   }
   if !is_store_path(&context.dest.to_string_lossy()) {
      return Err(io::Error::other(
         "reloc-fixup output must end up under the Nix store",
      ));
   }
   let mut pending = vec![context.prefix.clone()];
   let mut files = Vec::new();
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
                  context.directories.insert(parent.to_path_buf());
               }
            }
            files.push(path);
         }
      }
   }
   let mut failures = 0_usize;
   for path in files {
      if path
         .extension()
         .is_some_and(|extension| extension == "debug")
      {
         continue;
      }
      let result = (|| -> io::Result<()> {
         let bytes = read(&path)?;
         for hash in &denied {
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
         let mut image = Image { bytes };
         if is_elf(&image) {
            fix_elf(&context, &path, &mut image)
         } else {
            macho::fix(&context, &path, &mut image)
         }
      })();
      if let Err(error) = result {
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

pub fn write_back(path: &Path, image: &Image) -> io::Result<()> {
   use std::os::unix::fs::PermissionsExt as _;
   write_file(
      path,
      &image.bytes,
      Some(metadata(path)?.permissions().mode()),
   )
}

fn fix_elf(context: &Context, path: &Path, image: &mut Image) -> io::Result<()> {
   if !matches!(image.word(16)?, 2 | 3) {
      return Ok(());
   }
   let sections = sections(image)?;
   let segments = segments(image)?;
   let mut changed = fix_runpath(context, image, &sections, path)?;
   let entry_address = image.quad(24)?;
   let mut stub = None;
   if let Some(address) = entry_address.checked_sub(16) {
      for segment in &segments {
         let segment_size = u64::try_from(segment.size).map_err(io::Error::other)?;
         if segment.kind == 1
            && address >= segment.address
            && address - segment.address < segment_size
         {
            let offset = segment.offset
               + usize::try_from(address - segment.address).map_err(io::Error::other)?;
            if image.array::<8>(offset)? == *b"RELOCSTB" {
               stub = Some(entry_address);
            }
         }
      }
   }
   if stub.is_none()
      && let Some(symbols) = sections.iter().find(|section| section.kind == 11)
   {
      let strings = sections
         .get(symbols.link)
         .ok_or_else(|| io::Error::other("ELF symbols have no string table"))?;
      if symbols.stride < 24 {
         return Err(io::Error::other("ELF symbol stride is too short"));
      }
      for offset in (symbols.offset..symbols.offset + symbols.size).step_by(symbols.stride) {
         let name_offset = strings
            .offset
            .checked_add(usize::try_from(image.long(offset)?).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("ELF symbol offset overflows"))?;
         if image.string(name_offset)? == "__reloc_start" {
            stub = Some(image.quad(offset + 8)?);
            break;
         }
      }
   }
   if let Some(address) = stub {
      for segment in &segments {
         if segment.kind != 3 {
            continue;
         }
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

fn origin_relative(from: &Path, to: &Path) -> String {
   let relative = relative_from(from, to);
   if relative == Path::new(".") {
      "$ORIGIN".to_owned()
   } else {
      format!("$ORIGIN/{}", relative.display())
   }
}

#[expect(
   clippy::too_many_lines,
   reason = "runpath rewrite keeps symbol and version handling in one scan"
)]
fn fix_runpath(
   context: &Context,
   image: &mut Image,
   sections: &[Section],
   path: &Path,
) -> io::Result<bool> {
   let Some(dynamic) = sections.iter().find(|section| section.kind == 6) else {
      return Ok(false);
   };
   let strings = sections
      .get(dynamic.link)
      .ok_or_else(|| io::Error::other("ELF dynamic section has no string table"))?;
   let mut runpath_offset = None;
   let mut needed = Vec::new();
   for offset in (dynamic.offset..dynamic.offset + dynamic.size).step_by(16) {
      let tag = image.quad(offset)?;
      if tag == 0 {
         break;
      }
      if matches!(tag, 1 | 15 | 29) {
         let string_offset = strings
            .offset
            .checked_add(image.size(offset + 8)?)
            .ok_or_else(|| io::Error::other("ELF dynamic string offset overflows"))?;
         if tag == 1 {
            needed.push(Needed {
               name:            image.string(string_offset)?,
               value_offset:    offset + 8,
               version_offsets: Vec::new(),
            });
         } else {
            runpath_offset = Some(string_offset);
         }
      }
   }
   let Some(start) = runpath_offset else {
      return Ok(false);
   };
   if let Some(versions) = sections.iter().find(|section| section.kind == 0x6FFF_FFFE) {
      let mut offset = versions.offset;
      let end = versions.offset + versions.size;
      while offset < end {
         image.array::<16>(offset)?;
         let string_offset = strings
            .offset
            .checked_add(usize::try_from(image.long(offset + 4)?).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("ELF version name offset overflows"))?;
         let name = image.string(string_offset)?;
         for library in &mut needed {
            if library.name == name {
               library.version_offsets.push(offset + 4);
            }
         }
         let next = usize::try_from(image.long(offset + 12)?).map_err(io::Error::other)?;
         if next == 0 {
            break;
         }
         if next < 16 || next > end - offset {
            return Err(io::Error::other("invalid ELF version entry offset"));
         }
         offset += next;
      }
   }
   let old = image.string(start)?;
   let parent = path
      .parent()
      .ok_or_else(|| io::Error::other("ELF has no parent directory"))?;
   let final_here = context.final_path(parent);
   let mut search = Vec::new();
   for entry in old.split(':').filter(|entry| !entry.is_empty()) {
      let final_directory = if entry == "$ORIGIN" || entry.starts_with("$ORIGIN/") {
         normalize(&final_here.join(&entry[entry.len().min(8)..]))
      } else {
         context.final_path(Path::new(entry))
      };
      if is_store_path(&final_directory.to_string_lossy()) {
         search.push(SearchDirectory {
            text: origin_relative(&final_here, &final_directory),
            path: context.on_disk(&final_directory),
            keep: true,
         });
      }
   }
   for directory in &context.directories {
      search.push(SearchDirectory {
         text: origin_relative(&final_here, &context.final_path(directory)),
         path: directory.clone(),
         keep: false,
      });
   }
   let mut direct = Vec::new();
   for (index, library) in needed.iter().enumerate() {
      if library.name.contains('/')
         || library.name.starts_with("ld-linux")
         || library.name.starts_with("linux-vdso")
      {
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
      if !search[found].path.join("libc.so.6").exists() {
         search[found].keep = false;
         direct.push((index, found));
      }
   }
   let render = |directories: &[SearchDirectory]| {
      let mut entries = Vec::new();
      for directory in directories {
         if directory.keep && !entries.contains(&directory.text.as_str()) {
            entries.push(directory.text.as_str());
         }
      }
      entries.join(":")
   };
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
   if let Some(symbols) = sections.iter().find(|section| section.kind == 11) {
      let table = sections
         .get(symbols.link)
         .ok_or_else(|| io::Error::other("ELF symbols have no string table"))?;
      if symbols.stride < 24 {
         return Err(io::Error::other("ELF symbol stride is too short"));
      }
      for offset in (symbols.offset..symbols.offset + symbols.size).step_by(symbols.stride) {
         let name_offset = table
            .offset
            .checked_add(usize::try_from(image.long(offset)?).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("ELF symbol offset overflows"))?;
         if name_offset > start && name_offset < start + old.len() {
            return Err(io::Error::other("ELF symbol shares bytes with RUNPATH"));
         }
      }
   }
   image.padded(start, old.len() + 1, &blob)?;
   let mut string_offset = start
      .checked_sub(strings.offset)
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
