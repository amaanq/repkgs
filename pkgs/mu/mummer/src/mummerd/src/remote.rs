//! The remote tier over Bazel's HTTP cache protocol. A key's value is one CAS
//! blob named by its sha256, and the action cache maps the sha256 of the key
//! text to an `ActionResult` naming that blob, so a server that validates
//! action entries (bazel-remote does by default) takes them as they are.

use std::{
   fs::read_to_string,
   io,
   path::Path,
   sync::atomic::{
      AtomicU64,
      Ordering,
   },
   time::{
      Duration,
      SystemTime,
      UNIX_EPOCH,
   },
};

use base64::{
   Engine as _,
   engine::general_purpose::STANDARD,
};
use sha2::{
   Digest as _,
   Sha256,
};
use tightrope::LOCAL_PREFIX;
use ureq::Agent;

use crate::warn;

const REST: Duration = Duration::from_secs(60);
// values this small ride inside the action entry
const INLINE: usize = 1 << 20;

pub struct Remote {
   agent:         Agent,
   url:           String,
   authorization: Option<String>,
   resting_until: AtomicU64,
}

impl Remote {
   /// `token` holds `user:password` for basic auth, and having one is what
   /// lets this machine write.
   pub fn new(url: &str, token: Option<&Path>) -> io::Result<Self> {
      let authorization = token
         .map(|path| {
            let credentials = read_to_string(path)?;
            io::Result::Ok(format!("Basic {}", STANDARD.encode(credentials.trim())))
         })
         .transpose()?;
      Ok(Self {
         agent: Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .new_agent(),
         url: url.trim_end_matches('/').to_owned(),
         authorization,
         resting_until: AtomicU64::new(0),
      })
   }

   pub fn url(&self) -> &str {
      &self.url
   }

   pub const fn writes(&self) -> bool {
      self.authorization.is_some()
   }

   /// The value under `key`, or None for a miss, a bad blob or a remote that
   /// is resting after a failure.
   pub fn get(&self, key: &str) -> Option<Vec<u8>> {
      if key.starts_with(LOCAL_PREFIX) || now() < self.resting_until.load(Ordering::Relaxed) {
         return None;
      }
      let entry = self.fetch("ac", &sha256(key.as_bytes()))?;
      if let Some(value) = message_field(&entry, 5) {
         return Some(value.to_vec());
      }
      let (hash, size) = blob_of(&entry)?;
      let blob = self.fetch("cas", &hash)?;
      (u64::try_from(blob.len()).ok() == Some(size) && sha256(&blob) == hash).then_some(blob)
   }

   /// A small value goes in the entry's `stdout_raw`, a large one stores its
   /// blob before the entry naming it, so no reader finds a missing blob.
   pub fn put(&self, key: &str, value: &[u8]) {
      if key.starts_with(LOCAL_PREFIX) {
         return;
      }
      if value.len() <= INLINE {
         let mut entry = Vec::new();
         field(5, value, &mut entry);
         self.store("ac", &sha256(key.as_bytes()), &entry);
         return;
      }
      let hash = sha256(value);
      let size = u64::try_from(value.len()).unwrap_or(u64::MAX);
      if self.store("cas", &hash, value) {
         self.store("ac", &sha256(key.as_bytes()), &action_result(&hash, size));
      }
   }

   fn fetch(&self, kind: &str, hash: &str) -> Option<Vec<u8>> {
      let mut request = self.agent.get(format!("{}/{kind}/{hash}", self.url));
      if let Some(ref authorization) = self.authorization {
         request = request.header("Authorization", authorization);
      }
      let result = request.call().and_then(|mut response| {
         let status = response.status().as_u16();
         let body = response
            .body_mut()
            .with_config()
            .limit(u64::MAX)
            .read_to_vec()?;
         Ok((status, body))
      });
      match result {
         Ok((200, body)) => Some(body),
         Ok((404, _)) => None,
         Ok((status, _)) => {
            self.rest(&format!("GET {kind} answered {status}"));
            None
         },
         Err(error) => {
            self.rest(&format!("GET {kind} failed with {error}"));
            None
         },
      }
   }

