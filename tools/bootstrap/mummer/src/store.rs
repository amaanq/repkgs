use std::{
   cmp,
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
      read_dir,
      read_to_string,
   },
   io,
   ops::Range,
   os::unix::fs::{
      MetadataExt as _,
      PermissionsExt as _,
   },
   path::{
      Component,
      Path,
      PathBuf,
   },
   process,
   str::FromStr,
   sync::LazyLock,
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
      PT_LOAD,
      build_ids,
      is_elf,
      needed,
      runpath_text,
      segments,
   },
   files::utf8,
   image::Image,
   store_path::{
      HASH_ALPHABET,
      StorePath,
   },
   tree::find,
};

pub const STORE_DIR: &str = match option_env!("MUMMER_STORE_DIR") {
   Some(directory) => directory,
   None => "/nix/store",
};

const OUTPUT_PLACEHOLDER: &[u8] = b"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const PATH_ENDS: &str = " \t\n\\:\"';,)]}>";
const RANDOM_NAMES: [(&str, usize); 3] =
   [("TryCompile-", 6), ("cmTC_", 5), ("meson-private/tmp", 8)];
/// cc-rs names its compiler probe after a random number.
const RANDOM_STEMS: [&str; 1] = ["detect_compiler_family.c"];
/// mkdtemp templates whose random part can come out one case with no digit:
/// rustc's link dir and Python's tempfile, directly under `$TMPDIR`.
const TEMPLATES: [(&str, usize); 2] = [("rustc", 6), ("tmp", 8)];
/// This process and its ancestors by pid, each with when it started.
static ANCESTORS: LazyLock<BTreeMap<String, SystemTime>> = LazyLock::new(|| {
   let mut ancestors = BTreeMap::new();
   let mut pid = process::id().to_string();
   while let Ok(stat) = read_to_string(format!("/proc/{pid}/stat")) {
      let fields = stat
         .rsplit_once(')')
         .map(|(_, rest)| rest.split_whitespace().collect::<Vec<_>>())
         .unwrap_or_default();
      let started = fields.get(19).and_then(|starttime| started_at(starttime));
      ancestors.insert(pid.clone(), started.unwrap_or(UNIX_EPOCH));
      let Some(&parent) = fields.get(1) else {
         break;
      };
      if matches!(parent, "0" | "1") || ancestors.contains_key(parent) {
         break;
      }
      parent.clone_into(&mut pid);
   }
   ancestors
});

/// A process start in clock ticks after boot, `/proc/<pid>/stat`'s
/// starttime, as a time. The tick rate is the auxiliary vector's
/// `AT_CLKTCK`, the boot time `/proc/stat`'s btime.
fn started_at(starttime: &str) -> Option<SystemTime> {
   const AT_CLKTCK: u64 = 17;
   let auxv = read("/proc/self/auxv").ok()?;
   let rate = auxv
      .as_chunks::<16>()
      .0
      .iter()
      .map(|pair| {
         let (kind, value) = pair.split_at(8);
         (
            u64::from_ne_bytes(kind.try_into().unwrap_or_default()),
            u64::from_ne_bytes(value.try_into().unwrap_or_default()),
         )
      })
      .find(|pair| pair.0 == AT_CLKTCK)?
      .1;
   let boot = read_to_string("/proc/stat")
      .ok()?
      .lines()
      .find_map(|line| line.strip_prefix("btime "))?
      .trim()
      .parse::<u64>()
      .ok()?;
   let ticks = starttime.parse::<u64>().ok()?;
   let nanos = ticks
      .checked_rem(rate)?
      .checked_mul(1_000_000_000)?
      .checked_div(rate)?;
   let seconds = boot.checked_add(ticks.checked_div(rate)?)?;
   UNIX_EPOCH.checked_add(Duration::from_secs(seconds) + Duration::from_nanos(nanos))
}
static TEMPORARY: LazyLock<Option<String>> = LazyLock::new(|| {
   var("TMPDIR")
      .ok()
      .map(|directory| directory.trim_end_matches('/').to_owned())
      .filter(|directory| directory.starts_with('/') && directory.len() > 1)
});

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
   content:    bool,
   output:     String,
   /// `$out` when content mode masks its hash in what is stored.
   masked:     Option<StorePath>,
   roots:      BTreeMap<String, Vec<StorePath>>,
   identities: HashMap<String, InputId>,
   random:     Vec<(String, String)>,
}

