use std::{
   collections::BTreeSet,
   fs::{
      metadata,
      read,
      read_dir,
   },
   io::{
      self,
      Write,
   },
   path::{
      Path,
      PathBuf,
   },
};

use crate::{
   elf::{
      Image,
      Section,
      relative_from,
   },
   files::write_file,
   process::diagnostic,
   store::{
      STORE_DIR,
      is_store_path,
      normalize,
   },
};

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
   reason = "Only regular files can contain ELF images"
)]
pub fn run(args: &[String]) -> io::Result<()> {
   let Some((prefix, rest)) = args.split_first() else {
      return Err(io::Error::other("reloc-fixup requires an output directory"));
   };
   if !is_store_path(prefix) {
      return Err(io::Error::other(
         "reloc-fixup output must be under the Nix store",
      ));
   }
   let mut denied = Vec::new();
   for pair in rest.chunks(2) {
      let [flag, path] = pair else {
         return Err(io::Error::other("reloc-fixup --deny requires a store path"));
      };
      if flag != "--deny" || !is_store_path(path) {
         return Err(io::Error::other("invalid reloc-fixup argument"));
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
   let mut pending = vec![normalize(Path::new(prefix))];
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
            if entry.file_name().to_string_lossy().contains(".so") {
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
   let mut failures = 0;
   for path in files {
      if path
         .extension()
         .is_some_and(|extension| extension == "debug")
      {
         continue;
      }
      let result = (|| -> io::Result<()> {
         let bytes = read(&path)?;
         if denied
            .iter()
            .any(|hash| bytes.windows(hash.len()).any(|window| window == hash))
         {
            return Err(io::Error::other(
               "output refers to a denied build platform dependency",
            ));
         }
         let mut image = Image { bytes };
         if !image.is_supported() || !matches!(image.word(16)?, 2 | 3) {
            return Ok(());
         }
         let sections = image.sections()?;
         let segments = image.segments()?;
         let mut changed = fix_runpath(&mut image, &sections, &path, &directories)?;
         let entry_address = image.quad(24)?;
         let mut stub = None;
         if let Some(address) = entry_address.checked_sub(16) {
            for segment in &segments {
               if segment.kind == 1
                  && address >= segment.address
                  && address - segment.address < segment.size as u64
               {
                  let offset = segment.offset + (address - segment.address) as usize;
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
                  .checked_add(image.long(offset)? as usize)
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
                  let replacement = relative_from(parent, Path::new(&interpreter));
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
            write_file(&path, &image.bytes, Some(permission_bits(&path)?))?;
            writeln!(
               io::stdout().lock(),
               "relocated {}",
               path.strip_prefix(prefix).unwrap_or(&path).display()
            )?;
         }
         if image
            .bytes
            .windows(STORE_DIR.len())
            .any(|window| window == STORE_DIR.as_bytes())
         {
            writeln!(
               io::stdout().lock(),
               "absolute store reference remains in {}",
               path.display()
            )?;
         }
         Ok(())
      })();
      if let Err(error) = result {
         diagnostic(format_args!(
            "reloc-fixup failed for {}, {error}",
            path.display()
         ));
         failures += 1;
      }
   }
   if failures != 0 {
      return Err(io::Error::other(format!(
         "relocation failed for {failures} files"
      )));
   }
   Ok(())
}

fn permission_bits(path: &Path) -> io::Result<u32> {
   use std::os::unix::fs::PermissionsExt;
   Ok(metadata(path)?.permissions().mode())
}

fn fix_runpath(
   image: &mut Image,
   sections: &[Section],
   path: &Path,
   own: &BTreeSet<PathBuf>,
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
            .checked_add(image.long(offset + 4)? as usize)
            .ok_or_else(|| io::Error::other("ELF version name offset overflows"))?;
         let name = image.string(string_offset)?;
         for library in &mut needed {
            if library.name == name {
               library.version_offsets.push(offset + 4);
            }
         }
         let next = image.long(offset + 12)? as usize;
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
   let mut search = Vec::new();
   for entry in old.split(':').filter(|entry| !entry.is_empty()) {
      if is_store_path(entry) {
         search.push(SearchDirectory {
            text: format!(
               "$ORIGIN/{}",
               relative_from(parent, Path::new(entry)).display()
            ),
            path: normalize(Path::new(entry)),
            keep: true,
         });
      } else if entry == "$ORIGIN" {
         search.push(SearchDirectory {
            text: entry.to_owned(),
            path: parent.to_path_buf(),
            keep: true,
         });
      } else if let Some(suffix) = entry.strip_prefix("$ORIGIN/") {
         search.push(SearchDirectory {
            text: entry.to_owned(),
            path: normalize(&parent.join(suffix)),
            keep: true,
         });
      }
   }
   for directory in own {
      search.push(SearchDirectory {
         text: if directory == parent {
            "$ORIGIN".to_owned()
         } else {
            format!("$ORIGIN/{}", relative_from(parent, directory).display())
         },
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
            .checked_add(image.long(offset)? as usize)
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
         &(string_offset as u64).to_le_bytes(),
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
