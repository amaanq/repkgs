use std::{
   collections::{
      BTreeMap,
      HashMap,
   },
   env::var,
   fs::{
      canonicalize,
      metadata,
      read,
   },
   io,
   os::unix::fs::PermissionsExt as _,
   path::{
      Component,
      Path,
      PathBuf,
   },
};

use blake3::Hasher;
use wire::identity::Identity;

pub const STORE_DIR: &str = match option_env!("JIG_STORE_DIR") {
   Some(directory) => directory,
   None => "/nix/store",
};
const OUTPUT_PLACEHOLDER: &[u8] = b"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const PATH_ENDS: &str = " \t\n\\:\"';,)]}>";

pub struct Store {
   content:     bool,
   output:      String,
   output_hash: Vec<u8>,
   roots:       BTreeMap<String, String>,
   identities:  HashMap<String, String>,
}

impl Default for Store {
   #[inline]
   fn default() -> Self {
      Self::new()
   }
}

impl Store {
   #[must_use]
   #[inline]
   pub fn new() -> Self {
      let content = var("JIG_STORE_IDENTITY").is_ok_and(|value| value == "content");
      let output = var("out").unwrap_or_default();
      let output_hash = if content && is_store_path(&output) {
         output
            .as_bytes()
            .get(STORE_DIR.len() + 1..STORE_DIR.len() + 33)
            .unwrap_or_default()
            .to_vec()
      } else {
         Vec::new()
      };
      let mut roots = BTreeMap::new();
      for root in var("JIG_STORE_ROOTS")
         .unwrap_or_default()
         .split_whitespace()
         .chain([output.as_str()])
      {
         if !is_store_path(root) {
            continue;
         }
         let suffix = &root[STORE_DIR.len() + 1..];
         let end = suffix
            .find('/')
            .map_or(root.len(), |position| STORE_DIR.len() + 1 + position);
         let directory = &root[..end];
         roots
            .entry(mask_hashes(directory))
            .or_insert_with(|| directory.to_owned());
      }
      Self {
         content,
         output,
         output_hash,
         roots,
         identities: HashMap::new(),
      }
   }

   #[must_use]
   #[inline]
   pub fn mask_for_replay(&self, text: &str) -> String {
      if self.content {
         mask_hashes(text)
      } else {
         text.to_owned()
      }
   }

   #[must_use]
   #[inline]
   pub fn key(&self, arg: &str) -> String {
      self.mask_for_replay(&mask_random_names(&normalize_path_arg(arg)))
   }

   #[inline]
   pub fn mask_output(&self, data: &mut [u8], restore: bool) {
      if self.output_hash.len() != 32 {
         return;
      }
      let (from, to) = if restore {
         (OUTPUT_PLACEHOLDER, self.output_hash.as_slice())
      } else {
         (self.output_hash.as_slice(), OUTPUT_PLACEHOLDER)
      };
      let prefix = format!("{STORE_DIR}/");
      let mut offset = 0;
      while let Some(position) = data[offset..]
         .windows(from.len())
         .position(|window| window == from)
      {
         let start = offset + position;
         if data[..start].ends_with(prefix.as_bytes())
            && data.get(start + from.len()) == Some(&b'-')
         {
            data[start..start + from.len()].copy_from_slice(to);
         }
         offset = start + from.len();
      }
   }

   #[must_use]
   #[inline]
   pub fn resolve(&self, masked: &str) -> Option<String> {
      if !masked.starts_with(&format!("{STORE_DIR}/*-")) {
         return Some(masked.to_owned());
      }
      let end = masked[STORE_DIR.len() + 3..]
         .find('/')
         .map_or(masked.len(), |position| STORE_DIR.len() + 3 + position);
      self
         .roots
         .get(&masked[..end])
         .map(|root| format!("{root}{}", &masked[end..]))
   }

   #[must_use]
   #[inline]
   pub fn resolve_all(&self, text: &str) -> String {
      let mut resolved = text.to_owned();
      for (masked, real) in &self.roots {
         let mut result = String::with_capacity(resolved.len());
         let mut remaining = resolved.as_str();
         while let Some(position) = remaining.find(masked.as_str()) {
            let end = position + masked.len();
            let whole = remaining[end..]
               .chars()
               .next()
               .is_none_or(|next| next == '/' || PATH_ENDS.contains(next));
            result.push_str(&remaining[..position]);
            result.push_str(if whole { real } else { masked });
            remaining = &remaining[end..];
         }
         result.push_str(remaining);
         resolved = result;
      }
      resolved
   }

   #[must_use]
   #[inline]
   pub fn daemon_may_identify(&self, path: &str) -> bool {
      self.content
         && is_store_path(path)
         && (self.output.is_empty() || !Path::new(path).starts_with(&self.output))
   }

