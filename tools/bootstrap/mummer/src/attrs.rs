use std::{
   env::var_os,
   fs::read,
   io,
};

use serde::de::DeserializeOwned;

/// The derivation's structured attrs, as `__structuredAttrs` writes them.
#[inline]
pub fn load<Attrs>() -> io::Result<Attrs>
where
   Attrs: DeserializeOwned,
{
   let path = var_os("NIX_ATTRS_JSON_FILE")
      .ok_or_else(|| io::Error::other("NIX_ATTRS_JSON_FILE is not set"))?;
   Ok(serde_json::from_slice(&read(path)?)?)
}
