use std::{
   fmt::{
      self,
      Display,
      Formatter,
   },
   io,
   str::FromStr,
};

use serde::Deserialize;

#[derive(Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(try_from = "String")]
pub struct PackageId {
   name:    String,
   version: String,
}

impl PackageId {
   #[inline]
   pub fn new(name: &str, version: &str) -> io::Result<Self> {
      let valid_name = name.split('-').all(|part| {
         !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
            && part.bytes().any(|byte| byte.is_ascii_alphabetic())
      });
      let valid_version = version
         .split('.')
         .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));

      if !valid_name || !valid_version {
         return Err(io::Error::other(format!(
            "invalid Hackage package {name}-{version}"
         )));
      }

      Ok(Self {
         name:    name.to_owned(),
         version: version.to_owned(),
      })
   }

   #[inline]
   #[must_use]
   pub fn name(&self) -> &str {
      &self.name
   }

   #[inline]
   #[must_use]
   pub fn version(&self) -> &str {
      &self.version
   }
}

impl Display for PackageId {
   #[inline]
   #[expect(
      clippy::renamed_function_params,
      reason = "formatter describes the argument"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
      write!(formatter, "{}-{}", self.name, self.version)
   }
}

impl FromStr for PackageId {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      let (name, version) = text
         .rsplit_once('-')
         .ok_or_else(|| io::Error::other(format!("invalid Hackage package {text}")))?;
      Self::new(name, version)
   }
}

impl TryFrom<String> for PackageId {
   type Error = io::Error;

   #[inline]
   fn try_from(text: String) -> io::Result<Self> {
      text.parse()
   }
}
