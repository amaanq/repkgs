use std::{
   io,
   path::{
      Path,
      PathBuf,
   },
};

use crate::{
   image::Image,
   store::normalize,
};

pub struct Section {
   pub name:   u32,
   pub kind:   u32,
   pub offset: usize,
   pub size:   usize,
   pub link:   usize,
   pub stride: usize,
}
pub struct Segment {
   pub header:  usize,
   pub kind:    u32,
   pub offset:  usize,
   pub address: u64,
   pub size:    usize,
}

#[must_use]
#[inline]
pub fn is_elf(image: &Image) -> bool {
   image.bytes.starts_with(b"\x7fELF\x02\x01")
}

#[inline]
pub fn sections(image: &Image) -> io::Result<Vec<Section>> {
   let offset = image.size(40)?;
   let stride = usize::from(image.word(58)?);
   let count = usize::from(image.word(60)?);
   if count != 0 && stride < 64 {
      return Err(io::Error::other("ELF section header is too short"));
   }
   let mut sections = Vec::with_capacity(count);
   for index in 0..count {
      let start = offset
         .checked_add(index * stride)
         .ok_or_else(|| io::Error::other("ELF section offset overflows"))?;
      image.array::<64>(start)?;
      let section = Section {
         name:   image.long(start)?,
         kind:   image.long(start + 4)?,
         offset: image.size(start + 24)?,
         size:   image.size(start + 32)?,
         link:   image
            .long(start + 40)?
            .try_into()
            .map_err(io::Error::other)?,
         stride: image.size(start + 56)?,
      };
      if section.kind != 8
         && section
            .offset
            .checked_add(section.size)
            .is_none_or(|end| end > image.bytes.len())
      {
         return Err(io::Error::other("ELF section extends beyond the file"));
      }
      sections.push(section);
   }
   Ok(sections)
}

/// The functions and data a symbol table of `kind` (2 for `.symtab`, 11 for
/// `.dynsym`) defines with global or weak binding.
#[inline]
pub fn defined_symbols(image: &Image, sections: &[Section], kind: u32) -> io::Result<Vec<String>> {
   let mut found = Vec::new();
   for table in sections.iter().filter(|section| section.kind == kind) {
      let strings = sections
         .get(table.link)
         .ok_or_else(|| io::Error::other("ELF symbol table names no string table"))?;
      if table.stride < 24 {
         return Err(io::Error::other("ELF symbol entry is too short"));
      }
      for start in (table.offset..table.offset + table.size).step_by(table.stride) {
         let info = image.byte(start + 4)?;
         let index = image.word(start + 6)?;
         if index == 0
            || index == 0xFFF1
            || !matches!(info >> 4_u8, 1 | 2)
            || !matches!(info & 0xF, 1 | 2)
         {
            continue;
         }
         let name = usize::try_from(image.long(start)?).map_err(io::Error::other)?;
         let offset = strings
            .offset
            .checked_add(name)
            .ok_or_else(|| io::Error::other("ELF symbol name offset overflows"))?;
         found.push(image.string(offset)?);
      }
   }
   Ok(found)
}

#[inline]
pub fn names(image: &Image, sections: &[Section]) -> io::Result<Vec<String>> {
   let strings = sections
      .get(usize::from(image.word(62)?))
      .ok_or_else(|| io::Error::other("ELF has no section name table"))?;
   sections
      .iter()
      .map(|section| {
         let offset = strings
            .offset
            .checked_add(usize::try_from(section.name).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("ELF section name offset overflows"))?;
         image.string(offset)
      })
      .collect()
}

pub struct Note {
   pub owner:       String,
   pub kind:        u32,
   pub description: Vec<u8>,
}

#[inline]
pub fn notes(image: &Image, sections: &[Section]) -> io::Result<Vec<Note>> {
   let mut notes = Vec::new();
   for section in sections.iter().filter(|section| section.kind == 7) {
      let end = section.offset + section.size;
      let mut cursor = section.offset;
      while cursor + 12 <= end {
         let owner_size = usize::try_from(image.long(cursor)?).map_err(io::Error::other)?;
         let description_size =
            usize::try_from(image.long(cursor + 4)?).map_err(io::Error::other)?;
         let kind = image.long(cursor + 8)?;
         let owner_start = cursor + 12;
         let description_start = owner_start + owner_size.next_multiple_of(4);
         let next = description_start + description_size.next_multiple_of(4);
         if next > end {
            return Err(io::Error::other("ELF note extends beyond its section"));
         }
         let raw_owner = &image.bytes[owner_start..owner_start + owner_size];
         let owner = raw_owner.strip_suffix(b"\0").unwrap_or(raw_owner);
         notes.push(Note {
            owner: String::from_utf8_lossy(owner).into_owned(),
            kind,
            description: image.bytes[description_start..description_start + description_size]
               .to_vec(),
         });
         cursor = next;
      }
   }
   Ok(notes)
}

#[inline]
pub fn segments(image: &Image) -> io::Result<Vec<Segment>> {
   let offset = image.size(32)?;
   let stride = usize::from(image.word(54)?);
   let count = usize::from(image.word(56)?);
   if count != 0 && stride < 56 {
      return Err(io::Error::other("ELF program header is too short"));
   }
   let mut segments = Vec::with_capacity(count);
   for index in 0..count {
      let start = offset
         .checked_add(index * stride)
         .ok_or_else(|| io::Error::other("ELF segment offset overflows"))?;
      image.array::<56>(start)?;
      let segment = Segment {
         header:  start,
         kind:    image.long(start)?,
         offset:  image.size(start + 8)?,
         address: image.quad(start + 16)?,
         size:    image.size(start + 32)?,
      };
      if segment
         .offset
         .checked_add(segment.size)
         .is_none_or(|end| end > image.bytes.len())
      {
         return Err(io::Error::other("ELF segment extends beyond the file"));
      }
      segments.push(segment);
   }
   Ok(segments)
}

