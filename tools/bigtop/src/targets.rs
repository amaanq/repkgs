//! Every platform the set targets, the only place their facts live. `binfmt`
//! decides linker flavour, PIC, crt objects, interp and RUNPATH and whether
//! launchers apply. `march` ends up in every cc invocation, on Linux with the
//! cpu's control-flow hardening. `hardening` is the cpu's verdict on hardening
//! flags it cannot take. `names` is what other ecosystems call the cpu (kernel
//! ARCH=, GOARCH, rust triple prefix, meson `cpu_family`, qemu-user binary,
//! gyp/V8 dest-cpu, apple's clang arch) and `osNames` the same for the os
//! (cmake `CMAKE_SYSTEM_NAME`, meson system and kernel, GOOS, gyp/node
//! --dest-os).

use std::collections::BTreeMap;

use mummer::config::BinaryFormat;
use serde::{
   Deserialize,
   Serialize,
};

use crate::hardening::Hardening;

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Cpu {
   X86_64,
   Aarch64,
   Riscv64,
   Loongarch64,
   Powerpc64le,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Os {
   Linux,
   Windows,
   Macos,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Libc {
   Glibc,
   Musl,
   Msvc,
   Mingw,
   Apple,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Abi {
   Gnu,
   Musl,
   Msvc,
   Apple,
}

#[derive(Clone, Copy)]
enum LinuxLibc {
   Glibc,
   Musl,
}

impl Abi {
   const fn name(self) -> &'static str {
      match self {
         Self::Gnu => "gnu",
         Self::Musl => "musl",
         Self::Msvc => "msvc",
         Self::Apple => "apple",
      }
   }
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Names {
   kernel:  String,
   go:      String,
   rust:    String,
   meson:   String,
   qemu:    String,
   gyp:     String,
   clang:   String,
   openssl: String,
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OsNames {
   cmake:        String,
   meson:        String,
   meson_kernel: String,
   go:           String,
   gyp:          String,
   uname:        String,
}

/// File name pieces per binary format. Versioned shared library names order
/// differently per format, `shared-name` and `link-name` in builder/core.nu.
#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Ext {
   exe:     String,
   shared:  String,
   #[serde(rename = "static")]
   archive: String,
   /// what -l<name> finds for a shared library where that is not the library
   /// itself.
   import:  Option<String>,
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Facts {
   name:           String,
   cpu:            Cpu,
   os:             Os,
   libc:           Libc,
   abi:            Abi,
   posix:          bool,
   bits:           u8,
   endian:         String,
   binfmt:         BinaryFormat,
   ext:            Ext,
   names:          Names,
   os_names:       OsNames,
   march:          Vec<String>,
   hardening:      BTreeMap<Hardening, bool>,
   clang_target:   String,
   gnu_triple:     String,
   rust_triple:    String,
   openssl_target: String,
   interp:         String,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   minos:          Option<String>,
}

fn owned(words: &[&str]) -> Vec<String> {
   words.iter().map(|&word| word.to_owned()).collect()
}

impl Cpu {
   pub const ALL: [Self; 5] = [
      Self::X86_64,
      Self::Aarch64,
      Self::Riscv64,
      Self::Loongarch64,
      Self::Powerpc64le,
   ];

   pub const fn name(self) -> &'static str {
      match self {
         Self::X86_64 => "x86_64",
         Self::Aarch64 => "aarch64",
         Self::Riscv64 => "riscv64",
         Self::Loongarch64 => "loongarch64",
         Self::Powerpc64le => "powerpc64le",
      }
   }

   /// Where another ecosystem names the cpu differently from us.
   fn names(self) -> Names {
      let name = self.name().to_owned();
      let mut names = Names {
         kernel:  name.clone(),
         go:      name.clone(),
         rust:    name.clone(),
         meson:   name.clone(),
         qemu:    name.clone(),
         gyp:     name.clone(),
         clang:   name.clone(),
         openssl: name,
      };

      match self {
         Self::X86_64 => {
            "x86".clone_into(&mut names.kernel);
            "amd64".clone_into(&mut names.go);
            "x64".clone_into(&mut names.gyp);
            "linux-x86_64".clone_into(&mut names.openssl);
         },
         Self::Aarch64 => {
            "arm64".clone_into(&mut names.kernel);
            "arm64".clone_into(&mut names.go);
            "arm64".clone_into(&mut names.gyp);
            // apple triples say arm64
            "arm64".clone_into(&mut names.clang);
            "linux-aarch64".clone_into(&mut names.openssl);
         },
         Self::Riscv64 => {
            "riscv".clone_into(&mut names.kernel);
            "riscv64gc".clone_into(&mut names.rust);
         },
         Self::Loongarch64 => {
            "loongarch".clone_into(&mut names.kernel);
            "loong64".clone_into(&mut names.go);
            "loong64".clone_into(&mut names.gyp);
         },
         Self::Powerpc64le => {
            "powerpc".clone_into(&mut names.kernel);
            "ppc64le".clone_into(&mut names.go);
            "ppc64".clone_into(&mut names.meson);
            "ppc64".clone_into(&mut names.gyp);
            "ppc64le".clone_into(&mut names.qemu);
            "linux-ppc64le".clone_into(&mut names.openssl);
         },
      }

      names
   }

   const fn march(self) -> &'static [&'static str] {
      match self {
         Self::X86_64 => &["-march=x86-64-v3"],
         Self::Aarch64 => &["-march=armv8.2-a+lse"],
         // -mno-relax: lld 21 leaves R_RISCV_IRELATIVE addends unadjusted after relaxation, so
         // ld.so jumps into the middle of memcpy instead of the ifunc resolver (every dynamic
         // program SIGSEGVs)
         Self::Riscv64 => &["-march=rv64gc", "-mabi=lp64d", "-mno-relax"],
         // Loongson 3A5000+ (LA464): the LA64 v1.0 baseline every shipped core has
         Self::Loongarch64 => &["-march=loongarch64", "-mabi=lp64d"],
         // POWER9 and later, little endian, ELFv2, IEEE long double (what current distros ship).
         // clang defaults to the IBM long double unless built with
         // PPC_LINUX_DEFAULT_IEEELONGDOUBLE, and warns about -mabi= unless it finds a glibc >=
         // 2.32 at /lib64/ld64.so.2
         Self::Powerpc64le => {
            &[
               "-mcpu=power9",
               "-mabi=ieeelongdouble",
               "-Wno-unsupported-abi",
            ]
         },
      }
   }

   const fn control_flow(self) -> &'static [&'static str] {
      match self {
         Self::X86_64 => &["-fcf-protection=full"],
         Self::Aarch64 => &["-mbranch-protection=standard"],
         Self::Riscv64 | Self::Loongarch64 | Self::Powerpc64le => &[],
      }
   }

   const fn glibc_interp(self) -> &'static str {
      match self {
         Self::X86_64 => "ld-linux-x86-64.so.2",
         Self::Aarch64 => "ld-linux-aarch64.so.1",
         Self::Riscv64 => "ld-linux-riscv64-lp64d.so.1",
         Self::Loongarch64 => "ld-linux-loongarch-lp64d.so.1",
         Self::Powerpc64le => "ld64.so.2",
      }
   }

   /// riscv64: clang 23.1 SIGSEGVs in prologue/epilogue insertion on frames
   /// over 4 KiB with it. loongarch64: clang has it for x86, arm and riscv
   /// only.
   fn hardening(self) -> BTreeMap<Hardening, bool> {
      match self {
         Self::X86_64 | Self::Aarch64 => BTreeMap::new(),
         Self::Riscv64 | Self::Loongarch64 | Self::Powerpc64le => {
            BTreeMap::from([(Hardening::ZeroCallUsedRegs, false)])
         },
      }
   }
}

