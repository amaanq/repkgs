use std::{
   collections::{
      BTreeMap,
      BTreeSet,
      HashMap,
   },
   env::{
      current_exe,
      var,
   },
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   fs::{
      Metadata,
      canonicalize,
      metadata,
      read,
   },
   io,
   os::unix::fs::{
      MetadataExt as _,
      PermissionsExt as _,
   },
   path::{
      Component,
      Path,
      PathBuf,
   },
   str::FromStr,
   time::{
      Duration,
      SystemTime,
      UNIX_EPOCH,
   },
};

use blake3::{
   Hash,
   Hasher,
};
use serde::{
   Deserialize,
   Serialize,
};
use tightrope::{
   client::Client,
   digest::Digest,
   identity::Identity,
   key::{
      Key,
      Namespace,
   },
};

use crate::{
   elf::{
      is_elf,
      needed,
      runpath_text,
      segments,
   },
   image::Image,
};

pub const STORE_DIR: &str = match option_env!("MUMMER_STORE_DIR") {
   Some(directory) => directory,
   None => "/nix/store",
};

const OUTPUT_PLACEHOLDER: &[u8] = b"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const PATH_ENDS: &str = " \t\n\\:\"';,)]}>";
const HASH_ALPHABET: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";
const RANDOM_NAMES: [(&str, usize); 3] = [("TryCompile-", 6), ("cmTC_", 5), ("/rustc", 6)];

/// What a key records of an input file.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub enum InputId {
   /// A store path by name, when names alone identify the store.
   Path(String),
   Content(Identity),
   /// A program's bytes outside its RUNPATH, which relinking rewrites.
   Loaded(Hash),
}

impl Display for InputId {
   #[inline]
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      match *self {
         Self::Path(ref path) => write!(f, "S:{path}"),
         Self::Content(identity) => identity.fmt(f),
         Self::Loaded(ref hash) => write!(f, "L:{}", hash.to_hex()),
      }
   }
}

impl FromStr for InputId {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      if let Some(path) = text.strip_prefix("S:") {
         return Ok(Self::Path(path.to_owned()));
      }
      if let Some(hex) = text.strip_prefix("L:") {
         return Hash::from_hex(hex)
            .map(Self::Loaded)
            .map_err(io::Error::other);
      }
      text.parse().map(Self::Content)
   }
}

impl TryFrom<String> for InputId {
   type Error = io::Error;

   #[inline]
   fn try_from(text: String) -> io::Result<Self> {
      text.parse()
   }
}

impl From<InputId> for String {
   #[inline]
   fn from(identity: InputId) -> Self {
      identity.to_string()
   }
}

