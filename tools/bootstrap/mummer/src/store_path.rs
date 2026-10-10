use std::{
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
   str::FromStr,
};

use serde::{
   Deserialize,
   Serialize,
};

use crate::store::STORE_DIR;

pub const HASH_ALPHABET: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// A store object's path, `<store>/<hash>-<name>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct StorePath(String);

impl StorePath {
   #[inline]
   #[must_use]
   pub fn as_str(&self) -> &str {
      &self.0
   }

   /// The store object `path` lies in, if it lies in one.
   #[inline]
   #[must_use]
   pub fn containing(path: &str) -> Option<Self> {
      let object = path.strip_prefix(STORE_DIR)?.strip_prefix('/')?;
      let length = STORE_DIR.len() + 1 + object.find('/').unwrap_or(object.len());
      Self::try_from(path[..length].to_owned()).ok()
   }

   /// The 32 characters of its hash.
   #[inline]
   #[must_use]
   pub fn hash_part(&self) -> &str {
      &self.0[STORE_DIR.len() + 1..STORE_DIR.len() + 33]
   }

   /// The name after the hash.
   #[inline]
   #[must_use]
   pub fn name(&self) -> &str {
      &self.0[STORE_DIR.len() + 34..]
   }

   #[inline]
   #[must_use]
   pub fn join<Relative>(&self, relative: Relative) -> PathBuf
   where
      Relative: AsRef<Path>,
   {
      Path::new(&self.0).join(relative)
   }
}

impl TryFrom<String> for StorePath {
   type Error = io::Error;

   #[inline]
   fn try_from(text: String) -> io::Result<Self> {
      let valid = text
         .strip_prefix(STORE_DIR)
         .and_then(|rest| rest.strip_prefix('/'))
         .and_then(|object| object.split_at_checked(32))
         .is_some_and(|(hash, rest)| {
            hash.bytes().all(|byte| HASH_ALPHABET.contains(&byte))
               && rest
                  .strip_prefix('-')
                  .is_some_and(|name| !name.is_empty() && !name.contains('/'))
         });
      if !valid {
         return Err(io::Error::other(format!("{text} is not a store path")));
      }
      Ok(Self(text))
   }
}

impl FromStr for StorePath {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      Self::try_from(text.to_owned())
   }
}

impl From<StorePath> for String {
   #[inline]
   fn from(path: StorePath) -> Self {
      path.0
   }
}

impl AsRef<Path> for StorePath {
   #[inline]
   fn as_ref(&self) -> &Path {
      Path::new(&self.0)
   }
}

impl Display for StorePath {
   #[inline]
   #[expect(
      clippy::renamed_function_params,
      reason = "formatter describes the argument"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> FormatResult {
      formatter.write_str(&self.0)
   }
}

/// A derivation's store path, whose name ends in `.drv`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct DrvPath(StorePath);

impl DrvPath {
   #[inline]
   #[must_use]
   pub fn as_str(&self) -> &str {
      self.0.as_str()
   }
}

impl TryFrom<StorePath> for DrvPath {
   type Error = io::Error;

   #[inline]
   fn try_from(path: StorePath) -> io::Result<Self> {
      if Path::new(path.name())
         .extension()
         .is_none_or(|extension| extension != "drv")
      {
         return Err(io::Error::other(format!("{path} is not a derivation")));
      }
      Ok(Self(path))
   }
}

impl TryFrom<String> for DrvPath {
   type Error = io::Error;

   #[inline]
   fn try_from(text: String) -> io::Result<Self> {
      StorePath::try_from(text)?.try_into()
   }
}

impl FromStr for DrvPath {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      Self::try_from(text.to_owned())
   }
}

impl From<DrvPath> for String {
   #[inline]
   fn from(path: DrvPath) -> Self {
      path.0.0
   }
}

impl From<DrvPath> for StorePath {
   #[inline]
   fn from(path: DrvPath) -> Self {
      path.0
   }
}

impl Display for DrvPath {
   #[inline]
   #[expect(
      clippy::renamed_function_params,
      reason = "formatter describes the argument"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> FormatResult {
      Display::fmt(&self.0, formatter)
   }
}