fn os_names(os: Os) -> OsNames {
   let (cmake, meson, meson_kernel, go, gyp, uname) = match os {
      Os::Windows => ("Windows", "windows", "nt", "windows", "win", "Windows_NT"),
      Os::Macos => ("Darwin", "darwin", "xnu", "darwin", "mac", "Darwin"),
      Os::Linux => ("Linux", "linux", "linux", "linux", "linux", "Linux"),
   };

   OsNames {
      cmake:        cmake.to_owned(),
      meson:        meson.to_owned(),
      meson_kernel: meson_kernel.to_owned(),
      go:           go.to_owned(),
      gyp:          gyp.to_owned(),
      uname:        uname.to_owned(),
   }
}

fn ext(binfmt: BinaryFormat, libc: Libc) -> Ext {
   let (exe, shared) = match binfmt {
      BinaryFormat::Elf => ("", ".so"),
      BinaryFormat::MachO => ("", ".dylib"),
      BinaryFormat::Coff => (".exe", ".dll"),
   };
   let (archive, import) = match libc {
      Libc::Msvc => (".lib", Some(".lib")),
      Libc::Mingw => (".a", Some(".dll.a")),
      Libc::Glibc | Libc::Musl | Libc::Apple => (".a", None),
   };

   Ext {
      exe:     exe.to_owned(),
      shared:  shared.to_owned(),
      archive: archive.to_owned(),
      import:  import.map(str::to_owned),
   }
}

fn linux(cpu: Cpu, flavour: LinuxLibc) -> Facts {
   let name = cpu.name();
   let names = cpu.names();
   let (libc, abi, interp) = match flavour {
      LinuxLibc::Glibc => (Libc::Glibc, Abi::Gnu, cpu.glibc_interp().to_owned()),
      LinuxLibc::Musl => (Libc::Musl, Abi::Musl, format!("ld-musl-{name}.so.1")),
   };
   let abi_name = abi.name();
   // three spellings of one platform, named by consumer: clang -target,
   // configure --host (config.sub), rustc/cargo. They coincide on linux and
   // diverge on macos (arm64-apple-macos14 vs aarch64-apple-darwin), so
   // there is no plain `triple` to reach for
   let clang_target = format!("{name}-unknown-linux-{abi_name}");
   let rust_triple = format!("{}-unknown-linux-{abi_name}", names.rust);
   let openssl = match cpu {
      Cpu::X86_64 | Cpu::Aarch64 | Cpu::Powerpc64le => names.openssl.clone(),
      // its Configure's own table
      Cpu::Riscv64 | Cpu::Loongarch64 => format!("linux64-{name}"),
   };
   let mut march = owned(cpu.march());
   march.extend(owned(cpu.control_flow()));

   Facts {
      name: format!("{name}-linux"),
      cpu,
      os: Os::Linux,
      libc,
      abi,
      posix: true,
      bits: 64,
      endian: "little".to_owned(),
      binfmt: BinaryFormat::Elf,
      ext: ext(BinaryFormat::Elf, libc),
      names,
      os_names: os_names(Os::Linux),
      march,
      hardening: cpu.hardening(),
      gnu_triple: clang_target.clone(),
      clang_target,
      rust_triple,
      openssl_target: openssl,
      interp,
      minos: None,
   }
}

