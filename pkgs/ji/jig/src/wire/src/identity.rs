use std::{
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   io::{
      self,
      Read,
   },
   str::FromStr,
};

use blake3::Hasher;

use crate::digest;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Identity(String);

impl Identity {
   #[must_use]
   #[inline]
   pub fn of(bytes: &[u8]) -> Self {
      Self(format!("C:{}", digest(bytes)))
   }

   #[inline]
   pub fn of_reader<Reader>(reader: Reader) -> io::Result<Self>
   where
      Reader: Read,
   {
      let mut hasher = Hasher::new();
      hasher.update_reader(reader)?;
      Ok(Self(format!("C:{}", &hasher.finalize().to_hex()[..32])))
   }
}

impl Display for Identity {
   #[inline]
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      f.write_str(&self.0)
   }
}

impl FromStr for Identity {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      if text.len() != 34
         || !text.starts_with("C:")
         || !text.as_bytes()[2..].iter().all(u8::is_ascii_hexdigit)
      {
         return Err(io::Error::other("invalid cache identity"));
      }
      Ok(Self(text.to_owned()))
   }
}
