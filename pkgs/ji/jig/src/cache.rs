use std::{
   io::{
      self,
      BufRead,
      BufReader,
      Read,
      Write,
   },
   os::unix::net::UnixStream,
   path::Path,
};

use zstd::{
   bulk::compress,
   stream::read::Decoder,
};

pub const MAX_OBJECT_SIZE: usize = 2 << 30;
const MAX_LINE_SIZE: usize = 65536;
const BATCH_SIZE: usize = 256;

pub struct CacheClient {
   reader: Option<BufReader<UnixStream>>,
}

impl CacheClient {
   pub fn connect(socket: &Path) -> io::Result<Self> {
      Ok(Self {
         reader: Some(BufReader::with_capacity(
            65536,
            UnixStream::connect(socket)?,
         )),
      })
   }

   fn exchange<Value>(
      &mut self,
      operation: impl FnOnce(&mut BufReader<UnixStream>) -> io::Result<Value>,
   ) -> io::Result<Value> {
      let result = self.reader.as_mut().map_or_else(
         || {
            Err(io::Error::new(
               io::ErrorKind::NotConnected,
               "cache disconnected",
            ))
         },
         operation,
      );
      if result.is_err() {
         self.reader = None;
      }
      result
   }

   pub fn get(&mut self, key: &str) -> io::Result<Option<Vec<u8>>> {
      validate_key(key)?;
      self.exchange(|reader| {
         writeln!(reader.get_mut(), "GET {key}")?;
         receive_value(reader)
      })
   }

   pub fn get_many(&mut self, keys: &[String]) -> io::Result<Vec<Option<Vec<u8>>>> {
      keys.iter().try_for_each(|key| validate_key(key))?;
      self.exchange(|reader| {
         let mut values = Vec::with_capacity(keys.len());
         for batch in keys.chunks(BATCH_SIZE) {
            let mut request = Vec::new();
            for key in batch {
               writeln!(request, "GET {key}")?;
            }
            reader.get_mut().write_all(&request)?;
            for _ in batch {
               values.push(receive_value(reader)?);
            }
         }
         Ok(values)
      })
   }

   pub fn put(&mut self, key: &str, value: &[u8]) -> io::Result<()> {
      validate_key(key)?;
      let wire = encode_value(value)?;
      self.exchange(|reader| {
         writeln!(reader.get_mut(), "PUT {key} {}", wire.len())?;
         reader.get_mut().write_all(&wire)?;
         expect_ok(reader)
      })
   }

   pub fn has_many(&mut self, keys: &[String]) -> io::Result<Vec<bool>> {
      keys.iter().try_for_each(|key| validate_key(key))?;
      self.exchange(|reader| {
         let mut present = Vec::with_capacity(keys.len());
         for batch in keys.chunks(BATCH_SIZE) {
            let mut request = Vec::new();
            for key in batch {
               writeln!(request, "HAS {key}")?;
            }
            reader.get_mut().write_all(&request)?;
            for _ in batch {
               present.push(match read_line(reader)?.as_str() {
                  "1" => true,
                  "0" => false,
                  _ => return Err(io::Error::other("invalid cache presence response")),
               });
            }
         }
         Ok(present)
      })
   }

   pub fn identities(&mut self, paths: &[String]) -> io::Result<Vec<Option<String>>> {
      for path in paths {
         if path.contains(['\n', '\r', '\0']) || path.len() >= MAX_LINE_SIZE {
            return Err(io::Error::other("invalid cache identity path"));
         }
      }
      self.exchange(|reader| {
         let mut identities = Vec::with_capacity(paths.len());
         for batch in paths.chunks(BATCH_SIZE) {
            writeln!(reader.get_mut(), "IDS {}", batch.len())?;
            for path in batch {
               writeln!(reader.get_mut(), "{path}")?;
            }
            for _ in batch {
               let identity = read_line(reader)?;
               if !identity.is_empty()
                  && (identity.len() != 34
                     || !identity.starts_with("C:")
                     || !identity.as_bytes()[2..].iter().all(u8::is_ascii_hexdigit))
               {
                  return Err(io::Error::other("invalid cache identity response"));
               }
               identities.push((!identity.is_empty()).then_some(identity));
            }
         }
         Ok(identities)
      })
   }

   pub fn acquire(&mut self, build: &str) -> io::Result<()> {
      validate_key(build)?;
      self.exchange(|reader| {
         writeln!(reader.get_mut(), "SLOT {build}")?;
         expect_ok(reader)
      })
   }

   pub fn release(&mut self, build: &str) -> io::Result<()> {
      validate_key(build)?;
      self.exchange(|reader| {
         writeln!(reader.get_mut(), "DONE {build}")?;
         expect_ok(reader)
      })
   }
}

fn validate_key(key: &str) -> io::Result<()> {
   if key.is_empty()
      || key.len() > 4096
      || key
         .bytes()
         .any(|byte| byte.is_ascii_whitespace() || byte == 0)
   {
      return Err(io::Error::other("invalid cache key"));
   }
   Ok(())
}

fn read_line(reader: &mut impl BufRead) -> io::Result<String> {
   let mut bytes = Vec::new();
   let length = reader
      .take(MAX_LINE_SIZE as u64 + 1)
      .read_until(b'\n', &mut bytes)?;
   if length > MAX_LINE_SIZE || bytes.pop() != Some(b'\n') {
      return Err(io::Error::new(
         io::ErrorKind::InvalidData,
         "unterminated or oversized cache line",
      ));
   }
   String::from_utf8(bytes).map_err(io::Error::other)
}

fn expect_ok(reader: &mut impl BufRead) -> io::Result<()> {
   if read_line(reader)? != "OK" {
      return Err(io::Error::other("cache did not acknowledge request"));
   }
   Ok(())
}

fn encode_value(value: &[u8]) -> io::Result<Vec<u8>> {
   if value.len() > MAX_OBJECT_SIZE {
      return Err(io::Error::other("cache object exceeds size limit"));
   }
   let mut wire = Vec::from((value.len() as u32).to_ne_bytes());
   wire.extend(compress(value, 1)?);
   if wire.len() > MAX_OBJECT_SIZE {
      return Err(io::Error::other(
         "compressed cache object exceeds size limit",
      ));
   }
   Ok(wire)
}

fn receive_value(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
   let response = read_line(reader)?;
   if response == "MISS" {
      return Ok(None);
   }
   let length = response
      .strip_prefix("OK ")
      .ok_or_else(|| io::Error::other("invalid cache value response"))?
      .parse::<usize>()
      .map_err(io::Error::other)?;
   if !(4..=MAX_OBJECT_SIZE).contains(&length) {
      return Err(io::Error::other("invalid cache object length"));
   }
   let mut header = [0; 4];
   reader.read_exact(&mut header)?;
   let expected = u32::from_ne_bytes(header) as usize;
   if expected > MAX_OBJECT_SIZE {
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
   Ok(Some(value))
}