/// The non-Linux targets. PE and Mach-O find libraries beside the binary or by
/// install name, so there is no interp and finish and launchers have nothing to
/// do. macOS and msvc take libc, C++ library and SDK as given
/// (pkgs/ap/apple-sdk, pkgs/wi/windows-sdk), windows gnu is mingw-w64 on UCRT
/// with our libc++. `abi` tells the two windows apart, `posix` is what packages
/// needing fork, signals and ttys ask for. `minos` is the deployment target.
struct Given<'given> {
   os:      Os,
   libc:    Libc,
   abi:     Abi,
   binfmt:  BinaryFormat,
   name:    String,
   clang:   String,
   gnu:     String,
   rust:    String,
   openssl: &'given str,
}

fn given(cpu: Cpu, facts: Given<'_>, march: Vec<String>, minos: Option<String>) -> Facts {
   Facts {
      name: facts.name,
      cpu,
      os: facts.os,
      libc: facts.libc,
      abi: facts.abi,
      posix: facts.os != Os::Windows,
      bits: 64,
      endian: "little".to_owned(),
      binfmt: facts.binfmt,
      ext: ext(facts.binfmt, facts.libc),
      names: cpu.names(),
      os_names: os_names(facts.os),
      march,
      hardening: BTreeMap::new(),
      clang_target: facts.clang,
      gnu_triple: facts.gnu,
      rust_triple: facts.rust,
      openssl_target: facts.openssl.to_owned(),
      interp: String::new(),
      minos,
   }
}

fn msvc(cpu: Cpu) -> Facts {
   let name = cpu.name();
   let triple = format!("{name}-pc-windows-msvc");
   let facts = Given {
      os:      Os::Windows,
      libc:    Libc::Msvc,
      abi:     Abi::Msvc,
      binfmt:  BinaryFormat::Coff,
      name:    format!("{name}-windows-msvc"),
      clang:   triple.clone(),
      gnu:     triple.clone(),
      rust:    triple,
      openssl: if matches!(cpu, Cpu::Aarch64) {
         "VC-WIN64-CLANGASM-ARM"
      } else {
         "VC-WIN64A"
      },
   };
   given(cpu, facts, owned(cpu.march()), None)
}

fn mingw(cpu: Cpu) -> Facts {
   let name = cpu.name();
   let triple = format!("{name}-w64-mingw32");
   let facts = Given {
      os:      Os::Windows,
      libc:    Libc::Mingw,
      abi:     Abi::Gnu,
      binfmt:  BinaryFormat::Coff,
      name:    format!("{name}-windows-gnu"),
      clang:   triple.clone(),
      gnu:     triple,
      rust:    format!("{name}-pc-windows-gnullvm"),
      openssl: if matches!(cpu, Cpu::Aarch64) {
         "mingwarm64"
      } else {
         "mingw64"
      },
   };
   given(cpu, facts, owned(cpu.march()), None)
}

fn macos(cpu: Cpu) -> Facts {
   let name = cpu.name();
   let minos = "14.0";
   let clang = cpu.names().clang;
   let openssl = format!("darwin64-{clang}");
   let facts = Given {
      os:      Os::Macos,
      libc:    Libc::Apple,
      abi:     Abi::Apple,
      binfmt:  BinaryFormat::MachO,
      name:    format!("{name}-macos"),
      clang:   format!("{clang}-apple-macos{minos}"),
      gnu:     format!("{name}-apple-darwin"),
      rust:    format!("{name}-apple-darwin"),
      openssl: &openssl,
   };
   given(
      cpu,
      facts,
      owned(&["-mcpu=apple-m1"]),
      Some(minos.to_owned()),
   )
}

/// glibc Linux for every cpu, musl Linux for the bootstrap, the rest as listed.
pub fn targets() -> Vec<Facts> {
   let linux_targets = [LinuxLibc::Glibc, LinuxLibc::Musl]
      .into_iter()
      .flat_map(|flavour| Cpu::ALL.map(|cpu| linux(cpu, flavour)));
   let others = [
      msvc(Cpu::X86_64),
      msvc(Cpu::Aarch64),
      mingw(Cpu::X86_64),
      mingw(Cpu::Aarch64),
      macos(Cpu::Aarch64),
   ];
   linux_targets.chain(others).collect()
}
