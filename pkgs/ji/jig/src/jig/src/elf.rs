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

pub fn is_elf(image: &Image) -> bool {
   image.bytes.starts_with(b"\x7fELF\x02\x01")
}

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
