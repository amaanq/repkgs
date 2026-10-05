//! Version normalisation and ordering.
//!
//! One comparator covers semver, pep440 and distro-style "loose": numeric
//! runs compare as numbers, pre-release words sort before the release, other
//! words after (1.0rc1 < 1.0 == 1.0.0 < 1.0.post1 < 1.0.1).

use std::cmp::Ordering;

const PRE: [&str; 14] = [
   "dev", "pre", "preview", "alpha", "a", "beta", "b", "rc", "c", "DEV", "PRE", "ALPHA", "BETA",
   "RC",
];

/// A component with its class, the derived order is the class order.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
   Pre(usize),
   Absent,
   Word(String),
   Number(u64),
}

/// tag or file name -> bare version: v1.2, release-1.2, jq-1.8.2, curl-8_1_0,
/// `R_2_7_2`, llvmorg-21.1.8.
pub fn from_tag(tag: &str) -> String {
   let stripped = strip_name(tag);
   let bare = strip_letters(stripped);
   if underscored(bare) {
      bare.replace('_', ".")
   } else if dashed(bare) {
      bare.replace('-', ".")
   } else {
      bare.to_owned()
   }
}

fn starts_with_version(text: &str) -> bool {
   let digits = text.trim_start_matches(['v', 'V']);
   digits.len() + 1 >= text.len() && digits.starts_with(|ch: char| ch.is_ascii_digit())
}

/// `name-` prefixes, as many as leave a version behind (names may contain
/// digits: pcre2-10.44, bun-v1.4.2).
fn strip_name(tag: &str) -> &str {
   let mut cuts = Vec::new();
   let mut cursor = 0;
   loop {
      let rest = &tag[cursor..];
      let Some(first) = rest.chars().next().filter(char::is_ascii_alphabetic) else {
         break;
      };
      let word = rest
         .char_indices()
         .skip(1)
         .find(|&(_, ch)| !ch.is_ascii_alphanumeric())
         .map_or(rest.len(), |(index, _)| index);
      if !rest[word..].starts_with('-') || first == '-' {
         break;
      }
      cursor += word + 1;
      cuts.push(cursor);
   }
   cuts
      .iter()
      .rev()
      .copied()
      .find(|&cut| starts_with_version(&tag[cut..]))
      .map_or(tag, |cut| &tag[cut..])
}

/// A letters-only prefix, with one `.` or `_` after it, before the digits
/// (v4.0.6, go1.25, `R_2_7_2`).
fn strip_letters(text: &str) -> &str {
   let letters = text
      .find(|ch: char| !ch.is_ascii_alphabetic())
      .unwrap_or(text.len());
   if letters == 0 {
      return text;
   }
   let after = &text[letters..];
   let separated = after.strip_prefix(['.', '_']).unwrap_or(after);
   if separated.starts_with(|ch: char| ch.is_ascii_digit()) {
      separated
   } else {
      text
   }
}

fn underscored(text: &str) -> bool {
   let mut parts = text.splitn(3, '_');
   let first = parts.next().unwrap_or_default();
   let second = parts.next().unwrap_or_default();
   !first.is_empty()
      && first.bytes().all(|byte| byte.is_ascii_digit())
      && second.starts_with(|ch: char| ch.is_ascii_digit())
}

fn dashed(text: &str) -> bool {
   let parts = text.split('-').collect::<Vec<&str>>();
   parts.len() > 1
      && parts
         .iter()
         .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Components with a class, build metadata dropped.
fn ranks(version: &str) -> Vec<Rank> {
   let mut text = version.split('+').next().unwrap_or_default();
   if let Some(rest) = text.strip_prefix(['v', 'V'])
      && rest.starts_with(|ch: char| ch.is_ascii_digit())
   {
      text = rest;
   }
   let mut found = Vec::new();
   let mut chars = text.char_indices().peekable();
   while let Some((start, first)) = chars.next() {
      let digit = first.is_ascii_digit();
      if !digit && !first.is_ascii_alphabetic() {
         continue;
      }
      let mut end = start + first.len_utf8();
      while let Some(&(index, next)) = chars.peek() {
         if (digit && next.is_ascii_digit()) || (!digit && next.is_ascii_alphabetic()) {
            end = index + next.len_utf8();
            chars.next();
         } else {
            break;
         }
      }
      let part = &text[start..end];
      // pep440 spells pre-releases 1.0a1, while tmux's 3.7a and OpenSSL's
      // 1.1.1w are patch releases, so a lone letter needs a number after it
      let numbered = chars.peek().is_some_and(|&(_, next)| next.is_ascii_digit());
      found.push(if digit {
         Rank::Number(part.parse().unwrap_or(u64::MAX))
      } else if part.len() == 1 && !numbered {
         Rank::Word(part.to_owned())
      } else if let Some(index) = PRE.iter().position(|word| *word == part) {
         Rank::Pre(index)
      } else {
         Rank::Word(part.to_owned())
      });
   }
   found
}

pub fn is_prerelease(version: &str) -> bool {
   ranks(version)
      .iter()
      .any(|rank| matches!(*rank, Rank::Pre(_)))
}

/// A missing component equals 0 after a number (1.0 == 1.0.0) and sits
/// between pre-release and number otherwise (1.0rc1 < 1.0 < 1.0post1).
pub fn cmp(left: &str, right: &str) -> Ordering {
   let ours = ranks(left);
   let theirs = ranks(right);
   for index in 0..ours.len().max(theirs.len()) {
      let mut mine = ours.get(index).unwrap_or(&Rank::Absent);
      let mut other = theirs.get(index).unwrap_or(&Rank::Absent);
      if *mine == Rank::Absent && matches!(*other, Rank::Number(_)) {
         mine = &Rank::Number(0);
      }
      if *other == Rank::Absent && matches!(*mine, Rank::Number(_)) {
         other = &Rank::Number(0);
      }
      let order = mine.cmp(other);
      if order != Ordering::Equal {
         return order;
      }
   }
   Ordering::Equal
}

/// The newest of a list, None when empty.
pub fn max<'list, Versions>(versions: Versions) -> Option<&'list str>
where
   Versions: IntoIterator<Item = &'list str>,
{
   versions.into_iter().fold(None, |best, version| {
      match best {
         Some(found) if cmp(version, found) != Ordering::Greater => Some(found),
         _ => Some(version),
      }
   })
}

/// ">=1.2,<2", "==3.12.*", "!=4.0". Empty allows everything.
pub fn satisfies(version: &str, range: Option<&str>) -> bool {
   let Some(spec) = range else {
      return true;
   };
   spec
      .split(',')
      .map(str::trim)
      .filter(|clause| !clause.is_empty())
      .all(|clause| holds(version, clause))
}

fn holds(version: &str, clause: &str) -> bool {
   let (operator, rest) = [">=", "<=", "==", "!=", ">", "<"]
      .iter()
      .find_map(|op| clause.strip_prefix(op).map(|rest| (*op, rest)))
      .unwrap_or(("==", clause));
   let target = rest.trim();
   let order = match target.strip_suffix(".*") {
      Some(prefix) if version == prefix || version.starts_with(&format!("{prefix}.")) => {
         Ordering::Equal
      },
      Some(prefix) => cmp(version, prefix),
      None => cmp(version, target),
   };
   match operator {
      "!=" => order != Ordering::Equal,
      ">" => order == Ordering::Greater,
      ">=" => order != Ordering::Less,
      "<" => order == Ordering::Less,
      "<=" => order != Ordering::Greater,
      _ => order == Ordering::Equal,
   }
}
