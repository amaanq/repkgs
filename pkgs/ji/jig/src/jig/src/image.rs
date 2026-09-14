use std::io;

pub struct Image {
   pub bytes: Vec<u8>,
}

impl Image {
   #[inline]
   pub fn array<const SIZE: usize>(&self, offset: usize) -> io::Result<[u8; SIZE]> {
      self
         .bytes
         .get(offset..)
         .and_then(|tail| tail.get(..SIZE))
         .and_then(|bytes| bytes.try_into().ok())
         .ok_or_else(|| io::Error::other("binary field is outside the file"))
   }

   #[inline]
   pub fn byte(&self, offset: usize) -> io::Result<u8> {
      self.array(offset).map(|[byte]| byte)
   }
   #[inline]
   pub fn word(&self, offset: usize) -> io::Result<u16> {
      self.array(offset).map(u16::from_le_bytes)
   }
   #[inline]
   pub fn long(&self, offset: usize) -> io::Result<u32> {
      self.array(offset).map(u32::from_le_bytes)
   }
   #[inline]
   pub fn big_long(&self, offset: usize) -> io::Result<u32> {
      self.array(offset).map(u32::from_be_bytes)
   }
   #[inline]
   pub fn quad(&self, offset: usize) -> io::Result<u64> {
      self.array(offset).map(u64::from_le_bytes)
   }
   #[inline]
   pub fn size(&self, offset: usize) -> io::Result<usize> {
      self.quad(offset)?.try_into().map_err(io::Error::other)
   }

   #[inline]
   pub fn write(&mut self, offset: usize, data: &[u8]) -> io::Result<()> {
      self
         .bytes
         .get_mut(offset..)
         .and_then(|tail| tail.get_mut(..data.len()))
         .ok_or_else(|| io::Error::other("binary write is outside the file"))?
         .copy_from_slice(data);
      Ok(())
   }

   #[inline]
   pub fn string(&self, offset: usize) -> io::Result<String> {
      let tail = self
         .bytes
         .get(offset..)
         .ok_or_else(|| io::Error::other("binary string is outside the file"))?;
      let end = tail
         .iter()
         .position(|byte| *byte == 0)
         .ok_or_else(|| io::Error::other("binary string is unterminated"))?;
      String::from_utf8(tail[..end].to_vec()).map_err(io::Error::other)
   }

   #[inline]
   pub fn padded(&mut self, offset: usize, capacity: usize, data: &[u8]) -> io::Result<()> {
      if data.len() >= capacity {
         return Err(io::Error::other(
            "relocation does not fit the reserved space",
         ));
      }
      let target = self
         .bytes
         .get_mut(offset..)
         .and_then(|tail| tail.get_mut(..capacity))
         .ok_or_else(|| io::Error::other("binary padding is outside the file"))?;
      target.fill(0);
      target[..data.len()].copy_from_slice(data);
      Ok(())
   }
}
