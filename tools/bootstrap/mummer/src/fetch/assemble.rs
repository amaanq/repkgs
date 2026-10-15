use std::{
   collections::BTreeMap,
   fs::{
      create_dir_all,
      read,
   },
   io,
   os::unix::fs::symlink,
   path::Path,
   process::Command,
};

use mummer::{
   attrs,
   files::write_file,
   process::Checked as _,
   store_path::StorePath,
   tree::parallel_with,
   unpack::Bsdtar,
};
use serde::Deserialize;

use crate::{
   bootstrap::stage::cores,
   fetch::drv::{
      Entry,
      Kind,
      Member,
   },
};

#[derive(Deserialize)]
pub struct Attrs {
   pub outputs: BTreeMap<String, String>,
   pub peanut:  StorePath,
   pub layout:  Vec<Entry>,
}

impl Attrs {
   pub fn out(&self) -> io::Result<&str> {
      self
         .outputs
         .get("out")
         .map(String::as_str)
         .ok_or_else(|| io::Error::other("no out output"))
   }
}

fn place(entry: &Entry, out: &str, tar: &Bsdtar) -> io::Result<()> {
   let target = format!("{out}/{}", entry.to);
   let dst = Path::new(&target);
   match entry.kind {
      Kind::Link { ref link } => {
         create_dir_all(dst.parent().unwrap_or(dst))?;
         symlink(link, dst)
      },
      Kind::Unpack { ref unpack } => {
         create_dir_all(dst)?;
         tar.extract(Path::new(unpack), dst, &[
            "--strip-components",
            "1",
            "--no-same-owner",
            "--no-same-permissions",
         ])
      },
      Kind::Write { ref write } => {
         create_dir_all(dst.parent().unwrap_or(dst))?;
         write_file(dst, write.as_bytes(), None)
      },
      Kind::Copy {
         ref copy,
         ref append,
      } => {
         create_dir_all(dst.parent().unwrap_or(dst))?;
         let mut bytes = read(copy)?;
         bytes.extend_from_slice(append.as_bytes());
         write_file(dst, &bytes, None)
      },
      Kind::Zip { ref zip } => {
         create_dir_all(dst.parent().unwrap_or(dst))?;
         write_file(dst, &stored_zip(zip)?, None)
      },
   }
}

fn crc32(bytes: &[u8]) -> u32 {
   let table = (0..256_u32)
      .map(|seed| {
         (0..8_u8).fold(seed, |crc, _| {
            if crc & 1_u32 == 1_u32 {
               (crc >> 1_u32) ^ 0xEDB8_8320_u32
            } else {
               crc >> 1_u32
            }
         })
      })
      .collect::<Vec<u32>>();
   !bytes.iter().fold(u32::MAX, |crc, &byte| {
      table[usize::from(crc.to_le_bytes()[0] ^ byte)] ^ (crc >> 8_u32)
   })
}

/// `members` as an uncompressed zip, every timestamp 1980-01-01 so the bytes
/// depend on the members alone.
fn stored_zip(members: &[Member]) -> io::Result<Vec<u8>> {
   let narrow = |value: usize| {
      u32::try_from(value).map_err(|_overflow| io::Error::other("zip member over 4 GiB"))
   };
   let short = |value: usize| {
      u16::try_from(value).map_err(|_overflow| io::Error::other("zip name over 64 KiB"))
   };
   let mut archive = Vec::new();
   let mut directory = Vec::new();
   for member in members {
      let data = read(&member.path)?;
      let crc = crc32(&data);
      let size = narrow(data.len())?;
      let name = member.name.as_bytes();
      let offset = narrow(archive.len())?;
      let fields = |record: &mut Vec<u8>| {
         record.extend_from_slice(&0_u16.to_le_bytes());
         record.extend_from_slice(&0_u16.to_le_bytes());
         record.extend_from_slice(&0_u16.to_le_bytes());
         record.extend_from_slice(&0x21_u16.to_le_bytes());
         record.extend_from_slice(&crc.to_le_bytes());
         record.extend_from_slice(&size.to_le_bytes());
         record.extend_from_slice(&size.to_le_bytes());
      };
      archive.extend_from_slice(&0x0403_4B50_u32.to_le_bytes());
      archive.extend_from_slice(&10_u16.to_le_bytes());
      fields(&mut archive);
      archive.extend_from_slice(&short(name.len())?.to_le_bytes());
      archive.extend_from_slice(&0_u16.to_le_bytes());
      archive.extend_from_slice(name);
      archive.extend_from_slice(&data);
      directory.extend_from_slice(&0x0201_4B50_u32.to_le_bytes());
      directory.extend_from_slice(&0x030A_u16.to_le_bytes());
      directory.extend_from_slice(&10_u16.to_le_bytes());
      fields(&mut directory);
      directory.extend_from_slice(&short(name.len())?.to_le_bytes());
      directory.extend_from_slice(&[0; 8]);
      directory.extend_from_slice(&(0o100_644_u32 << 16).to_le_bytes());
      directory.extend_from_slice(&offset.to_le_bytes());
      directory.extend_from_slice(name);
   }
   let count = short(members.len())?;
   let start = narrow(archive.len())?;
   let length = narrow(directory.len())?;
   archive.extend_from_slice(&directory);
   archive.extend_from_slice(&0x0605_4B50_u32.to_le_bytes());
   archive.extend_from_slice(&[0; 4]);
   archive.extend_from_slice(&count.to_le_bytes());
   archive.extend_from_slice(&count.to_le_bytes());
   archive.extend_from_slice(&length.to_le_bytes());
   archive.extend_from_slice(&start.to_le_bytes());
   archive.extend_from_slice(&0_u16.to_le_bytes());
   Ok(archive)
}

/// lay the collecting derivation's `layout` out under $out, then drop the
/// write bits the daemon rejects.
pub fn run_mode() -> io::Result<()> {
   let attrs = attrs::load::<Attrs>()?;
   let out = attrs.out()?;
   let tar = Bsdtar::new(attrs.peanut.join("bin/bsdtar"));
   parallel_with(&attrs.layout, cores(), |entry| place(entry, out, &tar))?;
   Command::new(attrs.peanut.join("bin/chmod"))
      .args(["-R", "u+w,go-w,a-st", out])
      .checked()
}
