use std::{
   io,
   str::FromStr,
};

use crate::{
   MAX_OBJECT,
   check_key,
};

pub enum Request {
   Get(String),
   Has(String),
   Put { key: String, length: usize },
   Identities(usize),
   Acquire(String),
   Release(String),
   Stats,
}

impl FromStr for Request {
   type Err = io::Error;

   #[inline]
   fn from_str(line: &str) -> io::Result<Self> {
      let fields = line.split_whitespace().collect::<Vec<&str>>();
      match *fields.as_slice() {
         ["GET", key] => Ok(Self::Get((*key).to_owned())),
         ["HAS", key] => Ok(Self::Has((*key).to_owned())),
         ["PUT", key, size] => {
            check_key(key)?;
            let length = size.parse::<usize>().map_err(io::Error::other)?;
            if length > MAX_OBJECT {
               return Err(io::Error::other("cache object exceeds size limit"));
            }
            Ok(Self::Put {
               key: (*key).to_owned(),
               length,
            })
         },
         ["IDS", count] => {
            let length = count.parse::<usize>().map_err(io::Error::other)?;
            if length > 1 << 20 {
               return Err(io::Error::other("identity request exceeds size limit"));
            }
            Ok(Self::Identities(length))
         },
         ["SLOT", build] => Ok(Self::Acquire((*build).to_owned())),
         ["DONE", build] => Ok(Self::Release((*build).to_owned())),
         ["STATS"] => Ok(Self::Stats),
         _ => Err(io::Error::other("invalid cache request")),
      }
   }
}
