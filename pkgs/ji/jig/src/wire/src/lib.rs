pub mod client;
pub mod identity;
pub mod request;

use std::io::{
   self,
   BufRead,
   Read,
};

use zstd::{
   bulk::compress,
   stream::read::Decoder,
};

pub const MAX_OBJECT: usize = 2 << 30;
pub const MAX_LINE: usize = 65536;
pub const MAX_KEY: usize = 4096;

pub fn check_key(key: &str) -> io::Result<()> {
   if key.is_empty()
      || key.len() > MAX_KEY
      || key
         .bytes()
         .any(|byte| byte.is_ascii_whitespace() || byte == 0)
   {
      return Err(io::Error::other("invalid cache key"));
   }
   Ok(())
}

pub fn read_line<Reader: BufRead>(reader: &mut Reader) -> io::Result<Option<String>> {
   let mut bytes = Vec::new();
   let length = reader
      .take(MAX_LINE as u64 + 1)
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

#[must_use]
pub fn digest(bytes: &[u8]) -> String {
   blake3::hash(bytes).to_hex()[..32].to_owned()
}

pub fn encode(value: &[u8]) -> io::Result<Vec<u8>> {
   if value.len() > MAX_OBJECT {
      return Err(io::Error::other("cache object exceeds size limit"));
   }
   let mut frame = Vec::from((value.len() as u32).to_ne_bytes());
   frame.extend(compress(value, 1)?);
   if frame.len() > MAX_OBJECT {
      return Err(io::Error::other(
         "compressed cache object exceeds size limit",
      ));
   }
   Ok(frame)
}

pub fn decode<Reader: BufRead>(reader: &mut Reader, length: usize) -> io::Result<Vec<u8>> {
   if !(4..=MAX_OBJECT).contains(&length) {
      return Err(io::Error::other("invalid cache object length"));
   }
   let mut header = [0; 4];
   reader.read_exact(&mut header)?;
   let expected = u32::from_ne_bytes(header) as usize;
   if expected > MAX_OBJECT {
      return Err(io::Error::other(
         "decompressed cache object exceeds size limit",
      ));
   }
   let mut frame = reader.take((length - 4) as u64);
   let mut value = Vec::new();
   let mut decoder = Decoder::new(&mut frame)?;
   decoder.window_log_max(27)?;
   decoder.take(expected as u64 + 1).read_to_end(&mut value)?;
   if value.len() != expected || frame.limit() != 0 {
      return Err(io::Error::other(
         "cache object length does not match its contents",
      ));
   }
   Ok(value)
}
