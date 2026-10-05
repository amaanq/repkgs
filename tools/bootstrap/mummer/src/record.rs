use std::{
   io,
   slice::from_ref,
};

use base64::{
   Engine as _,
   engine::general_purpose::STANDARD,
};
use mummer::files::relative_path;
use serde::{
   Deserialize,
   Deserializer,
   Serialize,
   Serializer,
   de::Error as _,
};
use tightrope::{
   client::Client,
   digest::Digest,
   key::{
      Key,
      Namespace,
   },
};

use crate::manifest::Miss;

/// Outputs up to this size ride inside the record, saving a round trip on a
/// hit, larger ones are blobs shared by every record that names them.
const INLINE: usize = 64 << 10;

/// One compile's result, everything a hit replays.
pub struct Record {
   pub status:      i32,
   pub diagnostics: Vec<u8>,
   pub depfile:     Option<String>,
   pub link_deps:   Option<String>,
   pub artifacts:   Vec<Artifact>,
}

pub struct Artifact {
   pub name:   String,
   pub mode:   Option<u32>,
   pub bytes:  Vec<u8>,
   /// A link output as the linker wrote it, its RUNPATH settled on replay.
   pub settle: bool,
}

struct Bytes(Vec<u8>);

impl Serialize for Bytes {
   fn serialize<Target>(&self, serializer: Target) -> Result<Target::Ok, Target::Error>
   where
      Target: Serializer,
   {
      serializer.serialize_str(&STANDARD.encode(&self.0))
   }
}

impl<'de> Deserialize<'de> for Bytes {
   fn deserialize<Source>(deserializer: Source) -> Result<Self, Source::Error>
   where
      Source: Deserializer<'de>,
   {
      let text = String::deserialize(deserializer)?;
      STANDARD
         .decode(text)
         .map(Self)
         .map_err(Source::Error::custom)
   }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Content {
   Inline(Bytes),
   Blob(Digest),
}

#[derive(Deserialize, Serialize)]
struct StoredArtifact {
   name:    String,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   mode:    Option<u32>,
   content: Content,
   #[serde(default, skip_serializing_if = "is_false")]
   settle:  bool,
}

#[derive(Deserialize, Serialize)]
struct Stored {
   status:      i32,
   diagnostics: Bytes,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   depfile:     Option<String>,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   link_deps:   Option<String>,
   artifacts:   Vec<StoredArtifact>,
}

#[expect(
   clippy::trivially_copy_pass_by_ref,
   reason = "serde's skip_serializing_if passes a reference"
)]
const fn is_false(value: &bool) -> bool {
   !*value
}

impl Record {
   pub fn put(self, cache: &mut Client, result: &Digest) -> io::Result<()> {
      let mut artifacts = Vec::with_capacity(self.artifacts.len());
      for artifact in self.artifacts {
         let content = if artifact.bytes.len() <= INLINE {
            Content::Inline(Bytes(artifact.bytes))
         } else {
            let hash = Digest::of(&artifact.bytes);
            let key = Key::digest(Namespace::Blob, &hash);
            if !cache.has_many(from_ref(&key))?[0] {
               cache.put(&key, &artifact.bytes)?;
            }
            Content::Blob(hash)
         };
         artifacts.push(StoredArtifact {
            name: artifact.name,
            mode: artifact.mode,
            content,
            settle: artifact.settle,
         });
      }
      let stored = Stored {
         status: self.status,
         diagnostics: Bytes(self.diagnostics),
         depfile: self.depfile,
         link_deps: self.link_deps,
         artifacts,
      };
      cache.put(
         &Key::digest(Namespace::Record, result),
         &serde_json::to_vec(&stored)?,
      )
   }

   pub fn get(cache: &mut Client, result: &Digest) -> Result<Self, Miss> {
      let Some(json) = cache.get(&Key::digest(Namespace::Record, result))? else {
         return Err(Miss::ObjectGone);
      };
      let stored = serde_json::from_slice::<Stored>(&json).map_err(io::Error::from)?;
      let blobs = stored
         .artifacts
         .iter()
         .filter_map(|artifact| {
            match artifact.content {
               Content::Blob(ref hash) => Some(Key::digest(Namespace::Blob, hash)),
               Content::Inline(_) => None,
            }
         })
         .collect::<Vec<_>>();
      let mut fetched = cache.get_many(&blobs)?.into_iter();
      let mut artifacts = Vec::with_capacity(stored.artifacts.len());
      for artifact in stored.artifacts {
         relative_path(&artifact.name)?;
         if artifact.mode.is_some_and(|mode| mode > 0o777) {
            return Err(Miss::Failed(io::Error::other(
               "cached artifact has an invalid mode",
            )));
         }
         let bytes = match artifact.content {
            Content::Inline(Bytes(inline)) => inline,
            Content::Blob(hash) => {
               let Some(Some(blob)) = fetched.next() else {
                  return Err(Miss::ObjectGone);
               };
               if Digest::of(&blob) != hash {
                  return Err(Miss::Failed(io::Error::other(
                     "cached artifact content hash differs",
                  )));
               }
               blob
            },
         };
         artifacts.push(Artifact {
            name: artifact.name,
            mode: artifact.mode,
            bytes,
            settle: artifact.settle,
         });
      }
      Ok(Self {
         status: stored.status,
         diagnostics: stored.diagnostics.0,
         depfile: stored.depfile,
         link_deps: stored.link_deps,
         artifacts,
      })
   }
}
