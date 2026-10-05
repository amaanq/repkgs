use std::{
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   io,
};

use mummer::config::BinaryFormat;
use serde::Deserialize;

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cpu {
   X86_64,
   Aarch64,
   Riscv64,
   Loongarch64,
   Powerpc64le,
}

impl Cpu {
   pub const fn name(self) -> &'static str {
      match self {
         Self::X86_64 => "x86_64",
         Self::Aarch64 => "aarch64",
         Self::Riscv64 => "riscv64",
         Self::Loongarch64 => "loongarch64",
         Self::Powerpc64le => "powerpc64le",
      }
   }
}

impl Display for Cpu {
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      f.write_str(self.name())
   }
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
   Linux,
   Windows,
   Macos,
}

impl Os {
   pub const fn name(self) -> &'static str {
      match self {
         Self::Linux => "linux",
         Self::Windows => "windows",
         Self::Macos => "macos",
      }
   }
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Libc {
   Glibc,
   Musl,
   Msvc,
   Mingw,
   Apple,
}

#[derive(Clone, Copy)]
pub enum MingwMachine {
   X86_64,
   Aarch64,
}

impl MingwMachine {
   pub const fn emulation(self) -> &'static str {
      match self {
         Self::X86_64 => "i386pep",
         Self::Aarch64 => "arm64pe",
      }
   }

   pub const fn bfd(self) -> &'static str {
      match self {
         Self::X86_64 => "pe-x86-64",
         Self::Aarch64 => "pe-aarch64-little",
      }
   }

   pub const fn dlltool(self) -> &'static str {
      match self {
         Self::X86_64 => "i386:x86-64",
         Self::Aarch64 => "arm64",
      }
   }

   const fn cpu(self) -> Cpu {
      match self {
         Self::X86_64 => Cpu::X86_64,
         Self::Aarch64 => Cpu::Aarch64,
      }
   }
}

impl TryFrom<Cpu> for MingwMachine {
   type Error = io::Error;

   fn try_from(cpu: Cpu) -> io::Result<Self> {
      match cpu {
         Cpu::X86_64 => Ok(Self::X86_64),
         Cpu::Aarch64 => Ok(Self::Aarch64),
         Cpu::Riscv64 | Cpu::Loongarch64 | Cpu::Powerpc64le => {
            Err(io::Error::other(format!("no mingw profile for {cpu}")))
         },
      }
   }
}

/// How a target links and names its outputs, from its binary format and libc.
#[derive(Clone, Copy)]
pub enum Profile {
   Elf,
   Macho,
   Msvc,
   Mingw(MingwMachine),
}

impl Profile {
   pub fn of(binfmt: BinaryFormat, libc: Libc, cpu: Cpu) -> io::Result<Self> {
      Ok(match (binfmt, libc) {
         (BinaryFormat::Coff, Libc::Mingw) => Self::Mingw(MingwMachine::try_from(cpu)?),
         (BinaryFormat::Coff, _) => Self::Msvc,
         (BinaryFormat::MachO, _) => Self::Macho,
         (BinaryFormat::Elf, _) => Self::Elf,
      })
   }

   pub const fn posix(self) -> bool {
      matches!(self, Self::Elf | Self::Macho)
   }

   pub const fn pic(self) -> &'static [&'static str] {
      match self {
         Self::Elf | Self::Macho => &["-fPIC"],
         Self::Msvc | Self::Mingw(_) => &[],
      }
   }

   pub const fn exe(self) -> &'static str {
      match self {
         Self::Elf | Self::Macho => "",
         Self::Msvc | Self::Mingw(_) => ".exe",
      }
   }

   pub const fn lld(self) -> &'static str {
      match self {
         Self::Elf | Self::Mingw(_) => "ld.lld",
         Self::Macho => "ld64.lld",
         Self::Msvc => "lld-link",
      }
   }

   pub const fn ld_flavor(self) -> Option<&'static str> {
      match self {
         Self::Elf | Self::Mingw(_) => None,
         Self::Macho => Some("darwin"),
         Self::Msvc => Some("link"),
      }
   }

   /// Where clang looks for the compiler-rt library `name` of `target`.
   pub fn runtime(self, target: &str, name: &str) -> String {
      match self {
         Self::Elf => format!("lib/{target}/libclang_rt.{name}.a"),
         Self::Macho => {
            let prefix = if name == "builtins" {
               String::new()
            } else {
               format!("{name}_")
            };
            format!("lib/darwin/libclang_rt.{prefix}osx.a")
         },
         Self::Msvc => format!("lib/{target}/clang_rt.{name}.lib"),
         Self::Mingw(machine) => format!("lib/windows/libclang_rt.{name}-{}.a", machine.cpu()),
      }
   }
}
