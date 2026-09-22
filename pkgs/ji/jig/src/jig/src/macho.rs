use std::{
   io::{
      self,
      Write as _,
   },
   path::{
      Path,
      PathBuf,
   },
};

use sha2::{
   Digest as _,
   Sha256,
};

use crate::{
   elf::relative_from,
   fixup::{
      Context,
      write_back,
   },
   image::Image,
   store::{
      is_store_path,
      normalize,
   },
};

const MAGIC: u32 = 0xFEED_FACF;
const HEADER_SIZE: usize = 32;
const REQUIRED: u32 = 0x8000_0000;
const LOAD_DYLIB: u32 = 0xC;
const ID_DYLIB: u32 = 0xD;
const LOAD_WEAK_DYLIB: u32 = 0x18 | REQUIRED;
const RPATH: u32 = 0x1C | REQUIRED;
const REEXPORT_DYLIB: u32 = 0x1F | REQUIRED;
const LOAD_UPWARD_DYLIB: u32 = 0x23 | REQUIRED;
const CODE_SIGNATURE: u32 = 0x1D;
const SEGMENT_64: u32 = 0x19;

struct Command {
   kind:        u32,
   bytes:       Vec<u8>,
   path_offset: usize,
}

impl Command {
   fn path(&self) -> io::Result<String> {
      let tail = &self.bytes[self.path_offset..];
      let end = tail
         .iter()
         .position(|byte| *byte == 0)
         .unwrap_or(tail.len());
      String::from_utf8(tail[..end].to_vec()).map_err(io::Error::other)
   }

   fn set_path(&mut self, path: &str) -> io::Result<()> {
      self.bytes.truncate(self.path_offset);
      self.bytes.extend_from_slice(path.as_bytes());
      self.bytes.resize((self.bytes.len() + 8) / 8 * 8, 0);
      let size = u32::try_from(self.bytes.len()).map_err(io::Error::other)?;
      self.bytes[4..8].copy_from_slice(&size.to_le_bytes());
      Ok(())
   }
}

fn read_commands(image: &Image) -> io::Result<(Vec<Command>, usize)> {
   let count = image.long(16)?;
   let mut room = image.bytes.len() - HEADER_SIZE;
   let mut commands = Vec::new();
   let mut offset = HEADER_SIZE;
   for _ in 0..count {
      let Ok(kind) = image.long(offset) else {
         break;
      };
      let size = usize::try_from(image.long(offset + 4)?).map_err(io::Error::other)?;
      if size < 8 || offset + size > image.bytes.len() {
         break;
      }
      let mut command = Command {
         kind,
         bytes: image.bytes[offset..offset + size].to_vec(),
         path_offset: 0,
      };
      if matches!(
         kind,
         LOAD_DYLIB | LOAD_WEAK_DYLIB | REEXPORT_DYLIB | LOAD_UPWARD_DYLIB | ID_DYLIB | RPATH
      ) {
         let path_offset =
            usize::try_from(image.long(offset + 8).unwrap_or(0)).map_err(io::Error::other)?;
         if path_offset < size {
            command.path_offset = path_offset;
         }
      }
      if kind == SEGMENT_64 {
         let file_offset = image.size(offset + 40).unwrap_or(0);
         let file_size = image.size(offset + 48).unwrap_or(0);
         if file_size != 0 && file_offset == 0 {
            let section_count =
               usize::try_from(image.long(offset + 64).unwrap_or(0)).map_err(io::Error::other)?;
            for section in 0..section_count {
               let section_offset =
                  usize::try_from(image.long(offset + 72 + section * 80 + 48).unwrap_or(0))
                     .map_err(io::Error::other)?;
               if section_offset != 0 {
                  room = room.min(section_offset.saturating_sub(HEADER_SIZE));
               }
            }
         } else if file_size != 0 {
            room = room.min(file_offset.saturating_sub(HEADER_SIZE));
         }
      }
      commands.push(command);
      offset += size;
   }
   Ok((commands, room))
}

