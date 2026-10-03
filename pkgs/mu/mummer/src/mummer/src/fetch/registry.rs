//! npm registry naming shared by the pnpm, bun and deno producers, npm's own
//! lock carries full URLs.

pub const REGISTRY: &str = "https://registry.npmjs.org";

/// "@scope/name@1.2.3" or "name@1.2.3", the version starts at the last "@"
/// past position 0.
pub fn split_id(id: &str) -> (&str, &str) {
   id[1..]
      .rfind('@')
      .map_or((id, ""), |at| (&id[..=at], &id[at + 2..]))
}

/// the registry's canonical tarball location, scoped packages drop the scope
/// in the file name.
pub fn tarball_url(name: &str, version: &str, registry: &str) -> String {
   let base = if registry.is_empty() {
      REGISTRY
   } else {
      registry.trim_end_matches('/')
   };
   let short = name.rsplit('/').next().unwrap_or(name);
   format!("{base}/{name}/-/{short}-{version}.tgz")
}

/// "@scope/name" as one path component.
pub fn flat_name(name: &str) -> String {
   name.replace('/', "+")
}

/// the path of a URL, everything from the first "/" after the host.
pub fn url_path(url: &str) -> &str {
   let rest = url.split_once("://").map_or(url, |(_scheme, rest)| rest);
   rest.find('/').map_or("", |slash| &rest[slash..])
}

pub fn basename(path: &str) -> &str {
   path.rsplit('/').next().unwrap_or(path)
}
