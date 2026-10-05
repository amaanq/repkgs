pub mod client;
pub mod digest;
pub mod identity;
pub mod key;
pub mod request;

use std::io::{
   self,
   BufRead,
   Read as _,
};

use zstd::{
   bulk::compress,
   stream::read::Decoder,
};

pub const MAX_OBJECT: usize = 2 << 30;
pub const MAX_LINE: usize = 0x0001_0000;
pub const MAX_KEY: usize = 4096;

#[inline]
pub fn read_line<Reader>(reader: &mut Reader) -> io::Result<Option<String>>
where
   Reader: BufRead,
{
   let mut bytes = Vec::new();
   let length = reader
      .take(u64::try_from(MAX_LINE).map_err(io::Error::other)? + 1)
      .read_until(b'\n', &mut bytes)?;
   if length == 0 {
      return Ok(None);
   }
   if length > MAX_LINE || bytes.pop() != Some(b'\n') {
      return Err(io::Error::new(
         io::ErrorKind::InvalidData,
         "unterminated or oversized cache line",
      ));
   }
   String::from_utf8(bytes).map(Some).map_err(io::Error::other)
}

#[inline]
pub fn encode(value: &[u8]) -> io::Result<Vec<u8>> {
   if value.len() > MAX_OBJECT {
      return Err(io::Error::other("cache object exceeds size limit"));
   }
   let length = u32::try_from(value.len()).map_err(io::Error::other)?;
   let mut frame = Vec::from(length.to_ne_bytes());
   frame.extend(compress(value, 1)?);
   if frame.len() > MAX_OBJECT {
      return Err(io::Error::other(
         "compressed cache object exceeds size limit",
      ));
   }
   Ok(frame)
}

#[inline]
pub fn decode<Reader>(reader: &mut Reader, length: usize) -> io::Result<Vec<u8>>
where
   Reader: BufRead,
{
   if !(4..=MAX_OBJECT).contains(&length) {
      return Err(io::Error::other("invalid cache object length"));
   }
   let mut header = [0; 4];
   reader.read_exact(&mut header)?;
   let expected = usize::try_from(u32::from_ne_bytes(header)).map_err(io::Error::other)?;
   if expected > MAX_OBJECT {
      return Err(io::Error::other(
         "decompressed cache object exceeds size limit",
      ));
   }
   let mut frame = reader.take(u64::try_from(length - 4).map_err(io::Error::other)?);
   let mut value = Vec::new();
   let mut decoder = Decoder::new(&mut frame)?;
   decoder.window_log_max(27)?;
   decoder
      .take(u64::try_from(expected).map_err(io::Error::other)? + 1)
      .read_to_end(&mut value)?;
   if value.len() != expected || frame.limit() != 0 {
      return Err(io::Error::other(
         "cache object length does not match its contents",
      ));
   }
   Ok(value)
}
