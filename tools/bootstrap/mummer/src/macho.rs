use std::{
   ffi::CStr,
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
   elf::prefixed_relative,
   files::utf8,
   fixup::{
      Context,
      write_back,
   },
   image::Image,
   store::{
      in_store,
      normalize,
   },
   tree::find,
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
      let path = CStr::from_bytes_until_nul(tail).map_or(tail, CStr::to_bytes);
      String::from_utf8(path.to_vec()).map_err(io::Error::other)
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

/// A load command, its kind and the file range it spans.
struct Load {
   kind:   u32,
   offset: usize,
   size:   usize,
}

fn advance(base: usize, by: usize) -> io::Result<usize> {
   base
      .checked_add(by)
      .ok_or_else(|| io::Error::other("Mach-O offset overflows"))
}

fn load_commands(image: &Image) -> io::Result<Vec<Load>> {
   let count = image.long_usize(16)?;
   let mut loads = Vec::with_capacity(count);
   let mut offset = HEADER_SIZE;
   for _ in 0..count {
      let kind = image.long(offset)?;
      let size = image.long_usize(offset + 4)?;
      let end = advance(offset, size)?;
      if size < 8 || end > image.bytes.len() {
         return Err(io::Error::other("Mach-O load command is malformed"));
      }
      loads.push(Load { kind, offset, size });
      offset = end;
   }
   Ok(loads)
}

/// The space before the first section's contents, what the load commands
/// may grow into.
fn header_room(image: &Image, loads: &[Load]) -> io::Result<usize> {
   let mut room = image.bytes.len() - HEADER_SIZE;
   for load in loads.iter().filter(|load| load.kind == SEGMENT_64) {
      let file_offset = image.size(load.offset + 40)?;
      let file_size = image.size(load.offset + 48)?;
      if file_size == 0 {
         continue;
      }
      if file_offset != 0 {
         room = room.min(file_offset.saturating_sub(HEADER_SIZE));
         continue;
      }
      for section in 0..image.long_usize(load.offset + 64)? {
         let header = advance(
            load.offset + 72,
            section
               .checked_mul(80)
               .ok_or_else(|| io::Error::other("Mach-O section offset overflows"))?,
         )?;
         let section_offset = image.long_usize(header + 48)?;
         if section_offset != 0 {
            room = room.min(section_offset.saturating_sub(HEADER_SIZE));
         }
      }
   }
   Ok(room)
}

fn read_commands(image: &Image, loads: &[Load]) -> io::Result<Vec<Command>> {
   loads
      .iter()
      .map(|load| {
         let mut command = Command {
            kind:        load.kind,
            bytes:       image.bytes[load.offset..load.offset + load.size].to_vec(),
            path_offset: 0,
         };
         if matches!(
            load.kind,
            LOAD_DYLIB | LOAD_WEAK_DYLIB | REEXPORT_DYLIB | LOAD_UPWARD_DYLIB | ID_DYLIB | RPATH
         ) {
            let path_offset = image.long_usize(load.offset + 8)?;
            if path_offset < load.size {
               command.path_offset = path_offset;
            }
         }
         Ok(command)
      })
      .collect()
}

/// Rehashes the code directory pages, true when any hash was stale.
fn resign(image: &mut Image) -> io::Result<bool> {
   let mut signature = None;
   for load in load_commands(image)? {
      if load.kind == CODE_SIGNATURE {
         signature = Some(image.long_usize(load.offset + 8)?);
      }
   }
   let Some(signature_offset) = signature else {
      return Ok(false);
   };
   let mut stale = false;
   if image.big_long(signature_offset)? != 0xFADE_0CC0 {
      return Err(io::Error::other(
         "LC_CODE_SIGNATURE is no embedded signature blob",
      ));
   }
   let blobs = image.big_long(advance(signature_offset, 8)?)?;
   for blob in 0..usize::try_from(blobs).map_err(io::Error::other)? {
      let index = advance(signature_offset + 12, blob.saturating_mul(8))?;
      let directory = advance(
         signature_offset,
         usize::try_from(image.big_long(advance(index, 4)?)?).map_err(io::Error::other)?,
      )?;
      if image.big_long(directory)? != 0xFADE_0C02 {
         continue;
      }
      let field = |at: usize| -> io::Result<usize> {
         usize::try_from(image.big_long(advance(directory, at)?)?).map_err(io::Error::other)
      };
      let hash_offset = field(16)?;
      let slots = field(28)?;
      let limit = field(32)?;
      let hash_size = usize::from(image.byte(advance(directory, 36)?)?);
      let hash_type = image.byte(advance(directory, 37)?)?;
      let page = 1_usize
         .checked_shl(u32::from(image.byte(advance(directory, 39)?)?))
         .ok_or_else(|| io::Error::other("code signature page size overflows"))?;
      if hash_type != 2 || hash_size != 32 {
         return Err(io::Error::other(format!(
            "code signature hash type {hash_type} is not SHA-256"
         )));
      }
      let hashes = advance(directory, hash_offset)?;
      for slot in 0..slots {
         let overflow = || io::Error::other("code signature slot overflows");
         let begin = slot.checked_mul(page).ok_or_else(overflow)?;
         let end = begin.saturating_add(page).min(limit);
         let region = image
            .bytes
            .get(begin..end)
            .ok_or_else(|| io::Error::other("code signature page lies outside the file"))?;
         let digest = Sha256::digest(region);
         let at = advance(hashes, slot.checked_mul(hash_size).ok_or_else(overflow)?)?;
         if image.bytes.get(at..at + hash_size) != Some(digest.as_slice()) {
            image.write(at, &digest)?;
            stale = true;
         }
      }
   }
   Ok(stale)
}

fn loader_relative(from: &Path, target: &Path) -> String {
   prefixed_relative("@loader_path", from, target)
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

      for directory in context.directories.iter().chain(&context.libraries) {
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
      in_store(&absolute).then_some(absolute)
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
   // Nix rewrites a content-addressed output's own path after the build, which
   // would break the ad-hoc signature, so the id must not spell $out
   if command.kind == ID_DYLIB {
      let name = file
         .file_name()
         .ok_or_else(|| io::Error::other("Mach-O dylib has no file name"))?;
      return Ok(Some(format!("@rpath/{}", utf8(Path::new(name))?)));
   }

   let path = command.path()?;
   let here = file
      .parent()
      .ok_or_else(|| io::Error::other("Mach-O file has no parent directory"))?;

   if command.kind == RPATH {
      let target = context.final_path(Path::new(&path));
      return Ok(in_store(&target).then(|| loader_relative(here, &target)));
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
   let loads = load_commands(image)?;
   let room = header_room(image, &loads)?;
   let mut commands = read_commands(image, &loads)?;
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
      if resign(image)? {
         write_back(path, image)?;
      }
      return refuse_own_path(context, image);
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
   refuse_own_path(context, image)
}

fn refuse_own_path(context: &Context, image: &Image) -> io::Result<()> {
   let hash = context
      .dest
      .file_name()
      .and_then(|name| name.to_str())
      .and_then(|name| name.get(..32))
      .ok_or_else(|| io::Error::other("the output path has no store hash"))?;
   if find(&image.bytes, hash.as_bytes()).is_some() {
      return Err(io::Error::other(format!(
         "spells its own output path {}, which Nix rewrites after the ad-hoc signature",
         context.dest.display()
      )));
   }
   Ok(())
}
