use std::{
   io,
   str::FromStr,
};

use crate::{
   MAX_OBJECT,
   key::{
      Key,
      check_token,
   },
};

pub enum Request {
   Get(Key),
   Has(Key),
   /// A value, after `references` lines naming the keys it refers to when
   /// the client sends them.
   Put {
      key:        Key,
      length:     usize,
      references: Option<usize>,
   },
   /// A key the client's newer value replaces, answered `OK` when dropped
   /// and `KEPT` when the client should keep listing it.
   Forget(Key),
   /// A key the client lists past its limit, dropped unless read recently,
   /// answered like a forget.
   Expire(Key),
   /// How long ago a key was last read.
   Age(Key),
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
         ["GET", key] => Ok(Self::Get(key.parse()?)),
         ["HAS", key] => Ok(Self::Has(key.parse()?)),
         ["PUT", key, size] => put(key, size, None),
         ["PUT", key, size, count] => {
            let references = count.parse::<usize>().map_err(io::Error::other)?;
            if references > 1 << 20 {
               return Err(io::Error::other("reference list exceeds size limit"));
            }
            put(key, size, Some(references))
         },
         ["FORGET", key] => Ok(Self::Forget(key.parse()?)),
         ["EXPIRE", key] => Ok(Self::Expire(key.parse()?)),
         ["AGE", key] => Ok(Self::Age(key.parse()?)),
         ["IDS", count] => {
            let length = count.parse::<usize>().map_err(io::Error::other)?;
            if length > 1 << 20 {
               return Err(io::Error::other("identity request exceeds size limit"));
            }
            Ok(Self::Identities(length))
         },
         ["SLOT", build] => {
            check_token(build)?;
            Ok(Self::Acquire((*build).to_owned()))
         },
         ["DONE", build] => {
            check_token(build)?;
            Ok(Self::Release((*build).to_owned()))
         },
         ["STATS"] => Ok(Self::Stats),
         _ => Err(io::Error::other("invalid cache request")),
      }
   }
}

fn put(key: &str, size: &str, references: Option<usize>) -> io::Result<Request> {
   let length = size.parse::<usize>().map_err(io::Error::other)?;
   if length > MAX_OBJECT {
      return Err(io::Error::other("cache object exceeds size limit"));
   }
   Ok(Request::Put {
      key: key.parse()?,
      length,
      references,
   })
}
