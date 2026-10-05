//! npm registry naming shared by the pnpm, bun and deno producers, npm's own
//! lock carries full URLs.

use std::io;

pub const REGISTRY: &str = "https://registry.npmjs.org";

/// "@scope/name@1.2.3" or "name@1.2.3", the version starts at the last "@"
/// past position 0.
pub struct PackageId<'id> {
   pub name:    &'id str,
   pub version: &'id str,
}

impl<'id> TryFrom<&'id str> for PackageId<'id> {
   type Error = io::Error;

   fn try_from(id: &'id str) -> io::Result<Self> {
      let at = id
         .get(1..)
         .and_then(|rest| rest.rfind('@'))
         .map(|found| found + 1)
         .filter(|&found| found + 1 < id.len())
         .ok_or_else(|| io::Error::other(format!("package {id} names no version")))?;
      Ok(Self {
         name:    &id[..at],
         version: &id[at + 1..],
      })
   }
}

impl PackageId<'_> {
   /// The registry's canonical tarball location, scoped packages drop the
   /// scope in the file name.
   pub fn tarball_url(&self, registry: Option<&str>) -> String {
      let base = registry.map_or(REGISTRY, |found| found.trim_end_matches('/'));
      format!(
         "{base}/{}/-/{}-{}.tgz",
         self.name,
         basename(self.name),
         self.version
      )
   }

   /// "@scope/name" as one path component.
   pub fn flat_name(&self) -> String {
      self.name.replace('/', "+")
   }
}

/// A URL's scheme, authority, and path with its query.
pub struct Url<'url> {
   pub scheme:    &'url str,
   pub authority: &'url str,
   pub path:      &'url str,
}

impl<'url> TryFrom<&'url str> for Url<'url> {
   type Error = io::Error;

   fn try_from(url: &'url str) -> io::Result<Self> {
      let (scheme, rest) = url
         .split_once("://")
         .ok_or_else(|| io::Error::other(format!("{url} has no scheme")))?;
      let (authority, path) = rest
         .find('/')
         .map_or((rest, ""), |slash| rest.split_at(slash));
      Ok(Self {
         scheme,
         authority,
         path,
      })
   }
}

/// The path of a URL, everything from the first "/" after the host.
pub fn url_path(url: &str) -> &str {
   let rest = url.split_once("://").map_or(url, |(_scheme, rest)| rest);
   rest.find('/').map_or("", |slash| &rest[slash..])
}

pub fn basename(path: &str) -> &str {
   path.rsplit('/').next().unwrap_or(path)
}
