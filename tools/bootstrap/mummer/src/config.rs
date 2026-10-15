use std::{
   collections::BTreeMap,
   env::var,
   io,
};

use serde::{
   Deserialize,
   Serialize,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BinaryFormat {
   #[default]
   Elf,
   MachO,
   Coff,
}

/// A toolchain's `etc/mummer.json`, which its bootstrap stage writes and
/// every wrapper in it reads.
#[derive(Deserialize, Serialize)]
pub struct Config {
   pub cc:         String,
   pub binfmt:     BinaryFormat,
   #[serde(default)]
   pub flags:      Vec<String>,
   #[serde(default)]
   pub cxxflags:   Vec<String>,
   #[serde(default, rename = "prefix-map")]
   pub prefix_map: Vec<String>,
   #[serde(flatten, skip_serializing_if = "Option::is_none")]
   pub policy:     Option<ElfPolicy>,
   /// The Fortran compiler, which flang-rt adds to a copy of the C one.
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub fc:         Option<String>,
   #[serde(default, skip_serializing_if = "Vec::is_empty")]
   pub fflags:     Vec<String>,
   /// The programs `bin/ld` and its kin run, by the name they are called as.
   #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
   pub tools:      BTreeMap<String, Tool>,
}

/// A program a tool name runs, with the caller's arguments between `before`
/// and `after`.
#[derive(Deserialize, Serialize)]
pub struct Tool {
   pub program: String,
   #[serde(default, skip_serializing_if = "Vec::is_empty")]
   pub before:  Vec<String>,
   #[serde(default, skip_serializing_if = "Vec::is_empty")]
   pub after:   Vec<String>,
}

/// How an ELF toolchain links executables.
#[derive(Deserialize, Serialize)]
pub struct ElfPolicy {
   pub libc:   String,
   pub interp: String,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub crt:    Option<String>,
}

/// What a package adds to the flags of one toolchain.
#[derive(Default, Deserialize, Serialize)]
pub struct PackageFlags {
   #[serde(default)]
   pub cflags:   Vec<String>,
   #[serde(default)]
   pub cxxflags: Vec<String>,
   #[serde(default)]
   pub ldflags:  Vec<String>,
}

/// `PKGS_CC`, the package's flags by the root of the toolchain they are for.
#[inline]
pub fn package_flags() -> io::Result<BTreeMap<String, PackageFlags>> {
   match var("PKGS_CC") {
      Ok(text) => Ok(serde_json::from_str(&text)?),
      Err(_) => Ok(BTreeMap::new()),
   }
}
