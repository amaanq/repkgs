use std::{
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   io,
   str::FromStr,
};

use crate::{
   MAX_KEY,
   digest::Digest,
};

/// What a key's value is, written as the key's leading path.
#[derive(Clone, Copy)]
pub enum Namespace {
   Manifest,
   /// A compile's result, its status, diagnostics, dependency files and
   /// outputs.
   Record,
   Blob,
   Cc,
   Rust,
   Haskell,
   HeaderProbe,
   GoAction,
   GoOutput,
   /// A file's identity by its stat stamp, which only this machine can use.
   Stamp,
   Loaded,
}

impl Namespace {
   const fn prefix(self) -> &'static str {
      match self {
         Self::Manifest => "m/",
         Self::Record => "rec/",
         Self::Blob => "b/",
         Self::Cc => "cc/",
         Self::Rust => "rs/",
         Self::Haskell => "hs-module/",
         Self::HeaderProbe => "probe/header/",
         Self::GoAction => "go/a/",
         Self::GoOutput => "go/o/",
         Self::Stamp => "i/",
         Self::Loaded => "i/loaded/",
      }
   }
}

/// A cache key, short, printable and free of whitespace.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(String);

impl Key {
   #[must_use]
   #[inline]
   pub fn hex(namespace: Namespace, id: &[u8]) -> Self {
      Self(format!("{}{}", namespace.prefix(), Hex(id)))
   }

   #[must_use]
   #[inline]
   pub fn digest(namespace: Namespace, digest: &Digest) -> Self {
      Self::hex(namespace, digest.as_bytes())
   }

   #[must_use]
   #[inline]
   pub fn of(namespace: Namespace, inner: &Self) -> Self {
      Self(format!("{}{inner}", namespace.prefix()))
   }

   #[must_use]
   #[inline]
   pub fn is_local(&self) -> bool {
      self.0.starts_with(Namespace::Stamp.prefix())
   }

   #[must_use]
   #[inline]
   pub fn as_str(&self) -> &str {
      &self.0
   }
}

impl Display for Key {
   #[inline]
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      f.write_str(&self.0)
   }
}

impl FromStr for Key {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      check_token(text)?;
      Ok(Self(text.to_owned()))
   }
}

impl From<Key> for String {
   #[inline]
   fn from(key: Key) -> Self {
      key.0
   }
}

/// Bytes as lowercase hex digits.
pub struct Hex<'bytes>(pub &'bytes [u8]);

impl Display for Hex<'_> {
   #[inline]
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
   }
}

#[inline]
pub fn check_token(text: &str) -> io::Result<()> {
   if text.is_empty()
      || text.len() > MAX_KEY
      || text
         .bytes()
         .any(|byte| byte.is_ascii_whitespace() || byte == 0)
   {
      return Err(io::Error::other("invalid cache key"));
   }
   Ok(())
}
