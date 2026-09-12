use std::{
   collections::HashMap,
   env::var,
   fs::{
      canonicalize,
      read,
   },
   io,
   path::{
      Component,
      Path,
      PathBuf,
   },
};

use blake3::Hasher;

use crate::files::digest;

pub const STORE_DIR: &str = match option_env!("JIG_STORE_DIR") {
   Some(directory) => directory,
   None => "/nix/store",
};
const OUTPUT_PLACEHOLDER: &[u8] = b"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

pub struct Store {
   content:     bool,
   output:      String,
   output_hash: Vec<u8>,
   roots:       HashMap<String, String>,
   identities:  HashMap<String, String>,
}

impl Store {
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
      let mut roots = HashMap::new();
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

   pub fn key(&self, text: &str) -> String {
      if self.content {
         mask_hashes(text)
      } else {
         text.to_owned()
      }
   }

   pub fn mask_output(&self, data: &mut [u8], restore: bool) {
      if self.output_hash.len() != 32 {
         return;
      }
      let (from, to) = if restore {
         (OUTPUT_PLACEHOLDER, self.output_hash.as_slice())
      } else {
         (self.output_hash.as_slice(), OUTPUT_PLACEHOLDER)
      };
      let mut offset = 0;
      while let Some(position) = data[offset..]
         .windows(from.len())
         .position(|window| window == from)
      {
         let start = offset + position;
         data[start..start + from.len()].copy_from_slice(to);
         offset = start + from.len();
      }
   }

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

   pub fn resolve_all(&self, text: &str) -> io::Result<String> {
      if !self.content {
         return Ok(text.to_owned());
      }
      let masked = mask_hashes(text);
      let marker = format!("{STORE_DIR}/*-");
      let mut result = String::new();
      let mut remaining = masked.as_str();
      while let Some(position) = remaining.find(&marker) {
         result.push_str(&remaining[..position]);
         let path = &remaining[position..];
         let end = path
            .find([' ', '\t', '\n', '\\', ':'])
            .unwrap_or(path.len());
         result.push_str(
            &self
               .resolve(&path[..end])
               .ok_or_else(|| io::Error::other("cached dependency has no current store root"))?,
         );
         remaining = &path[end..];
      }
      result.push_str(remaining);
      Ok(result)
   }

   pub fn daemon_may_identify(&self, path: &str) -> bool {
      self.content
         && is_store_path(path)
         && (self.output.is_empty() || !Path::new(path).starts_with(&self.output))
   }

   pub fn remember(&mut self, path: String, identity: String) {
      self.identities.insert(path, identity);
   }

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
      Ok(format!("C:{}", digest(&bytes)))
   }

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
         return Ok(filename.to_owned());
      }
      let identity = self.input_id(filename)?;
      Ok(format!("{filename}={identity}"))
   }
}

pub fn field<Data: AsRef<[u8]>>(hasher: &mut Hasher, data: Data) {
   let bytes = data.as_ref();
   hasher.update(format!("{}:", bytes.len()).as_bytes());
   hasher.update(bytes);
}

pub fn is_store_path(path: &str) -> bool {
   path
      .strip_prefix(STORE_DIR)
      .is_some_and(|tail| tail.starts_with('/'))
}

pub fn mask_hashes(text: &str) -> String {
   let prefix = format!("{STORE_DIR}/");
   let mut remaining = text;
   let mut output = String::new();
   while let Some(position) = remaining.find(&prefix) {
      let start = position + prefix.len();
      output.push_str(&remaining[..start]);
      let tail = &remaining[start..];
      if tail.as_bytes().get(32) == Some(&b'-')
         && tail.as_bytes()[..32].iter().all(u8::is_ascii_alphanumeric)
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

pub fn normalize(path: &Path) -> PathBuf {
   let mut normal = PathBuf::new();
   for component in path.components() {
      match component {
         Component::CurDir => {},
         Component::ParentDir if normal.file_name().is_some_and(|name| name != "..") => {
            normal.pop();
         },
         Component::ParentDir if normal.has_root() => {},
         other => normal.push(other),
      }
   }
   if normal.as_os_str().is_empty() {
      normal.push(".");
   }
   normal
}

pub fn on_path(program: &str) -> PathBuf {
   if !program.contains('/') {
      for directory in var("PATH").unwrap_or_default().split(':') {
         let path = Path::new(directory).join(program);
         if path.is_file() {
            return path;
         }
      }
   }
   PathBuf::from(program)
}