   fn store(&self, kind: &str, hash: &str, body: &[u8]) -> bool {
      let mut request = self.agent.put(format!("{}/{kind}/{hash}", self.url));
      if let Some(ref authorization) = self.authorization {
         request = request.header("Authorization", authorization);
      }
      match request.send(body) {
         Ok(response) if response.status().is_success() => true,
         Ok(response) => {
            self.rest(&format!(
               "PUT {kind} answered {}",
               response.status().as_u16()
            ));
            false
         },
         Err(error) => {
            self.rest(&format!("PUT {kind} failed with {error}"));
            false
         },
      }
   }

   /// A failing remote is skipped for a while rather than asked once per
   /// compile, and it only ever costs a local miss.
   fn rest(&self, why: &str) {
      let until = now() + REST.as_secs();
      if self.resting_until.swap(until, Ordering::Relaxed) < now() {
         warn(format_args!(
            "remote cache {}, resting {}s: {why}",
            self.url,
            REST.as_secs()
         ));
      }
   }
}

fn now() -> u64 {
   SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |since| since.as_secs())
}

fn sha256(bytes: &[u8]) -> String {
   const HEX: &[u8; 16] = b"0123456789abcdef";
   let mut hex = String::with_capacity(64);
   for byte in Sha256::digest(bytes) {
      hex.push(char::from(HEX[usize::from(byte >> 4_u8)]));
      hex.push(char::from(HEX[usize::from(byte & 0x0F)]));
   }
   hex
}

fn varint(mut value: u64, into: &mut Vec<u8>) {
   while value >= 0x80 {
      into.push(u8::try_from(value & 0x7F).unwrap_or_default() | 0x80);
      value >>= 7_u8;
   }
   into.push(u8::try_from(value).unwrap_or_default());
}

fn field(number: u64, bytes: &[u8], into: &mut Vec<u8>) {
   varint((number << 3_u8) | 2, into);
   varint(u64::try_from(bytes.len()).unwrap_or(u64::MAX), into);
   into.extend_from_slice(bytes);
}

/// `ActionResult { output_files: [OutputFile { path: "value", digest }] }`.
fn action_result(hash: &str, size: u64) -> Vec<u8> {
   let mut digest = Vec::new();
   field(1, hash.as_bytes(), &mut digest);
   varint(2 << 3_u8, &mut digest);
   varint(size, &mut digest);
   let mut output = Vec::new();
   field(1, b"value", &mut output);
   field(2, &digest, &mut output);
   let mut result = Vec::new();
   field(2, &output, &mut result);
   result
}

enum Wire<'message> {
   Int(u64),
   Bytes(&'message [u8]),
}

fn varint_at(bytes: &[u8], position: &mut usize) -> Option<u64> {
   let mut value = 0_u64;
   for shift in (0_u32..64).step_by(7) {
      let byte = *bytes.get(*position)?;
      *position += 1;
      value |= u64::from(byte & 0x7F) << shift;
      if byte & 0x80 == 0 {
         return Some(value);
      }
   }
   None
}

/// A protobuf message's fields, fixed-width ones skipped.
fn decode(message: &[u8]) -> Option<Vec<(u64, Wire<'_>)>> {
   let mut fields = Vec::new();
   let mut position = 0;
   while position < message.len() {
      let tag = varint_at(message, &mut position)?;
      match tag & 7 {
         0 => fields.push((tag >> 3_u8, Wire::Int(varint_at(message, &mut position)?))),
         1 => position += 8,
         2 => {
            let length = usize::try_from(varint_at(message, &mut position)?).ok()?;
            let bytes = message.get(position..position.checked_add(length)?)?;
            position += length;
            fields.push((tag >> 3_u8, Wire::Bytes(bytes)));
         },
         5 => position += 4,
         _ => return None,
      }
   }
   Some(fields)
}

fn message_field(message: &[u8], number: u64) -> Option<&[u8]> {
   decode(message)?.into_iter().find_map(|(found, wire)| {
      match wire {
         Wire::Bytes(bytes) if found == number => Some(bytes),
         Wire::Bytes(_) | Wire::Int(_) => None,
      }
   })
}

/// The digest of the one output file an entry names.
fn blob_of(entry: &[u8]) -> Option<(String, u64)> {
   let digest = message_field(message_field(entry, 2)?, 2)?;
   let mut hash = None;
   let mut size = 0;
   for (number, wire) in decode(digest)? {
      match (number, wire) {
         (1, Wire::Bytes(bytes)) => hash = Some(String::from_utf8(bytes.to_vec()).ok()?),
         (2, Wire::Int(value)) => size = value,
         _ => {},
      }
   }
   Some((hash?, size))
}