fn resign(image: &mut Image) -> io::Result<()> {
   let count = image.long(16)?;
   let mut offset = HEADER_SIZE;
   let mut signature = None;
   for _ in 0..count {
      let Ok(kind) = image.long(offset) else {
         break;
      };
      let size = usize::try_from(image.long(offset + 4)?).map_err(io::Error::other)?;
      if size < 8 {
         break;
      }
      if kind == CODE_SIGNATURE {
         signature = Some(usize::try_from(image.long(offset + 8)?).map_err(io::Error::other)?);
      }
      offset += size;
   }
   let Some(signature_offset) = signature else {
      return Ok(());
   };
   if image.big_long(signature_offset)? != 0xFADE_0CC0 {
      return Err(io::Error::other(
         "LC_CODE_SIGNATURE is no embedded signature blob",
      ));
   }
   let blobs = image.big_long(signature_offset + 8)?;
   for blob in 0..usize::try_from(blobs).map_err(io::Error::other)? {
      let directory = signature_offset
         + usize::try_from(image.big_long(signature_offset + 12 + blob * 8 + 4)?)
            .map_err(io::Error::other)?;
      if image.big_long(directory)? != 0xFADE_0C02 {
         continue;
      }
      let hash_offset =
         usize::try_from(image.big_long(directory + 16)?).map_err(io::Error::other)?;
      let slots = usize::try_from(image.big_long(directory + 28)?).map_err(io::Error::other)?;
      let limit = usize::try_from(image.big_long(directory + 32)?).map_err(io::Error::other)?;
      let hash_size = usize::from(image.byte(directory + 36)?);
      let hash_type = image.byte(directory + 37)?;
      let page = 1_usize << image.byte(directory + 39)?;
      if hash_type != 2 || hash_size != 32 {
         return Err(io::Error::other(format!(
            "code signature hash type {hash_type} is not SHA-256"
         )));
      }
      for slot in 0..slots {
         let begin = slot * page;
         let end = (begin + page).min(limit);
         let region = image
            .bytes
            .get(begin..end)
            .ok_or_else(|| io::Error::other("code signature page lies outside the file"))?;
         let digest = Sha256::digest(region);
         image.write(directory + hash_offset + slot * hash_size, &digest)?;
      }
   }
   Ok(())
}

fn loader_relative(from: &Path, target: &Path) -> String {
   let relative = relative_from(from, target);
   if relative == Path::new(".") {
      "@loader_path".to_owned()
   } else {
      format!("@loader_path/{}", relative.display())
   }
}

fn system_path(path: &str) -> bool {
   path.starts_with("/usr/lib/") || path.starts_with("/System/")
}

fn resolve(context: &Context, here: &Path, path: &str) -> io::Result<Option<PathBuf>> {
   if system_path(path) {
      let known = match context.sdk {
         Some(ref sdk) => {
            let stem = path.strip_suffix(".dylib").unwrap_or(path);
            sdk.join(format!("{}.tbd", &stem[1..])).try_exists()?
         },
         None => true,
      };

      return Ok(known.then(|| PathBuf::from(path)));
   }

   let target = if let Some(name) = path.strip_prefix("@rpath/") {
      let mut found = None;

      for directory in &context.directories {
         let candidate = directory.join(name);

         if candidate.try_exists()? {
            found = Some(context.final_path(&candidate));
            break;
         }
      }

      found
   } else if let Some(relative) = path.strip_prefix("@loader_path/") {
      Some(normalize(&here.join(relative)))
   } else {
      let absolute = context.final_path(Path::new(path));
      is_store_path(&absolute.to_string_lossy()).then_some(absolute)
   };

   if let Some(ref resolved) = target {
      let disk = context.on_disk(resolved);

      if disk != *resolved && !disk.try_exists()? {
         return Ok(None);
      }
   }

   Ok(target)
}

fn new_spelling(context: &Context, file: &Path, command: &Command) -> io::Result<Option<String>> {
   if command.kind == ID_DYLIB {
      return Ok(Some(file.to_string_lossy().into_owned()));
   }

   let path = command.path()?;
   let here = file
      .parent()
      .ok_or_else(|| io::Error::other("Mach-O file has no parent directory"))?;

   if command.kind == RPATH {
      let target = context.final_path(Path::new(&path));
      return Ok(is_store_path(&target.to_string_lossy()).then(|| loader_relative(here, &target)));
   }

   let target = resolve(context, here, &path)?.ok_or_else(|| {
      io::Error::other(format!(
         "links {path}, which neither the package, a dependency nor the SDK has"
      ))
   })?;
   Ok(Some(if system_path(&path) {
      path
   } else {
      loader_relative(here, &target)
   }))
}

#[inline]
pub fn fix(context: &Context, path: &Path, image: &mut Image) -> io::Result<()> {
   if image.long(0).ok() != Some(MAGIC) {
      return Ok(());
   }
   let file = context.final_path(path);
   let (mut commands, room) = read_commands(image)?;
   let mut changed = Vec::new();
   for command in &mut commands {
      if command.path_offset == 0 {
         continue;
      }
      if let Some(text) = new_spelling(context, &file, command)?
         && text != command.path()?
      {
         command.set_path(&text)?;
         changed.push(text);
      }
   }
   if changed.is_empty() {
      return Ok(());
   }
   let mut bytes = commands
      .iter()
      .flat_map(|command| command.bytes.iter().copied())
      .collect::<Vec<u8>>();
   if bytes.len() > room {
      return Err(io::Error::other(format!(
         "load commands need {} bytes, the header has room for {room} (-headerpad)",
         bytes.len()
      )));
   }
   let size = u32::try_from(bytes.len()).map_err(io::Error::other)?;
   image.write(20, &size.to_le_bytes())?;
   bytes.resize(room, 0);
   image.write(HEADER_SIZE, &bytes)?;
   resign(image)?;
   write_back(path, image)?;
   writeln!(
      io::stdout().lock(),
      "relocated {}  {}",
      context.shown(path),
      changed.join("  ")
   )?;
   Ok(())
}
