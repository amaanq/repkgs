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

use crate::digest::Digest;

/// A file as its content, `C:` and its digest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Identity(Digest);

impl Identity {
   #[inline]
   #[must_use]
   pub fn of(bytes: &[u8]) -> Self {
      Self(Digest::of(bytes))
   }

   #[inline]
   pub fn of_reader<Reader>(reader: Reader) -> io::Result<Self>
   where
      Reader: Read,
   {
      let mut hasher = Hasher::new();
      hasher.update_reader(reader)?;
      Ok(Self(Digest::from(hasher.finalize())))
   }
}

impl Display for Identity {
   #[inline]
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      write!(f, "C:{}", self.0)
   }
}

impl FromStr for Identity {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      let digest = text
         .strip_prefix("C:")
         .ok_or_else(|| io::Error::other("invalid cache identity"))?;
      Ok(Self(digest.parse()?))
   }
}