impl Default for Store {
   #[inline]
   fn default() -> Self {
      Self::new()
   }
}

impl Store {
   #[inline]
   #[must_use]
   pub fn new() -> Self {
      Self::identified(var("MUMMER_STORE_IDENTITY").is_ok_and(|value| value == "content"))
   }

   /// Store paths identified by path, never masked, for outputs that embed
   /// the paths they read.
   #[inline]
   #[must_use]
   pub fn exact() -> Self {
      Self::identified(false)
   }

   fn identified(content: bool) -> Self {
      let output = var("out").unwrap_or_default();
      let masked = content.then(|| StorePath::containing(&output)).flatten();
      let mut roots = BTreeMap::new();
      let toolchain = current_exe()
         .ok()
         .and_then(|exe| Some(exe.parent()?.parent()?.to_str()?.to_owned()))
         .unwrap_or_default();
      for root in var("MUMMER_STORE_ROOTS")
         .unwrap_or_default()
         .split_whitespace()
         .chain([output.as_str(), toolchain.as_str()])
      {
         if let Some(object) = StorePath::containing(root) {
            let same = roots
               .entry(mask_hashes(object.as_str()))
               .or_insert_with(Vec::new);
            if !same.contains(&object) {
               same.push(object);
            }
         }
      }
      Self {
         content,
         output,
         masked,
         roots,
         identities: HashMap::new(),
         random: Vec::new(),
      }
   }

   #[inline]
   #[must_use]
   pub fn mask_for_replay(&self, text: &str) -> String {
      if self.content {
         mask_hashes(text)
      } else {
         text.to_owned()
      }
   }

   #[inline]
   #[must_use]
   pub fn key(&self, arg: &str) -> String {
      self.mask_for_replay(&mask_random_names(&normalize_path_arg(arg)))
   }

   #[inline]
   pub fn mask_output(&self, data: &mut [u8], restore: bool) {
      let Some(ref output) = self.masked else {
         return;
      };
      let hash = output.hash_part().as_bytes();
      let (from, to) = if restore {
         (OUTPUT_PLACEHOLDER, hash)
      } else {
         (hash, OUTPUT_PLACEHOLDER)
      };
      let prefix = format!("{STORE_DIR}/");
      let mut offset = 0;
      while let Some(position) = find(&data[offset..], from) {
         let start = offset + position;
         if data[..start].ends_with(prefix.as_bytes())
            && data.get(start + from.len()) == Some(&b'-')
         {
            data[start..start + from.len()].copy_from_slice(to);
         }
         offset = start + from.len();
      }
   }

   #[inline]
   #[must_use]
   pub fn resolve(&self, masked: &str) -> Option<String> {
      if !masked.starts_with(&format!("{STORE_DIR}/*-")) {
         return Some(self.unmask_random(masked));
      }
      let end = masked[STORE_DIR.len() + 3..]
         .find('/')
         .map_or(masked.len(), |position| STORE_DIR.len() + 3 + position);
      self
         .root_for(&masked[..end], &masked[end..])
         .map(|root| format!("{root}{}", &masked[end..]))
   }

   /// Files in this run's scratch directories that mask to `masked`, for an
   /// input no argument names, as the CPP header GHC writes in its own
   /// pid-named temp dir.
   #[inline]
   #[must_use]
   pub fn scratch_candidates(masked: &str) -> Vec<String> {
      let Some(ref temporary) = *TEMPORARY else {
         return Vec::new();
      };
      let Some(rest) = masked.strip_prefix(&format!("{temporary}/")) else {
         return Vec::new();
      };
      let mut found = vec![temporary.clone()];
      let mut spelled = temporary.clone();
      for component in rest.split('/') {
         spelled = format!("{spelled}/{component}");
         let mut next = Vec::new();
         for directory in &found {
            if !component.contains('#') {
               next.push(format!("{directory}/{component}"));
               continue;
            }
            let Ok(entries) = read_dir(directory) else {
               continue;
            };
            next.extend(
               entries
                  .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                  .map(|name| format!("{directory}/{name}"))
                  .filter(|path| mask_random_names(path) == spelled),
            );
         }
         found = next;
      }
      found.retain(|path| Path::new(path).is_file());
      found
   }

