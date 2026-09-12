use std::{
   io,
   path::{
      Path,
      PathBuf,
   },
};

use crate::store::normalize;

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
pub struct Image {
   pub bytes: Vec<u8>,
}

impl Image {
   pub fn is_supported(&self) -> bool {
      self.bytes.starts_with(b"\x7fELF\x02\x01")
   }

   pub fn array<const SIZE: usize>(&self, offset: usize) -> io::Result<[u8; SIZE]> {
      self
         .bytes
         .get(offset..)
         .and_then(|tail| tail.get(..SIZE))
         .and_then(|bytes| bytes.try_into().ok())
         .ok_or_else(|| io::Error::other("ELF field is outside the file"))
   }

   pub fn word(&self, offset: usize) -> io::Result<u16> {
      self.array(offset).map(u16::from_le_bytes)
   }
   pub fn long(&self, offset: usize) -> io::Result<u32> {
      self.array(offset).map(u32::from_le_bytes)
   }
   pub fn quad(&self, offset: usize) -> io::Result<u64> {
      self.array(offset).map(u64::from_le_bytes)
   }
   pub fn size(&self, offset: usize) -> io::Result<usize> {
      self.quad(offset)?.try_into().map_err(io::Error::other)
   }

   pub fn write(&mut self, offset: usize, data: &[u8]) -> io::Result<()> {
      self
         .bytes
         .get_mut(offset..)
         .and_then(|tail| tail.get_mut(..data.len()))
         .ok_or_else(|| io::Error::other("ELF write is outside the file"))?
         .copy_from_slice(data);
      Ok(())
   }

   pub fn string(&self, offset: usize) -> io::Result<String> {
      let tail = self
         .bytes
         .get(offset..)
         .ok_or_else(|| io::Error::other("ELF string is outside the file"))?;
      let end = tail
         .iter()
         .position(|byte| *byte == 0)
         .ok_or_else(|| io::Error::other("ELF string is unterminated"))?;
      String::from_utf8(tail[..end].to_vec()).map_err(io::Error::other)
   }

   pub fn padded(&mut self, offset: usize, capacity: usize, data: &[u8]) -> io::Result<()> {
      if data.len() >= capacity {
         return Err(io::Error::other(
            "relocation does not fit reserved ELF space",
         ));
      }
      let target = self
         .bytes
         .get_mut(offset..)
         .and_then(|tail| tail.get_mut(..capacity))
         .ok_or_else(|| io::Error::other("ELF padding is outside the file"))?;
      target.fill(0);
      target[..data.len()].copy_from_slice(data);
      Ok(())
   }

   pub fn sections(&self) -> io::Result<Vec<Section>> {
      let offset = self.size(40)?;
      let stride = usize::from(self.word(58)?);
      let count = usize::from(self.word(60)?);
      if count != 0 && stride < 64 {
         return Err(io::Error::other("ELF section header is too short"));
      }
      let mut sections = Vec::with_capacity(count);
      for index in 0..count {
         let start = offset
            .checked_add(index * stride)
            .ok_or_else(|| io::Error::other("ELF section offset overflows"))?;
         self.array::<64>(start)?;
         let section = Section {
            kind:   self.long(start + 4)?,
            offset: self.size(start + 24)?,
            size:   self.size(start + 32)?,
            link:   self.long(start + 40)? as usize,
            stride: self.size(start + 56)?,
         };
         if section.kind != 8
            && section
               .offset
               .checked_add(section.size)
               .is_none_or(|end| end > self.bytes.len())
         {
            return Err(io::Error::other("ELF section extends beyond the file"));
         }
         sections.push(section);
      }
      Ok(sections)
   }

   pub fn segments(&self) -> io::Result<Vec<Segment>> {
      let offset = self.size(32)?;
      let stride = usize::from(self.word(54)?);
      let count = usize::from(self.word(56)?);
      if count != 0 && stride < 56 {
         return Err(io::Error::other("ELF program header is too short"));
      }
      let mut segments = Vec::with_capacity(count);
      for index in 0..count {
         let start = offset
            .checked_add(index * stride)
            .ok_or_else(|| io::Error::other("ELF segment offset overflows"))?;
         self.array::<56>(start)?;
         let segment = Segment {
            header:  start,
            kind:    self.long(start)?,
            offset:  self.size(start + 8)?,
            address: self.quad(start + 16)?,
            size:    self.size(start + 32)?,
         };
         if segment
            .offset
            .checked_add(segment.size)
            .is_none_or(|end| end > self.bytes.len())
         {
            return Err(io::Error::other("ELF segment extends beyond the file"));
         }
         segments.push(segment);
      }
      Ok(segments)
   }
}

pub fn relative_from(directory: &Path, target: &Path) -> PathBuf {
   let base = normalize(directory);
   let destination = normalize(target);
   let base_parts: Vec<_> = base.components().collect();
   let target_parts: Vec<_> = destination.components().collect();
   let common = base_parts
      .iter()
      .zip(&target_parts)
      .take_while(|(left, right)| left == right)
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
