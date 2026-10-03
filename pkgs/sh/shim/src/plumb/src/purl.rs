//! Package-URL, <https://github.com/package-url/purl-spec>.
//!
//! `pkg:type/namespace/name@version?k=v&k=v#subpath`, used as identities only,
//! so version and subpath are accepted and dropped.

use std::{
   collections::BTreeMap,
   io,
   str::FromStr,
};

use percent_encoding::percent_decode_str;

#[derive(Clone)]
pub struct Purl {
   pub kind:       String,
   pub namespace:  String,
   pub name:       String,
   pub qualifiers: BTreeMap<String, String>,
}

fn decoded(text: &str) -> String {
   percent_decode_str(text).decode_utf8_lossy().into_owned()
}

impl FromStr for Purl {
   type Err = io::Error;

   fn from_str(text: &str) -> io::Result<Self> {
      let bad = || io::Error::other(format!("not a purl: {text}"));
      let trimmed = text.trim();
      let Some(rest) = trimmed
         .get(..4)
         .filter(|head| head.eq_ignore_ascii_case("pkg:"))
         .and_then(|_| trimmed.get(4..))
      else {
         return Err(bad());
      };
      let body = rest.trim_start_matches('/');
      let (kind, tail) = body.split_once('/').ok_or_else(bad)?;
      let (before_fragment, _) = tail.split_once('#').unwrap_or((tail, ""));
      let (path, query) = before_fragment
         .split_once('?')
         .unwrap_or((before_fragment, ""));
      let segments: Vec<String> = path
         .split('/')
         .filter(|segment| !segment.is_empty())
         .map(decoded)
         .collect();
      let Some((last, namespace)) = segments.split_last() else {
         return Err(bad());
      };
      let name = last.split_once('@').map_or(last.as_str(), |(name, _)| name);
      let qualifiers = query
         .split('&')
         .filter_map(|pair| pair.split_once('='))
         .map(|(key, value)| (key.to_owned(), decoded(value)))
         .collect();
      Ok(Self {
         kind: kind.to_owned(),
         namespace: namespace.join("/"),
         name: name.to_owned(),
         qualifiers,
      })
   }
}