   /// The root a masked one stands for, the one holding `rest` when two
   /// share a name, as a cross build's host and build zlib do.
   fn root_for(&self, masked: &str, rest: &str) -> Option<&StorePath> {
      let candidates = self.roots.get(masked)?;
      match *candidates.as_slice() {
         [ref only] => Some(only),
         _ => {
            candidates
               .iter()
               .find(|root| Path::new(&format!("{root}{rest}")).exists())
               .or_else(|| candidates.first())
         },
      }
   }

   #[inline]
   #[must_use]
   pub fn resolve_all(&self, text: &str) -> String {
      let mut resolved = text.to_owned();
      for masked in self.roots.keys() {
         let mut result = String::with_capacity(resolved.len());
         let mut remaining = resolved.as_str();
         while let Some(position) = remaining.find(masked.as_str()) {
            let end = position + masked.len();
            let rest = &remaining[end..];
            let whole = rest
               .chars()
               .next()
               .is_none_or(|next| next == '/' || PATH_ENDS.contains(next));
            let path = rest
               .find(|next: char| next.is_whitespace() || PATH_ENDS.contains(next))
               .map_or(rest, |length| &rest[..length]);
            result.push_str(&remaining[..position]);
            match self.root_for(masked, path) {
               Some(real) if whole => result.push_str(real.as_str()),
               _ => result.push_str(masked),
            }
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

   /// The scratch names `CMake`, meson, cc-rs and GHC made up for this run, so
   /// paths a manifest stored masked lead back into this run's directories.
   #[inline]
   pub fn learn_random(&mut self, args: &[String]) {
      for arg in args {
         for (span, masked) in random_names(arg) {
            if !self.random.iter().any(|known| known.0 == masked) {
               self.random.push((masked, arg[span].to_owned()));
            }
         }
      }
   }

   /// A cc1 job also names its source bare, as `-main-file-name`, so the
   /// file names this call made up are masked as whole tokens too.
   #[inline]
   #[must_use]
   pub fn key_job(&self, job: &str) -> String {
      let base = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
      self.random.iter().fold(self.key(job), |keyed, pair| {
         let (masked, real) = (base(&pair.0), base(&pair.1));
         if masked == real {
            keyed
         } else {
            keyed.replace(&format!("\"{real}\""), &format!("\"{masked}\""))
         }
      })
   }

   /// Longest first, since a dir's spelling begins its files'.
   fn unmask_random(&self, text: &str) -> String {
      let mut known = self.random.iter().collect::<Vec<_>>();
      known.sort_by_key(|pair| cmp::Reverse(pair.0.len()));
      known.into_iter().fold(text.to_owned(), |unmasked, pair| {
         unmasked.replace(&pair.0, &pair.1)
      })
   }

   /// A link argument the output embeds as written, its store hashes masked
   /// only when this build's roots give every one back, as a replay must.
   #[inline]
   #[must_use]
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
   #[inline]
   #[must_use]
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
      while let Some(position) = find(&data[offset..], prefix.as_bytes()) {
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
         let rest_end = data[end..]
            .iter()
            .position(|&byte| byte == 0 || PATH_ENDS.as_bytes().contains(&byte))
            .map_or(data.len(), |length| end + length);
         let rest = str::from_utf8(&data[end..rest_end]).unwrap_or_default();
         if let Some(root) = self.root_for(&format!("{STORE_DIR}/*-{name}"), rest) {
            data[start..start + 32].copy_from_slice(root.hash_part().as_bytes());
         }
         offset = end;
      }
   }

   #[inline]
   #[must_use]
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
   #[inline]
   #[must_use]
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
      Some(self.stamp_of(path, &meta))
   }

   /// The stamp of a file this process just wrote, whose bytes it holds, so
   /// no recent change can hide behind an unchanged stat.
   #[inline]
   #[must_use]
   pub fn written_stamp_key(&self, path: &str) -> Option<Digest> {
      if (is_store_path(path) && !self.content) || self.identities.contains_key(path) {
         return None;
      }
      let meta = metadata(path).ok().filter(Metadata::is_file)?;
      Some(self.stamp_of(path, &meta))
   }

   fn stamp_of(&self, path: &str, meta: &Metadata) -> Digest {
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
      Digest::from(hasher.finalize())
   }

   #[inline]
   pub fn input_id(&self, path: &str) -> io::Result<InputId> {
      if is_store_path(path) && !self.content {
         return Ok(InputId::Path(path.to_owned()));
      }
      if let Some(identity) = self.identities.get(path) {
         return Ok(identity.clone());
      }
      Ok(self.content_id(path, read(path)?))
   }

   /// What `input_id` makes of `path` holding `bytes`.
   #[inline]
   #[must_use]
   pub fn content_id(&self, path: &str, mut bytes: Vec<u8>) -> InputId {
      if !is_store_path(path) {
         self.mask_output(&mut bytes, false);
         if let Some(ref temporary) = *TEMPORARY {
            bytes = mask_scratch_bytes(&bytes, temporary);
         }
      }
      InputId::Content(Identity::of(&bytes))
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
         let name = utf8(Path::new(file.file_name().unwrap_or_default()))?;
         found.insert(format!("{name}={identity}"));
         pending.extend(libraries);
      }
      Ok(found.into_iter().collect::<Vec<_>>().join(","))
   }