#[must_use]
#[inline]
pub fn has_segment(segments: &[Segment], kind: u32) -> bool {
   segments.iter().any(|segment| segment.kind == kind)
}

fn file_offset(segments: &[Segment], address: u64) -> Option<usize> {
   segments
      .iter()
      .filter(|segment| segment.kind == 1)
      .find(|segment| {
         address >= segment.address
            && address - segment.address < u64::try_from(segment.size).unwrap_or(u64::MAX)
      })
      .and_then(|segment| {
         segment
            .offset
            .checked_add(usize::try_from(address - segment.address).ok()?)
      })
}

struct Dynamic {
   strings: Option<usize>,
   entries: Vec<(u64, usize)>,
}

/// A NUL-terminated string in the file, at `offset`, with `capacity` bytes
/// reserved for it including the terminator.
pub struct Text {
   pub offset:   usize,
   pub capacity: usize,
   pub value:    String,
}

fn dynamic(image: &Image, segments: &[Segment]) -> io::Result<Option<Dynamic>> {
   let Some(segment) = segments.iter().find(|segment| segment.kind == 2) else {
      return Ok(None);
   };
   let mut table = Dynamic {
      strings: None,
      entries: Vec::new(),
   };
   for offset in (segment.offset..segment.offset + segment.size).step_by(16) {
      match image.quad(offset)? {
         0 => break,
         5 => table.strings = file_offset(segments, image.quad(offset + 8)?),
         tag => table.entries.push((tag, image.size(offset + 8)?)),
      }
   }
   Ok(Some(table))
}

fn table_text(image: &Image, table: &Dynamic, entry: usize) -> io::Result<Option<Text>> {
   let Some(strings) = table.strings else {
      return Ok(None);
   };
   let offset = strings
      .checked_add(entry)
      .ok_or_else(|| io::Error::other("ELF dynamic string offset overflows"))?;
   let value = image.string(offset)?;
   Ok(Some(Text {
      offset,
      capacity: value.len() + 1,
      value,
   }))
}

#[inline]
pub fn runpath_text(image: &Image, segments: &[Segment]) -> io::Result<Option<Text>> {
   let Some(table) = dynamic(image, segments)? else {
      return Ok(None);
   };
   let by_tag = |wanted: u64| {
      table
         .entries
         .iter()
         .find(|&&(tag, _)| tag == wanted)
         .map(|&(_, value)| value)
   };
   let Some(entry) = by_tag(29).or_else(|| by_tag(15)) else {
      return Ok(None);
   };
   table_text(image, &table, entry)
}

#[inline]
pub fn runpath(image: &Image, segments: &[Segment]) -> io::Result<Option<String>> {
   Ok(runpath_text(image, segments)?.map(|text| text.value))
}

#[inline]
pub fn needed(image: &Image, segments: &[Segment]) -> io::Result<Vec<Text>> {
   let Some(table) = dynamic(image, segments)? else {
      return Ok(Vec::new());
   };
   table
      .entries
      .iter()
      .filter(|&&(tag, _)| tag == 1)
      .filter_map(|&(_, entry)| table_text(image, &table, entry).transpose())
      .collect()
}

/// `PT_INTERP`, or the one reloc-fixup disabled to `PT_NULL` for the entry
/// stub, which still holds the path.
#[inline]
pub fn interpreter(image: &Image, segments: &[Segment]) -> io::Result<Option<Text>> {
   segments
      .iter()
      .find(|segment| segment.kind == 3 || (segment.kind == 0 && segment.size > 1))
      .map(|segment| {
         Ok(Text {
            offset:   segment.offset,
            capacity: segment.size,
            value:    image.string(segment.offset)?,
         })
      })
      .transpose()
}

#[must_use]
#[inline]
pub fn relative_from(directory: &Path, target: &Path) -> PathBuf {
   let base = normalize(directory);
   let destination = normalize(target);
   let base_parts = base.components().collect::<Vec<_>>();
   let target_parts = destination.components().collect::<Vec<_>>();
   let common = base_parts
      .iter()
      .zip(&target_parts)
      .take_while(|&(left, right)| left == right)
      .count();
   let mut relative = PathBuf::new();
   for _ in common..base_parts.len() {
      relative.push("..");
   }
   for component in &target_parts[common..] {
      relative.push(component);
   }
   if relative.as_os_str().is_empty() {
      relative.push(".");
   }
   relative
}

pub const RUNPATH_SLACK: usize = 48;

#[must_use]
#[inline]
pub fn shared_library(name: &str) -> bool {
   if Path::new(name)
      .extension()
      .is_some_and(|extension| extension == "dylib")
   {
      return true;
   }
   name.rsplit_once(".so").is_some_and(|(_, tail)| {
      tail.is_empty()
         || tail.strip_prefix('.').is_some_and(|suffix| {
            suffix
               .bytes()
               .all(|byte| byte == b'.' || byte.is_ascii_digit())
         })
   })
}
