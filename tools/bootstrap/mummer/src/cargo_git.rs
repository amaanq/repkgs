//! The git sources a Cargo.lock pins, `git+<url>[?<ref>]#<commit>`.
//!
//! - spieler pins each one as a forge archive in act.lock's `[sources]`, keyed
//!   by the lock's own source string.
//! - `mummer fetch cargo` vendors their crates from the unpacked archives.
//! - bigtop points cargo's git source at the vendored copy, which needs the
//!   reference the manifest named, not only the commit.

use std::{
   collections::BTreeSet,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   io,
   str::FromStr,
};

use serde::Deserialize;

/// The file beside the vendored crates naming cargo's `[source]` tables for
/// the git sources they replace. cargo skips a directory source's dot entries.
pub const GIT_SOURCES: &str = ".git-sources.toml";

/// What the manifest asked for, the lock's query.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reference {
   Default,
   Branch(String),
   Tag(String),
   Rev(String),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitSource {
   pub url:       String,
   pub reference: Reference,
   pub commit:    String,
}

impl FromStr for GitSource {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      let invalid = || io::Error::other(format!("{text} is not a cargo git source"));
      let rest = text.strip_prefix("git+").ok_or_else(invalid)?;
      let (located, commit) = rest.rsplit_once('#').ok_or_else(invalid)?;
      let (url, query) = located.split_once('?').unwrap_or((located, ""));
      let reference = match query.split_once('=') {
         None if query.is_empty() => Reference::Default,
         Some(("branch", name)) => Reference::Branch(name.to_owned()),
         Some(("tag", name)) => Reference::Tag(name.to_owned()),
         Some(("rev", name)) => Reference::Rev(name.to_owned()),
         _ => return Err(invalid()),
      };
      if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
         return Err(invalid());
      }
      Ok(Self {
         url: url.to_owned(),
         reference,
         commit: commit.to_owned(),
      })
   }
}

impl Display for GitSource {
   #[inline]
   #[expect(
      clippy::renamed_function_params,
      reason = "formatter describes the argument"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> FormatResult {
      write!(formatter, "git+{}", self.url)?;
      match self.reference {
         Reference::Default => {},
         Reference::Branch(ref name) => write!(formatter, "?branch={name}")?,
         Reference::Tag(ref name) => write!(formatter, "?tag={name}")?,
         Reference::Rev(ref name) => write!(formatter, "?rev={name}")?,
      }
      write!(formatter, "#{}", self.commit)
   }
}

impl GitSource {
   /// The forge's tarball of the commit, its tree under one top directory.
   #[inline]
   pub fn archive_url(&self) -> io::Result<String> {
      let unsupported = || {
         io::Error::other(format!(
            "{self}: no archive url for this forge, only GitHub, GitLab, Codeberg and SourceHut"
         ))
      };
      let bare = self.url.trim_end_matches('/').trim_end_matches(".git");
      let path = bare.strip_prefix("https://").ok_or_else(unsupported)?;
      let (host, repo) = path.split_once('/').ok_or_else(unsupported)?;
      let name = repo.rsplit('/').next().ok_or_else(unsupported)?;
      let commit = &self.commit;
      match host {
         "github.com" | "codeberg.org" | "git.sr.ht" => {
            Ok(format!("https://{host}/{repo}/archive/{commit}.tar.gz"))
         },
         _ if host == "gitlab.com" || host.starts_with("gitlab.") => {
            Ok(format!(
               "https://{host}/{repo}/-/archive/{commit}/{name}-{commit}.tar.gz"
            ))
         },
         _ => Err(unsupported()),
      }
   }

   /// The store name of the unpacked archive.
   #[inline]
   #[must_use]
   pub fn name(&self) -> String {
      let bare = self.url.trim_end_matches('/').trim_end_matches(".git");
      let repo = bare.rsplit('/').next().unwrap_or("git");
      format!("{repo}-{}", &self.commit[..12])
   }

   /// cargo's `[source.<name>]` table for this source, replaced by `with`.
   #[inline]
   #[must_use]
   pub fn cargo_config(&self, with: &str) -> String {
      let quoted = |text: &str| toml::Value::String(text.to_owned()).to_string();
      let reference = match self.reference {
         Reference::Default => String::new(),
         Reference::Branch(ref name) => format!("branch = {}\n", quoted(name)),
         Reference::Tag(ref name) => format!("tag = {}\n", quoted(name)),
         Reference::Rev(ref name) => format!("rev = {}\n", quoted(name)),
      };
      format!(
         "[source.{}]\ngit = {}\n{reference}replace-with = {}\n",
         quoted(&self.to_string()),
         quoted(&self.url),
         quoted(with)
      )
   }
}

#[derive(Deserialize)]
struct Lock {
   #[serde(default)]
   package: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
   source: Option<String>,
}

/// The git sources of a Cargo.lock's text.
#[inline]
pub fn locked(text: &str) -> io::Result<BTreeSet<GitSource>> {
   toml::from_str::<Lock>(text)
      .map_err(io::Error::other)?
      .package
      .into_iter()
      .filter_map(|package| package.source)
      .filter(|origin| origin.starts_with("git+"))
      .map(|origin| origin.parse())
      .collect()
}