   #[inline]
   pub fn remember(&mut self, path: String, identity: String) {
      self.identities.insert(path, identity);
   }

   #[inline]
   pub fn input_id(&self, path: &str) -> io::Result<String> {
      if is_store_path(path) && !self.content {
         return Ok(format!("S:{path}"));
      }
      if let Some(identity) = self.identities.get(path) {
         return Ok(identity.to_owned());
      }
      let mut bytes = read(path)?;
      if !is_store_path(path) {
         self.mask_output(&mut bytes, false);
      }
      Ok(Identity::of(&bytes).to_string())
   }

   #[inline]
   pub fn tool_id(&self, program: &str) -> io::Result<String> {
      let path = on_path(program);
      let parent = path.parent().unwrap_or_else(|| Path::new("."));
      let name = path
         .file_name()
         .ok_or_else(|| io::Error::other("compiler has no filename"))?;
      let location = canonicalize(parent)?.join(name);
      let filename = location
         .to_str()
         .ok_or_else(|| io::Error::other("compiler path is not UTF-8"))?;
      if is_store_path(filename) {
         return Ok(self.key(filename));
      }
      let identity = self.input_id(filename)?;
      Ok(format!("{filename}={identity}"))
   }
}

#[inline]
pub fn field<Data>(hasher: &mut Hasher, data: Data)
where
   Data: AsRef<[u8]>,
{
   let bytes = data.as_ref();
   hasher.update(format!("{}:", bytes.len()).as_bytes());
   hasher.update(bytes);
}

#[must_use]
#[inline]
pub fn is_store_path(path: &str) -> bool {
   path
      .strip_prefix(STORE_DIR)
      .is_some_and(|tail| tail.starts_with('/'))
}

#[must_use]
#[inline]
pub fn mask_hashes(text: &str) -> String {
   let prefix = format!("{STORE_DIR}/");
   let mut remaining = text;
   let mut output = String::new();
   while let Some(position) = remaining.find(&prefix) {
      let start = position + prefix.len();
      output.push_str(&remaining[..start]);
      let tail = &remaining[start..];
      if tail.as_bytes().get(32) == Some(&b'-')
         && tail.as_bytes()[..32]
            .iter()
            .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(byte))
      {
         output.push('*');
         remaining = &tail[32..];
      } else {
         remaining = tail;
      }
   }
   output.push_str(remaining);
   output
}

fn normalize_path_arg(arg: &str) -> String {
   if arg.starts_with("-D") || arg.starts_with("-U") {
      return arg.to_owned();
   }
   let start = if let Some(option) = arg.strip_prefix("--") {
      let Some(equals) = option.find('=') else {
         return arg.to_owned();
      };
      equals + 3
   } else if arg.starts_with('-') && arg.len() > 2 && !arg.starts_with("--") {
      match arg.find('=') {
         Some(equals) if !arg[..equals].contains('/') => equals + 1,
         _ => 2,
      }
   } else if arg.starts_with('-') {
      return arg.to_owned();
   } else {
      0
   };
   let value = &arg[start..];
   if !value.contains('/') && !value.starts_with('.') {
      return arg.to_owned();
   }
   format!(
      "{}{}",
      &arg[..start],
      normalize(Path::new(value)).to_string_lossy()
   )
}

fn mask_random_names(text: &str) -> String {
   let mut masked = text.to_owned();
   for (prefix, width) in [("TryCompile-", 6), ("cmTC_", 5)] {
      let mut position = 0;
      while let Some(found) = masked[position..].find(prefix) {
         let start = position + found + prefix.len();
         position = start;
         if masked
            .get(start..start + width)
            .is_some_and(|tail| tail.bytes().all(|byte| byte.is_ascii_alphanumeric()))
         {
            masked.replace_range(start..start + width, &"#".repeat(width));
         }
      }
   }
   masked
}

#[must_use]
#[inline]
pub fn normalize(path: &Path) -> PathBuf {
   let mut normal = PathBuf::new();
   for component in path.components() {
      match component {
         Component::CurDir => {},
         Component::ParentDir if normal.file_name().is_some_and(|name| name != "..") => {
            normal.pop();
         },
         Component::ParentDir if normal.has_root() => {},
         Component::ParentDir => normal.push(".."),
         other @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
            normal.push(other);
         },
      }
   }
   if normal.as_os_str().is_empty() {
      normal.push(".");
   }
   normal
}

#[must_use]
#[inline]
pub fn on_path(program: &str) -> PathBuf {
   if !program.contains('/') {
      for directory in var("PATH").unwrap_or_default().split(':') {
         let path = Path::new(directory).join(program);
         if metadata(&path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
         {
            return path;
         }
      }
   }
   PathBuf::from(program)
}
