//! The remote tier over REAPI gRPC, as `NativeLink` serves it. The action
//! cache maps the sha256 of a key's text to an `ActionResult` holding the
//! value in `stdout_raw`, or naming a CAS blob for a value too large to ride
//! in one message.

use std::{
   fs::File,
   io::{
      self,
      Read as _,
   },
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

use sha2::{
   Digest as _,
   Sha256,
};
use tightrope::key::Key;
use tokio::runtime::{
   Builder,
   Runtime,
};

use crate::{
   remote::{
      grpc::{
         Channel,
         Code,
         tls,
      },
      proto::{
         ActionResult,
         Digest,
         GetActionResultRequest,
         Ignored,
         OutputFile,
         ReadRequest,
         ReadResponse,
         UpdateActionResultRequest,
         WriteRequest,
      },
   },
   warn,
};

const REST: Duration = Duration::from_secs(60);
// tonic, under NativeLink, refuses gRPC messages over 4 MiB unless
// max_decoding_message_size raises it
const INLINE: usize = 1 << 20;
const CHUNK: usize = 1 << 20;

const GET_ACTION_RESULT: &str = "build.bazel.remote.execution.v2.ActionCache/GetActionResult";
const UPDATE_ACTION_RESULT: &str = "build.bazel.remote.execution.v2.ActionCache/UpdateActionResult";
const READ: &str = "google.bytestream.ByteStream/Read";
const WRITE: &str = "google.bytestream.ByteStream/Write";

pub struct Remote {
   runtime:        Runtime,
   url:            String,
   instance:       String,
   channel:        Channel,
   writes:         bool,
   reads_resting:  AtomicU64,
   writes_resting: AtomicU64,
}

impl Remote {
   /// `url` is `grpc://host:port/instance`, or `grpcs://` for TLS checked
   /// against `ca`, presenting `identity`.
   pub fn new(
      url: &str,
      ca: Option<&Path>,
      identity: Option<&Path>,
      writes: bool,
   ) -> io::Result<Self> {
      let (secure, rest) = if let Some(rest) = url.strip_prefix("grpcs://") {
         (true, rest)
      } else if let Some(rest) = url.strip_prefix("grpc://") {
         (false, rest)
      } else {
         return Err(io::Error::other(format!(
            "remote {url} is not a grpc:// or grpcs:// URL"
         )));
      };
      let (authority, instance) = rest.split_once('/').unwrap_or((rest, ""));
      Ok(Self {
         runtime: Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?,
         url: url.to_owned(),
         instance: instance.trim_end_matches('/').to_owned(),
         channel: Channel::new(authority, secure.then(|| tls(ca, identity)).transpose()?),
         writes,
         reads_resting: AtomicU64::new(0),
         writes_resting: AtomicU64::new(0),
      })
   }

   pub fn url(&self) -> &str {
      &self.url
   }

   pub const fn writes(&self) -> bool {
      self.writes
   }

   /// The value under `key`, or None for a miss, a bad blob or a remote that
   /// is resting after a failure.
   pub fn get(&self, key: &Key) -> Option<Vec<u8>> {
      if key.is_local() || now() < self.reads_resting.load(Ordering::Relaxed) {
         return None;
      }
      self
         .runtime
         .block_on(self.fetch(key))
         .unwrap_or_else(|error| {
            self.rest(&self.reads_resting, &format!("GET failed with {error}"));
            None
         })
   }

   /// A small value goes in the entry's `stdout_raw`, a large one stores its
   /// blob before the entry naming it, so no reader finds a missing blob.
   pub fn put(&self, key: &Key, value: &[u8]) {
      if !self.writes || key.is_local() || now() < self.writes_resting.load(Ordering::Relaxed) {
         return;
      }
      if let Err(error) = self.runtime.block_on(self.store(key, value)) {
         self.rest(&self.writes_resting, &format!("PUT failed with {error}"));
      }
   }

   async fn fetch(&self, key: &Key) -> io::Result<Option<Vec<u8>>> {
      let request = GetActionResultRequest {
         instance_name: self.instance.clone(),
         action_digest: key_digest(key),
         inline_stdout: true,
      };
      let (code, replies) = self
         .channel
         .call::<_, ActionResult>(GET_ACTION_RESULT, &[request])
         .await?;
      if !found(GET_ACTION_RESULT, code)? {
         return Ok(None);
      }
      let entry = replies.into_iter().next().unwrap_or_default();
      let Some(digest) = entry
         .output_files
         .into_iter()
         .next()
         .and_then(|output| output.digest)
      else {
         return Ok(Some(entry.stdout_raw));
      };
      let read = ReadRequest {
         resource_name: self.resource(None, &digest),
      };
      let (read_code, chunks) = self.channel.call::<_, ReadResponse>(READ, &[read]).await?;
      if !found(READ, read_code)? {
         return Ok(None);
      }
      let blob = chunks
         .into_iter()
         .flat_map(|reply| reply.data)
         .collect::<Vec<u8>>();
      let whole =
         i64::try_from(blob.len()).ok() == Some(digest.size_bytes) && sha256(&blob) == digest.hash;
      Ok(whole.then_some(blob))
   }

   async fn store(&self, key: &Key, value: &[u8]) -> io::Result<()> {
      let result = if value.len() <= INLINE {
         ActionResult {
            output_files: Vec::new(),
            stdout_raw:   value.to_vec(),
         }
      } else {
         let digest = Digest {
            hash:       sha256(value),
            size_bytes: i64::try_from(value.len()).map_err(io::Error::other)?,
         };
         let resource = self.resource(Some(&upload_id()?), &digest);
         let mut offset = 0;
         let requests = value
            .chunks(CHUNK)
            .map(|chunk| {
               let request = WriteRequest {
                  resource_name: resource.clone(),
                  write_offset:  i64::try_from(offset).unwrap_or(i64::MAX),
                  finish_write:  offset + chunk.len() == value.len(),
                  data:          chunk.to_vec(),
               };
               offset += chunk.len();
               request
            })
            .collect::<Vec<_>>();
         let (written, _) = self.channel.call::<_, Ignored>(WRITE, &requests).await?;
         expect_ok(WRITE, written)?;
         ActionResult {
            output_files: vec![OutputFile {
               path:   "value".to_owned(),
               digest: Some(digest),
            }],
            stdout_raw:   Vec::new(),
         }
      };
      let request = UpdateActionResultRequest {
         instance_name: self.instance.clone(),
         action_digest: key_digest(key),
         action_result: result,
      };
      let (code, _) = self
         .channel
         .call::<_, Ignored>(UPDATE_ACTION_RESULT, &[request])
         .await?;
      expect_ok(UPDATE_ACTION_RESULT, code)
   }

   /// A `ByteStream` resource name, `upload` naming a write's session.
   fn resource(&self, upload: Option<&str>, digest: &Digest) -> String {
      let session = upload.map_or_else(String::new, |id| format!("uploads/{id}/"));
      let blob = format!("{session}blobs/{}/{}", digest.hash, digest.size_bytes);
      if self.instance.is_empty() {
         blob
      } else {
         format!("{}/{blob}", self.instance)
      }
   }

   /// A failing remote is skipped for a while rather than asked once per
   /// compile, reads and writes apart so a refused upload keeps hits coming.
   fn rest(&self, resting: &AtomicU64, why: &str) {
      let until = now() + REST.as_secs();
      if resting.swap(until, Ordering::Relaxed) < now() {
         warn(format_args!(
            "remote cache {}, resting {}s: {why}",
            self.url,
            REST.as_secs()
         ));
      }
   }
}

fn expect_ok(method: &str, code: Code) -> io::Result<()> {
   match code {
      Code::Ok => Ok(()),
      Code::NotFound | Code::Other(_) => {
         Err(io::Error::other(format!(
            "{method} answered gRPC status {code:?}"
         )))
      },
   }
}

/// Whether a call that may answer `NOT_FOUND` found what it asked for.
fn found(method: &str, code: Code) -> io::Result<bool> {
   match code {
      Code::Ok => Ok(true),
      Code::NotFound => Ok(false),
      Code::Other(_) => {
         Err(io::Error::other(format!(
            "{method} answered gRPC status {code:?}"
         )))
      },
   }
}

fn key_digest(key: &Key) -> Digest {
   Digest {
      hash:       sha256(key.as_str().as_bytes()),
      size_bytes: i64::try_from(key.as_str().len()).unwrap_or(i64::MAX),
   }
}

/// A fresh UUID naming a `ByteStream` upload.
fn upload_id() -> io::Result<String> {
   let mut bytes = [0_u8; 16];
   File::open("/dev/urandom")?.read_exact(&mut bytes)?;
   let hex = hex(&bytes);
   Ok(format!(
      "{}-{}-{}-{}-{}",
      &hex[..8],
      &hex[8..12],
      &hex[12..16],
      &hex[16..20],
      &hex[20..]
   ))
}

fn now() -> u64 {
   SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |since| since.as_secs())
}

fn sha256(bytes: &[u8]) -> String {
   hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
   const DIGITS: &[u8; 16] = b"0123456789abcdef";
   let mut text = String::with_capacity(bytes.len() * 2);
   for &byte in bytes {
      text.push(char::from(DIGITS[usize::from(byte >> 4_u8)]));
      text.push(char::from(DIGITS[usize::from(byte & 0x0F)]));
   }
   text
}
