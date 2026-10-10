use std::{
   array::from_fn,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   io,
   str::FromStr,
};

use serde::{
   Deserialize,
   Serialize,
};

use crate::key::Hex;

/// The first 128 bits of a BLAKE3 hash, written as 32 hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest([u8; 16]);

impl Digest {
   #[inline]
   #[must_use]
   pub fn of(bytes: &[u8]) -> Self {
      Self::from(blake3::hash(bytes))
   }

   #[inline]
   #[must_use]
   pub const fn as_bytes(&self) -> &[u8; 16] {
      &self.0
   }
}

impl From<blake3::Hash> for Digest {
   #[inline]
   fn from(hash: blake3::Hash) -> Self {
      let bytes = hash.as_bytes();
      Self(from_fn(|index| bytes[index]))
   }
}

impl Display for Digest {
   #[inline]
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      Hex(&self.0).fmt(f)
   }
}

impl FromStr for Digest {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      let invalid = || io::Error::other(format!("invalid digest {text:?}"));
      let (pairs, rest) = text.as_bytes().as_chunks::<2>();
      if pairs.len() != 16 || !rest.is_empty() {
         return Err(invalid());
      }
      let nibble = |digit: u8| char::from(digit).to_digit(16).ok_or_else(invalid);
      let mut bytes = [0_u8; 16];
      for (byte, &[high, low]) in bytes.iter_mut().zip(pairs) {
         *byte =
            u8::try_from((nibble(high)? << 4_u32) | nibble(low)?).map_err(|_error| invalid())?;
      }
      Ok(Self(bytes))
   }
}

impl TryFrom<String> for Digest {
   type Error = io::Error;

   #[inline]
   fn try_from(text: String) -> io::Result<Self> {
      text.parse()
   }
}

impl From<Digest> for String {
   #[inline]
   fn from(digest: Digest) -> Self {
      digest.to_string()
   }
}