pub struct Store {
   content:     bool,
   output:      String,
   output_hash: Vec<u8>,
   roots:       BTreeMap<String, String>,
   identities:  HashMap<String, InputId>,
   random:      Vec<(String, String)>,
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
      Self::identified(var("MUMMER_STORE_IDENTITY").is_ok_and(|value| value == "content"))
   }

   /// Store paths identified by path, never masked, for outputs that embed
   /// the paths they read.
   #[must_use]
   #[inline]
   pub fn exact() -> Self {
      Self::identified(false)
   }

   fn identified(content: bool) -> Self {
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
      let toolchain = current_exe()
         .ok()
         .and_then(|exe| Some(exe.parent()?.parent()?.to_string_lossy().into_owned()))
         .unwrap_or_default();
      for root in var("MUMMER_STORE_ROOTS")
         .unwrap_or_default()
         .split_whitespace()
         .chain([output.as_str(), toolchain.as_str()])
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
         random: Vec::new(),
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
         return Some(self.unmask_random(masked));
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
      self.unmask_random(&resolved)
   }

   /// Stored text with this build's output and store paths put back.
   #[inline]
   pub fn replayed_text(&self, text: String) -> io::Result<String> {
      let mut bytes = text.into_bytes();
      self.mask_output(&mut bytes, true);
      Ok(self.resolve_all(&String::from_utf8(bytes).map_err(io::Error::other)?))
   }

   /// Text to store with this build's output and store hashes masked.
   #[inline]
   pub fn stored_text(&self, text: &str) -> io::Result<String> {
      let mut bytes = self.mask_text(text).into_bytes();
      self.mask_output(&mut bytes, false);
      String::from_utf8(bytes).map_err(io::Error::other)
   }

   /// The scratch names `CMake` made up for this run, so paths a manifest
   /// stored masked lead back into this run's directories.
   #[inline]
   pub fn learn_random(&mut self, args: &[String]) {
      for arg in args {
         for (prefix, width) in RANDOM_NAMES {
            let mut position = 0;
            while let Some(found) = arg[position..].find(prefix) {
               let start = position + found + prefix.len();
               position = start;
               let Some(tail) = random_tail(arg, start, width) else {
                  continue;
               };
               let masked = format!("{prefix}{}", "#".repeat(width));
               if !self.random.iter().any(|known| known.0 == masked) {
                  self.random.push((masked, format!("{prefix}{tail}")));
               }
            }
         }
      }
   }

   fn unmask_random(&self, text: &str) -> String {
      self.random.iter().fold(text.to_owned(), |unmasked, known| {
         unmasked.replace(&known.0, &known.1)
      })
   }

   /// A link argument the output embeds as written, its store hashes masked
   /// only when this build's roots give every one back, as a replay must.
   #[must_use]
   #[inline]
   pub fn key_embedded(&self, arg: &str) -> String {
      let masked = mask_hashes(arg);
      if self.content && self.resolve_all(&masked) == arg {
         masked
      } else {
         arg.to_owned()
      }
   }

   /// Text to store for replay, store hashes and `CMake` scratch names
   /// masked.
   #[must_use]
   #[inline]
   pub fn mask_text(&self, text: &str) -> String {
      self.mask_for_replay(&mask_random_names(text))
   }

   /// Store paths in a replayed output moved in place to the roots of the same
   /// name, since content keys let it come from a build against other hashes.
   #[inline]
   pub fn resolve_bytes(&self, data: &mut [u8]) {
      if !self.content {
         return;
      }
      let prefix = format!("{STORE_DIR}/");
      let mut offset = 0;
      while let Some(position) = data[offset..]
         .windows(prefix.len())
         .position(|window| window == prefix.as_bytes())
      {
         let start = offset + position + prefix.len();
         offset = start;
         let named = data.get(start..start + 33).is_some_and(|hash| {
            hash[32] == b'-' && hash[..32].iter().all(|byte| HASH_ALPHABET.contains(byte))
         });
         if !named {
            continue;
         }
         let end = data[start + 33..]
            .iter()
            .position(|&byte| byte == b'/' || byte == 0 || PATH_ENDS.as_bytes().contains(&byte))
            .map_or(data.len(), |length| start + 33 + length);
         let Ok(name) = str::from_utf8(&data[start + 33..end]) else {
            continue;
         };
         if let Some(root) = self.roots.get(&format!("{STORE_DIR}/*-{name}")) {
            data[start..start + 32]
               .copy_from_slice(&root.as_bytes()[prefix.len()..prefix.len() + 32]);
         }
         offset = end;
      }
   }

   #[must_use]
   #[inline]
   pub fn daemon_may_identify(&self, path: &str) -> bool {
      self.content
         && is_store_path(path)
         && (self.output.is_empty() || !Path::new(path).starts_with(&self.output))
   }

   #[inline]
   pub fn remember(&mut self, path: String, identity: InputId) {
      self.identities.insert(path, identity);
   }

   /// The cache key for a file's identity, from everything `stat` says about
   /// it, or None when it is a store path named by path, already known, or
   /// changed within the last two seconds, finer than the kernel's timestamps.
   #[must_use]
   #[inline]
   pub fn stamp_key(&self, path: &str) -> Option<Digest> {
      if (is_store_path(path) && !self.content) || self.identities.contains_key(path) {
         return None;
      }
      let meta = metadata(path).ok().filter(Metadata::is_file)?;
      let settled = SystemTime::now().checked_sub(Duration::from_secs(2))?;
      if meta.modified().ok()? >= settled
         || UNIX_EPOCH.checked_add(Duration::new(
            u64::try_from(meta.ctime()).ok()?,
            u32::try_from(meta.ctime_nsec()).ok()?,
         ))? >= settled
      {
         return None;
      }
      let mut hasher = Hasher::new();
      for part in [
         path.to_owned(),
         self.output.clone(),
         meta.dev().to_string(),
         meta.ino().to_string(),
         meta.len().to_string(),
         format!("{}.{}", meta.mtime(), meta.mtime_nsec()),
         format!("{}.{}", meta.ctime(), meta.ctime_nsec()),
      ] {
         field(&mut hasher, part);
      }
      Some(Digest::from(hasher.finalize()))
   }

   #[inline]
   pub fn input_id(&self, path: &str) -> io::Result<InputId> {
      if is_store_path(path) && !self.content {
         return Ok(InputId::Path(path.to_owned()));
      }
      if let Some(identity) = self.identities.get(path) {
         return Ok(identity.clone());
      }
      let mut bytes = read(path)?;
      if !is_store_path(path) {
         self.mask_output(&mut bytes, false);
      }
      Ok(InputId::Content(Identity::of(&bytes)))
   }

   /// A program as the code it runs, itself and the libraries it loads
   /// through RUNPATH, so relinking a toolchain against other directories
   /// keeps the compiles it already did.
   #[inline]
   pub fn tool_identity(&self, cache: &mut Client, program: &str) -> io::Result<String> {
      let mut pending = vec![canonicalize(on_path(program))?];
      let mut visited = BTreeSet::new();
      let mut found = BTreeSet::new();
      while let Some(file) = pending.pop() {
         if !visited.insert(file.clone()) {
            continue;
         }
         let (identity, libraries) = self.loaded_identity(cache, &file)?;
         let name = file.file_name().unwrap_or_default().to_string_lossy();
         found.insert(format!("{name}={identity}"));
         pending.extend(libraries);
      }
      Ok(found.into_iter().collect::<Vec<_>>().join(","))
   }

   /// One file's identity with its RUNPATH and the room reserved after it
   /// left out, and the libraries it loads, remembered by its stat stamp.
   fn loaded_identity(
      &self,
      cache: &mut Client,
      file: &Path,
   ) -> io::Result<(InputId, Vec<PathBuf>)> {
      let memo = self
         .stamp_key(&file.to_string_lossy())
         .map(|stamp| Key::digest(Namespace::Loaded, &stamp));
      if let Some(ref key) = memo
         && let Some(bytes) = cache.get(key)?
      {
         let text = String::from_utf8(bytes).map_err(io::Error::other)?;
         let mut lines = text.lines();
         if let Some(identity) = lines.next() {
            return Ok((identity.parse()?, lines.map(PathBuf::from).collect()));
         }
      }
      let image = Image { bytes: read(file)? };
      let mut libraries = Vec::new();
      let identity = if is_elf(&image) {
         let program_headers = segments(&image)?;
         let mut hasher = Hasher::new();
         if let Some(text) = runpath_text(&image, &program_headers)? {
            let mut end = text.offset + text.capacity;
            while image.bytes.get(end) == Some(&0) {
               end += 1;
            }
            let outside = image
               .bytes
               .get(..text.offset)
               .zip(image.bytes.get(end..))
               .ok_or_else(|| io::Error::other("ELF RUNPATH is outside the file"))?;
            hasher.update(outside.0);
            field(&mut hasher, format!("runpath {}", end - text.offset));
            hasher.update(outside.1);
            let origin = file.parent().unwrap_or_else(|| Path::new("/"));
            let directories = text
               .value
               .split(':')
               .filter(|entry| !entry.is_empty())
               .map(|entry| {
                  PathBuf::from(
                     entry
                        .replace("${ORIGIN}", &origin.to_string_lossy())
                        .replace("$ORIGIN", &origin.to_string_lossy()),
                  )
               })
               .collect::<Vec<_>>();
            for library in needed(&image, &program_headers)? {
               if let Some(path) = directories
                  .iter()
                  .map(|directory| directory.join(&library.value))
                  .find(|path| path.is_file())
               {
                  libraries.push(canonicalize(path)?);
               }
            }
         } else {
            hasher.update(&image.bytes);
         }
         InputId::Loaded(hasher.finalize())
      } else {
         InputId::Content(Identity::of(&image.bytes))
      };
      if let Some(key) = memo {
         let mut record = identity.to_string();
         for library in &libraries {
            record.push('\n');
            record.push_str(&library.to_string_lossy());
         }
         cache.put(&key, record.as_bytes())?;
      }
      Ok((identity, libraries))
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
            .all(|byte| HASH_ALPHABET.contains(byte))
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

/// The `width` alphanumerics at `start`, when nothing alphanumeric follows.
fn random_tail(text: &str, start: usize, width: usize) -> Option<&str> {
   let tail = text.get(start..start + width)?;
   let ends = text
      .as_bytes()
      .get(start + width)
      .is_none_or(|byte| !byte.is_ascii_alphanumeric());
   (ends && tail.bytes().all(|byte| byte.is_ascii_alphanumeric())).then_some(tail)
}

#[must_use]
#[inline]
pub fn mask_random_names(text: &str) -> String {
   let mut masked = text.to_owned();
   for (prefix, width) in RANDOM_NAMES {
      let mut position = 0;
      while let Some(found) = masked[position..].find(prefix) {
         let start = position + found + prefix.len();
         position = start;
         if random_tail(&masked, start, width).is_some() {
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
