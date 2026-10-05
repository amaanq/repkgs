//! act.toml's `tag`, how upstream spells a release: fixed text around one
//! `{version}` or `{version_}` slot, and a trailing `*` for whatever upstream
//! appends, or a bare `*` for tags that do not spell the version at all. The
//! slot only matches integers joined by `.`, `-`, `_` or `+`, so `v{version}`
//! passes over `v2.1-rc1` and `v2.0.3-purple`.

use std::{
   io,
   str::FromStr,
};

// `+` for build numbers, jdk-25.0.4.1+1
const SEPARATORS: [char; 4] = ['.', '-', '_', '+'];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
   slot:        bool,
   prefix:      String,
   underscored: bool,
   suffix:      String,
   open:        bool,
}

impl FromStr for Template {
   type Err = io::Error;

   fn from_str(raw: &str) -> io::Result<Self> {
      if raw == "*" {
         return Ok(Self {
            slot:        false,
            prefix:      String::new(),
            underscored: false,
            suffix:      String::new(),
            open:        true,
         });
      }
      let (slot, underscored) = if raw.contains("{version_}") {
         ("{version_}", true)
      } else {
         ("{version}", false)
      };
      let Some((prefix, rest)) = raw.split_once(slot) else {
         return Err(io::Error::other(format!("tag {raw} needs a {slot} slot")));
      };
      if rest.contains('{') {
         return Err(io::Error::other(format!(
            "tag {raw} has more than one slot"
         )));
      }
      let (suffix, open) = rest
         .strip_suffix('*')
         .map_or((rest, false), |fixed| (fixed, true));
      if suffix.contains('*') || prefix.contains('*') {
         return Err(io::Error::other(format!(
            "tag {raw} may end in *, nowhere else"
         )));
      }
      Ok(Self {
         slot: true,
         prefix: prefix.to_owned(),
         underscored,
         suffix: suffix.to_owned(),
         open,
      })
   }
}

impl Template {
   /// The tag for `version`, None when a trailing `*` leaves it to upstream.
   pub fn spell(&self, version: &str) -> Option<String> {
      if self.open {
         return None;
      }
      let slot = if self.underscored {
         version.replace('.', "_")
      } else {
         version.to_owned()
      };
      Some(format!("{}{slot}{}", self.prefix, self.suffix))
   }

   /// Whether upstream's `tag` is a release this template spells.
   pub fn matches(&self, tag: &str) -> bool {
      !self.slot || self.slot_of(tag).is_some()
   }

   /// The version `tag` carries in the slot, `.` for `_` in a `{version_}`.
   pub fn version_in(&self, tag: &str) -> Option<String> {
      let slot = self.slot_of(tag)?;
      Some(if self.underscored {
         slot.replace('_', ".")
      } else {
         slot.to_owned()
      })
   }

   fn slot_of<'tag>(&self, tag: &'tag str) -> Option<&'tag str> {
      if !self.slot {
         return None;
      }
      let rest = tag.strip_prefix(self.prefix.as_str())?;
      let run = rest
         .find(|ch: char| !(ch.is_ascii_digit() || SEPARATORS.contains(&ch)))
         .unwrap_or(rest.len());
      (1..=run).rev().find_map(|length| {
         let (slot, after) = rest.split_at(length);
         let fits = numeric(slot)
            && after
               .strip_prefix(self.suffix.as_str())
               .is_some_and(|tail| self.open || tail.is_empty());
         fits.then_some(slot)
      })
   }
}

fn numeric(slot: &str) -> bool {
   slot
      .split(SEPARATORS)
      .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}
