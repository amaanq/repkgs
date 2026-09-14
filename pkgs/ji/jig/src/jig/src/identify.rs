use std::{
   fs::{
      metadata,
      read,
   },
   io::{
      self,
      Write as _,
   },
   path::Path,
};

use blake3::Hasher;
use serde_json::{
   Map,
   Value,
};
use wire::client::Client;

use crate::{
   elf::{
      is_elf,
      names,
      sections,
   },
   image::Image,
   store::field,
   tree::{
      parallel,
      walk,
   },
};

// store paths never change, so an identity computed once is good for the life
// of the daemon
pub fn run(roots: &[String], socket: &Path) -> io::Result<()> {
   let mut client = Client::connect(socket).ok();
   let mut found = Map::new();
   for root in roots {
      let key = format!("identity/v3/{root}");
      let known = client
         .as_mut()
         .map(|daemon| daemon.get(&key))
         .transpose()?
         .flatten();
      let identity = if let Some(bytes) = known {
         String::from_utf8(bytes).map_err(io::Error::other)?
      } else {
         let computed = identity(Path::new(root))?;
         if let Some(daemon) = client.as_mut() {
            daemon.put(&key, computed.as_bytes())?;
         }
         computed
      };
      found.insert(root.clone(), Value::String(identity));
   }
   let mut stdout = io::stdout().lock();
   serde_json::to_writer(&mut stdout, &found)?;
   writeln!(stdout)?;
   Ok(())
}

fn identity(root: &Path) -> io::Result<String> {
   let mut hasher = Hasher::new();
   if metadata(root)?.is_file() {
      field(&mut hasher, masked(read(root)?));
      return Ok(hasher.finalize().to_hex()[..32].to_owned());
   }
   let entries = walk(root)?;
   let contents = parallel(&entries, |entry| {
      Ok(if entry.kind == 'f' {
         blake3::hash(&masked(read(&entry.path)?))
            .to_hex()
            .to_string()
      } else {
         String::from_utf8_lossy(&masked(entry.target.clone().into_bytes())).into_owned()
      })
   })?;
   for (entry, content) in entries.iter().zip(contents) {
      field(&mut hasher, &entry.rel);
      field(&mut hasher, entry.kind.to_string());
      field(&mut hasher, content);
   }
   Ok(hasher.finalize().to_hex()[..32].to_owned())
}

// relocated outputs name their neighbours by store hash, in RUNPATHs, launch
// records and scripts, and a build-id is a hash over exactly those bytes
fn masked(bytes: Vec<u8>) -> Vec<u8> {
   let mut image = Image { bytes };
   if is_elf(&image)
      && let Ok(found) = sections(&image)
      && let Ok(titles) = names(&image, &found)
   {
      for (section, title) in found.iter().zip(titles) {
         if title == ".note.gnu.build-id" || title == ".gnu_debuglink" {
            image.bytes[section.offset..section.offset + section.size].fill(0);
         }
      }
   }
   masked_hashes(image.bytes)
}

fn masked_hashes(mut bytes: Vec<u8>) -> Vec<u8> {
   let mut at = 0;
   while let Some(offset) = bytes[at..].iter().position(|byte| *byte == b'/') {
      let start = at + offset + 1;
      let end = start + 32;
      if bytes.get(end) == Some(&b'-')
         && bytes[start..end]
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'd' | b'f'..=b'n' | b'p'..=b's' | b'v'..=b'z'))
      {
         bytes[start..end].fill(b'*');
         at = end;
      } else {
         at = start;
      }
   }
   bytes
}
