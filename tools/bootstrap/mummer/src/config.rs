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
}

/// How an ELF toolchain links executables.
#[derive(Deserialize, Serialize)]
pub struct ElfPolicy {
   pub libc:   String,
   pub interp: String,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub crt:    Option<String>,
}
