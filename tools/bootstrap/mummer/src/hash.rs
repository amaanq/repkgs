use std::{
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
      Write as _,
   },
   io,
   str::FromStr,
};

use base64::{
   Engine as _,
   engine::general_purpose::STANDARD,
};
use serde::Deserialize;
use sha2::{
   Digest as _,
   Sha256,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
   Sha1,
   Sha256,
   Sha512,
}

impl Algorithm {
   const fn size(self) -> usize {
      match self {
         Self::Sha1 => 20,
         Self::Sha256 => 32,
         Self::Sha512 => 64,
      }
   }
}

impl Display for Algorithm {
   #[inline]
   #[expect(
      clippy::renamed_function_params,
      reason = "formatter describes the argument"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> FormatResult {
      formatter.write_str(match *self {
         Self::Sha1 => "sha1",
         Self::Sha256 => "sha256",
         Self::Sha512 => "sha512",
      })
   }
}

impl FromStr for Algorithm {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      match text {
         "sha1" => Ok(Self::Sha1),
         "sha256" => Ok(Self::Sha256),
         "sha512" => Ok(Self::Sha512),
         _ => Err(io::Error::other(format!("unknown hash algorithm {text}"))),
      }
   }
}

/// A digest of a known algorithm and length. `Display` writes its SRI form.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Digest {
   algorithm: Algorithm,
   bytes:     Vec<u8>,
}

impl Digest {
   #[inline]
   pub fn new(algorithm: Algorithm, bytes: Vec<u8>) -> io::Result<Self> {
      if bytes.len() != algorithm.size() {
         return Err(io::Error::other(format!(
            "a {algorithm} digest has {} bytes, not {}",
            algorithm.size(),
            bytes.len()
         )));
      }
      Ok(Self { algorithm, bytes })
   }

   #[inline]
   #[must_use]
   pub fn sha256(contents: &[u8]) -> Self {
      Self {
         algorithm: Algorithm::Sha256,
         bytes:     Sha256::digest(contents).to_vec(),
      }
   }

   #[inline]
   pub fn from_hex(algorithm: Algorithm, text: &str) -> io::Result<Self> {
      let invalid = || io::Error::other(format!("{text} is not a hex {algorithm} digest"));
      if !text.len().is_multiple_of(2) {
         return Err(invalid());
      }
      let bytes = text
         .as_bytes()
         .chunks(2)
         .map(|pair| {
            str::from_utf8(pair)
               .ok()
               .and_then(|digits| u8::from_str_radix(digits, 16).ok())
               .ok_or_else(invalid)
         })
         .collect::<io::Result<Vec<_>>>()?;
      Self::new(algorithm, bytes).map_err(|_length| invalid())
   }

   #[inline]
   #[must_use]
   pub const fn algorithm(&self) -> Algorithm {
      self.algorithm
   }

   #[inline]
   #[must_use]
   pub fn hex(&self) -> String {
      self
         .bytes
         .iter()
         .try_fold(String::new(), |mut text, byte| {
            write!(text, "{byte:02x}").map(|()| text)
         })
         .unwrap_or_default()
   }
}

impl Display for Digest {
   #[inline]
   #[expect(
      clippy::renamed_function_params,
      reason = "formatter describes the argument"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> FormatResult {
      write!(
         formatter,
         "{}-{}",
         self.algorithm,
         STANDARD.encode(&self.bytes)
      )
   }
}

/// SRI, `sha256-<base64>`, or `sha256:<hex>` as uv writes it.
impl FromStr for Digest {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      if let Some((algorithm, hex)) = text.split_once(':') {
         return Self::from_hex(algorithm.parse()?, hex);
      }
      let (algorithm, encoded) = text
         .split_once('-')
         .ok_or_else(|| io::Error::other(format!("{text} is not an SRI hash")))?;
      let bytes = STANDARD
         .decode(encoded)
         .map_err(|error| io::Error::other(format!("{text}, {error}")))?;
      Self::new(algorithm.parse()?, bytes)
   }
}

/// How a lock spelled a hash, which a fetchurl derivation repeats as its
/// `outputHash`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spelling {
   Sri,
   LowerHex,
   UpperHex,
}

impl TryFrom<String> for Digest {
   type Error = io::Error;

   #[inline]
   fn try_from(text: String) -> io::Result<Self> {
      text.parse()
   }
}

/// A fixed output's hash as the lock spelled it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputHash {
   digest:   Digest,
   spelling: Spelling,
}

impl OutputHash {
   #[inline]
   #[must_use]
   pub const fn sri(digest: Digest) -> Self {
      Self {
         digest,
         spelling: Spelling::Sri,
      }
   }

   /// The first of an integrity field's space-separated SRI hashes.
   #[inline]
   pub fn integrity(text: &str) -> io::Result<Self> {
      let first = text
         .split_whitespace()
         .next()
         .ok_or_else(|| io::Error::other("empty integrity"))?;
      Ok(Self::sri(first.parse()?))
   }

   #[inline]
   pub fn hex(algorithm: Algorithm, text: &str) -> io::Result<Self> {
      Ok(Self {
         digest:   Digest::from_hex(algorithm, text)?,
         spelling: if text.bytes().any(|byte| byte.is_ascii_uppercase()) {
            Spelling::UpperHex
         } else {
            Spelling::LowerHex
         },
      })
   }

   #[inline]
   #[must_use]
   pub const fn digest(&self) -> &Digest {
      &self.digest
   }

   #[inline]
   #[must_use]
   pub const fn spelling(&self) -> Spelling {
      self.spelling
   }
}

impl Display for OutputHash {
   #[inline]
   #[expect(
      clippy::renamed_function_params,
      reason = "formatter describes the argument"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> FormatResult {
      match self.spelling {
         Spelling::Sri => write!(formatter, "{}", self.digest),
         Spelling::LowerHex => formatter.write_str(&self.digest.hex()),
         Spelling::UpperHex => formatter.write_str(&self.digest.hex().to_ascii_uppercase()),
      }
   }
}
