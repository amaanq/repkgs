use std::{
   io::{
      self,
      BufRead,
      BufReader,
      Write as _,
   },
   os::unix::net::UnixStream,
   path::Path,
   time::Duration,
};

use crate::{
   MAX_LINE,
   decode,
   encode,
   identity::Identity,
   key::{
      Key,
      check_token,
   },
   read_line,
};

const BATCH: usize = 256;

pub struct Client {
   reader: Option<BufReader<UnixStream>>,
}

impl Client {
   #[inline]
   pub fn connect(socket: &Path) -> io::Result<Self> {
      Ok(Self {
         reader: Some(BufReader::with_capacity(
            MAX_LINE,
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

   #[inline]
   pub fn get(&mut self, key: &Key) -> io::Result<Option<Vec<u8>>> {
      self.exchange(|reader| {
         writeln!(reader.get_mut(), "GET {key}")?;
         receive(reader)
      })
   }

   #[inline]
   pub fn get_many(&mut self, keys: &[Key]) -> io::Result<Vec<Option<Vec<u8>>>> {
      self.exchange(|reader| {
         let mut values = Vec::with_capacity(keys.len());
         for batch in keys.chunks(BATCH) {
            let mut request = Vec::new();
            for key in batch {
               writeln!(request, "GET {key}")?;
            }
            reader.get_mut().write_all(&request)?;
            for _ in batch {
               values.push(receive(reader)?);
            }
         }
         Ok(values)
      })
   }

   #[inline]
   pub fn has_many(&mut self, keys: &[Key]) -> io::Result<Vec<bool>> {
      self.exchange(|reader| {
         let mut present = Vec::with_capacity(keys.len());
         for batch in keys.chunks(BATCH) {
            let mut request = Vec::new();
            for key in batch {
               writeln!(request, "HAS {key}")?;
            }
            reader.get_mut().write_all(&request)?;
            for _ in batch {
               present.push(match line(reader)?.as_str() {
                  "1" => true,
                  "0" => false,
                  _ => return Err(io::Error::other("invalid cache presence response")),
               });
            }
         }
         Ok(present)
      })
   }

   #[inline]
   pub fn put(&mut self, key: &Key, value: &[u8]) -> io::Result<()> {
      let frame = encode(value)?;
      self.exchange(|reader| {
         writeln!(reader.get_mut(), "PUT {key} {}", frame.len())?;
         reader.get_mut().write_all(&frame)?;
         acknowledged(reader)
      })
   }

   /// Stores a value with the keys it refers to, which the cache keeps
   /// while the value lives and drops once nothing refers to them.
   #[inline]
   pub fn put_referring(&mut self, key: &Key, value: &[u8], references: &[Key]) -> io::Result<()> {
      let frame = encode(value)?;
      self.exchange(|reader| {
         let mut request = Vec::new();
         writeln!(request, "PUT {key} {} {}", frame.len(), references.len())?;
         for referenced in references {
            writeln!(request, "{referenced}")?;
         }
         request.extend_from_slice(&frame);
         reader.get_mut().write_all(&request)?;
         acknowledged(reader)
      })
   }

   /// Drops keys a newer value of the client's replaces, true for each the
   /// cache dropped, false for one it keeps and the client should still list.
   #[inline]
   pub fn forget(&mut self, keys: &[Key]) -> io::Result<Vec<bool>> {
      self.dropped("FORGET", keys)
   }

   /// Drops keys the client lists past its limit, each unless the cache saw
   /// it read recently, answered like `forget`.
   #[inline]
   pub fn expire(&mut self, keys: &[Key]) -> io::Result<Vec<bool>> {
      self.dropped("EXPIRE", keys)
   }

   fn dropped(&mut self, verb: &str, keys: &[Key]) -> io::Result<Vec<bool>> {
      self.exchange(|reader| {
         let mut dropped = Vec::with_capacity(keys.len());
         for batch in keys.chunks(BATCH) {
            let mut request = Vec::new();
            for key in batch {
               writeln!(request, "{verb} {key}")?;
            }
            reader.get_mut().write_all(&request)?;
            for _ in batch {
               dropped.push(match line(reader)?.as_str() {
                  "OK" => true,
                  "KEPT" => false,
                  _ => return Err(io::Error::other("invalid cache drop response")),
               });
            }
         }
         Ok(dropped)
      })
   }

   /// How long ago each key was last read, none for a missing one.
   #[inline]
   pub fn ages(&mut self, keys: &[Key]) -> io::Result<Vec<Option<Duration>>> {
      self.exchange(|reader| {
         let mut ages = Vec::with_capacity(keys.len());
         for batch in keys.chunks(BATCH) {
            let mut request = Vec::new();
            for key in batch {
               writeln!(request, "AGE {key}")?;
            }
            reader.get_mut().write_all(&request)?;
            for _ in batch {
               let answer = line(reader)?;
               ages.push(if answer == "MISS" {
                  None
               } else {
                  Some(Duration::from_nanos(
                     answer.parse::<u64>().map_err(io::Error::other)?,
                  ))
               });
            }
         }
         Ok(ages)
      })
   }

   #[inline]
   pub fn identities(&mut self, paths: &[String]) -> io::Result<Vec<Option<Identity>>> {
      for path in paths {
         if path.contains(['\n', '\r', '\0']) || path.len() >= MAX_LINE {
            return Err(io::Error::other("invalid cache identity path"));
         }
      }
      self.exchange(|reader| {
         let mut identities = Vec::with_capacity(paths.len());
         for batch in paths.chunks(BATCH) {
            let mut request = Vec::new();
            writeln!(request, "IDS {}", batch.len())?;
            for path in batch {
               writeln!(request, "{path}")?;
            }
            reader.get_mut().write_all(&request)?;
            for _ in batch {
               let answer = line(reader)?;
               identities.push(if answer.is_empty() {
                  None
               } else {
                  Some(answer.parse()?)
               });
            }
         }
         Ok(identities)
      })
   }

   #[inline]
   pub fn acquire(&mut self, build: &str) -> io::Result<()> {
      check_token(build)?;
      self.exchange(|reader| {
         writeln!(reader.get_mut(), "SLOT {build}")?;
         acknowledged(reader)
      })
   }

   #[inline]
   pub fn release(&mut self, build: &str) -> io::Result<()> {
      check_token(build)?;
      self.exchange(|reader| {
         writeln!(reader.get_mut(), "DONE {build}")?;
         acknowledged(reader)
      })
   }
}

fn line(reader: &mut impl BufRead) -> io::Result<String> {
   read_line(reader)?
      .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "cache closed the connection"))
}

fn acknowledged(reader: &mut impl BufRead) -> io::Result<()> {
   if line(reader)? != "OK" {
      return Err(io::Error::other("cache did not acknowledge request"));
   }
   Ok(())
}

fn receive(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
   let response = line(reader)?;
   if response == "MISS" {
      return Ok(None);
   }
   let length = response
      .strip_prefix("OK ")
      .ok_or_else(|| io::Error::other("invalid cache value response"))?
      .parse::<usize>()
      .map_err(io::Error::other)?;
   decode(reader, length).map(Some)
}