   /// One file's identity, the bytes it loads with its RUNPATH, the room
   /// reserved after it and its build-id left out, since the build-id
   /// hashes debug info too, and the libraries it loads, remembered by its
   /// stat stamp.
   fn loaded_identity(
      &self,
      cache: &mut Client,
      file: &Path,
   ) -> io::Result<(InputId, Vec<PathBuf>)> {
      let memo = self
         .stamp_key(utf8(file)?)
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
         let mut loaded = image.bytes.clone();
         loaded
            .get_mut(40..48)
            .ok_or_else(|| io::Error::other("ELF header is too short"))?
            .fill(0);
         for range in build_ids(&image, &program_headers)? {
            loaded[range].fill(0);
         }
         if let Some(text) = runpath_text(&image, &program_headers)? {
            let mut end = text.offset + text.capacity;
            while image.bytes.get(end) == Some(&0) {
               end += 1;
            }
            loaded
               .get_mut(text.offset..end)
               .ok_or_else(|| io::Error::other("ELF RUNPATH is outside the file"))?
               .fill(0);
            field(&mut hasher, format!("runpath {}", end - text.offset));
            let origin = utf8(file.parent().unwrap_or_else(|| Path::new("/")))?;
            let directories = text
               .value
               .split(':')
               .filter(|entry| !entry.is_empty())
               .map(|entry| {
                  PathBuf::from(
                     entry
                        .replace("${ORIGIN}", origin)
                        .replace("$ORIGIN", origin),
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
         }
         for segment in program_headers
            .iter()
            .filter(|segment| segment.kind == PT_LOAD)
         {
            hasher.update(&loaded[segment.offset..segment.offset + segment.size]);
         }
         InputId::Loaded(hasher.finalize())
      } else {
         InputId::Content(Identity::of(&image.bytes))
      };
      if let Some(key) = memo {
         let mut record = identity.to_string();
         for library in &libraries {
            record.push('\n');
            record.push_str(utf8(library)?);
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

/// `path` lies inside the store, as a path rather than its text.
#[inline]
#[must_use]
pub fn in_store(path: &Path) -> bool {
   path.starts_with(STORE_DIR) && path != Path::new(STORE_DIR)
}

#[inline]
#[must_use]
pub fn is_store_path(path: &str) -> bool {
   path
      .strip_prefix(STORE_DIR)
      .is_some_and(|tail| tail.starts_with('/'))
}

#[inline]
#[must_use]
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
   normalize(Path::new(value)).to_str().map_or_else(
      || arg.to_owned(),
      |normal| format!("{}{normal}", &arg[..start]),
   )
}

/// Whether the `width` bytes at `start` are a scratch name, letters, digits
/// or `_` with nothing alphanumeric after them.
fn random_tail(text: &str, start: usize, width: usize) -> bool {
   let Some(tail) = text.get(start..start + width) else {
      return false;
   };
   let ends = text
      .as_bytes()
      .get(start + width)
      .is_none_or(|byte| !byte.is_ascii_alphanumeric());
   ends
      && tail
         .bytes()
         .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn random_names(text: &str) -> Vec<(Range<usize>, String)> {
   let mut names = Vec::new();
   for (prefix, width) in RANDOM_NAMES {
      for (found, _) in text.match_indices(prefix) {
         let start = found + prefix.len();
         if random_tail(text, start, width) {
            names.push((
               found..start + width,
               format!("{prefix}{}", "#".repeat(width)),
            ));
         }
      }
   }
   for stem in RANDOM_STEMS {
      for (found, _) in text.match_indices(stem) {
         let digits = text[..found]
            .bytes()
            .rev()
            .take_while(u8::is_ascii_digit)
            .count();
         if digits > 0 {
            names.push((found - digits..found + stem.len(), format!("#{stem}")));
         }
      }
   }
   names.extend(pid_names(text));
   names.extend(gnulib_names(text));
   if let Some(ref temporary) = *TEMPORARY {
      names.extend(scratch_names(text, temporary));
   }
   names.sort_by_key(|name| name.0.start);
   names.dedup_by(|later, earlier| later.0.start < earlier.0.end);
   names
}

/// What tools make up directly under `$TMPDIR` for one run, go-build,
/// config.guess's cg, GHC's and base's pid-numbered names, mkdtemp's: every
/// entry there with a random run in its name, and the entries below it while
/// they have one too. One span per path, so each spells apart for unmasking.
fn scratch_names(text: &str, temporary: &str) -> Vec<(Range<usize>, String)> {
   let mut names = Vec::new();
   let root = format!("{temporary}/");
   for (found, _) in text.match_indices(&root) {
      if !scratch_root_at(text.as_bytes(), found) {
         continue;
      }
      let mut spelling = temporary.to_owned();
      let mut start = found + root.len();
      let mut end = found;
      loop {
         let length = text[start..]
            .bytes()
            .take_while(|byte| is_name_byte(*byte))
            .count();
         let component = &text[start..start + length];
         let templated = (end == found)
            .then(|| {
               TEMPLATES.iter().find(|&&(prefix, width)| {
                  component.len() == prefix.len() + width
                     && component.starts_with(prefix)
                     && random_tail(component, prefix.len(), width)
               })
            })
            .flatten();
         let masked = match templated {
            Some(&(prefix, _)) => format!("{prefix}#"),
            None => mask_runs(component),
         };
         if masked == component {
            break;
         }
         spelling.push('/');
         spelling.push_str(&masked);
         end = start + length;
         if text.as_bytes().get(end) != Some(&b'/') {
            break;
         }
         start = end + 1;
      }
      if end > found {
         names.push((found..end, spelling));
      }
   }
   names
}

/// A `$TMPDIR` path standing alone or joined to an option, as in
/// `-I/build/tmp…`, not the tail of a longer path.
fn scratch_root_at(text: &[u8], found: usize) -> bool {
   let before = text[..found]
      .iter()
      .rev()
      .take_while(|byte| is_name_byte(**byte))
      .count();
   before == 0 || text[found - before] == b'-'
}

const fn is_name_byte(byte: u8) -> bool {
   byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+')
}

/// Where the path holding the name at `start` begins.
fn path_start(text: &str, start: usize) -> usize {
   start
      - text[..start]
         .bytes()
         .rev()
         .take_while(|byte| is_name_byte(*byte) || *byte == b'/')
         .count()
}

/// Whether the ancestor with pid `run` could have made `path`, which a file
/// already there before it started, as rhash's has160.c beside a make whose
/// pid repeats as 160 in every sandbox, rules out.
fn made_by(path: &str, run: &str) -> bool {
   let Some(&started) = ANCESTORS.get(run) else {
      return false;
   };
   let earlier = started
      .checked_sub(Duration::from_secs(1))
      .unwrap_or(UNIX_EPOCH);
   metadata(path)
      .and_then(|found| found.modified())
      .ok()
      .is_none_or(|modified| modified >= earlier)
}

/// gnulib's tests/init.sh runs each test in `gt-<test>.XXXX` under the
/// directory the test started in, made by mktemp.
fn gnulib_names(text: &str) -> Vec<(Range<usize>, String)> {
   let mut names = Vec::new();
   for (found, _) in text.match_indices("gt-") {
      if found > 0 && text.as_bytes()[found - 1] != b'/' {
         continue;
      }

      let end = text[found..]
         .find('/')
         .map_or(text.len(), |length| found + length);
      let Some(dot) = text[found..end].rfind('.').map(|offset| found + offset) else {
         continue;
      };

      if end - dot == 5 && random_tail(text, dot + 1, 4) {
         names.push((found..end, format!("{}####", &text[found..=dot])));
      }
   }

   names
}

/// Names a process of this build made from its pid, gen$$.c and the like:
/// a digit run, three digits or more, that is the pid of this process or
/// one of its ancestors.
fn pid_names(text: &str) -> Vec<(Range<usize>, String)> {
   let mut names = Vec::new();
   let bytes = text.as_bytes();
   let mut index = 0;
   while index < bytes.len() {
      let digits = bytes[index..]
         .iter()
         .take_while(|byte| byte.is_ascii_digit())
         .count();
      if digits == 0 {
         index += 1;
         continue;
      }
      let run = &text[index..index + digits];
      let start = text[..index]
         .bytes()
         .rev()
         .take_while(|byte| is_name_byte(*byte))
         .count();
      let end = text[index + digits..]
         .bytes()
         .take_while(|byte| is_name_byte(*byte))
         .count();
      let name = index - start..index + digits + end;
      let chunk = index
         - text[..index]
            .bytes()
            .rev()
            .take_while(u8::is_ascii_alphanumeric)
            .count()
         ..index
            + digits
            + text[index + digits..]
               .bytes()
               .take_while(u8::is_ascii_alphanumeric)
               .count();
      let hashed = chunk.len() >= 8 && text[chunk].bytes().all(|byte| byte.is_ascii_hexdigit());
      // only a file's stem ending in the pid, as gen$$.c, since sandbox pids
      // are small and new for every call and would match version numbers,
      // flag values and digests
      let extension = text[index + digits..name.end]
         .strip_prefix('.')
         .and_then(|rest| rest.split('.').next())
         .is_some_and(|part| {
            !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_alphabetic())
         });
      if digits >= 3
         && extension
         && !hashed
         && made_by(&text[path_start(text, name.start)..name.end], run)
      {
         let spelling = format!(
            "{}#{}",
            &text[name.start..index],
            &text[index + digits..name.end]
         );
         names.push((name, spelling));
      }
      index += digits;
   }
   names
}

/// A name with its random runs, letters, digits and `_` holding a digit or
/// both cases in six or more, as `#`.
fn mask_runs(name: &str) -> String {
   let mut masked = String::with_capacity(name.len());
   let mut rest = name;
   while !rest.is_empty() {
      let run = rest
         .bytes()
         .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
         .count();
      if run == 0 {
         let other = rest
            .bytes()
            .take_while(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
            .count();
         masked.push_str(&rest[..other]);
         rest = &rest[other..];
         continue;
      }
      let word = &rest[..run];
      let random = word.bytes().any(|byte| byte.is_ascii_digit())
         || run >= 6
            && word.bytes().any(|byte| byte.is_ascii_uppercase())
            && word.bytes().any(|byte| byte.is_ascii_lowercase());
      masked.push_str(if random { "#" } else { word });
      rest = &rest[run..];
   }
   masked
}

#[inline]
#[must_use]
pub fn mask_random_names(text: &str) -> String {
   let mut masked = String::with_capacity(text.len());
   let mut copied = 0;
   for (span, spelling) in random_names(text) {
      if span.start < copied {
         continue;
      }
      masked.push_str(&text[copied..span.start]);
      masked.push_str(&spelling);
      copied = span.end;
   }
   masked.push_str(&text[copied..]);
   masked
}

#[inline]
#[must_use]
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

#[inline]
#[must_use]
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

/// An object built in a scratch dir carries the dir's name in its debug
/// info, as GHC's link stubs and rustdoc's doctests do, and a replayed one
/// carries the name from the run that stored it, so both mask alike.
fn mask_scratch_bytes(data: &[u8], temporary: &str) -> Vec<u8> {
   let root = format!("{temporary}/");
   let mut masked = Vec::with_capacity(data.len());
   let mut rest = data;
   while let Some(position) = find(rest, root.as_bytes()) {
      let length = rest[position..]
         .iter()
         .take_while(|byte| is_name_byte(**byte) || **byte == b'/')
         .count();
      let path = str::from_utf8(&rest[position..position + length]).unwrap_or_default();
      masked.extend_from_slice(&rest[..position]);
      let unbounded = scratch_root_at(rest, position);
      match scratch_names(path, temporary).first() {
         Some(found) if unbounded && found.0.start == 0 => {
            masked.extend_from_slice(found.1.as_bytes());
            rest = &rest[position + found.0.end..];
         },
         _ => {
            masked.extend_from_slice(root.as_bytes());
            rest = &rest[position + root.len()..];
         },
      }
   }
   masked.extend_from_slice(rest);
   masked
}
