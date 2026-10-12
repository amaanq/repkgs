use std::{
   fs::{
      create_dir_all,
      remove_dir_all,
   },
   io,
   process::{
      Command,
      Stdio,
   },
};

use mummer::{
   files::write_file,
   tree::copy_tree,
};

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      cc_facts,
      dirs,
      execute,
      say,
   },
};

/// -isysroot, libSystem, libc++ and the frameworks as .tbd stubs. The
/// deployment target rides in the triple, ld64.lld ad-hoc signs arm64 output
/// itself. -stdlib for both languages, so cxxflags is empty.
const FLAGS: [&str; 6] = [
   "-isysroot",
   "SYSROOT",
   "-resource-dir=SYSROOT/lib/clang",
   "-rtlib=compiler-rt",
   "-stdlib=libc++",
   "--ld-path=LLD",
];

/// pbzx framing, the magic, u64 flags, then {u64 flags, u64 size, xz stream}
/// while flags bit 24 is set. The xz streams concatenated are one valid .xz.
const PBZX: &str = "
import struct, sys
f, o = open(sys.argv[1], 'rb'), sys.stdout.buffer
assert f.read(4) == b'pbzx'
flags, = struct.unpack('>Q', f.read(8))
while flags & (1 << 24):
    flags, n = struct.unpack('>QQ', f.read(16))
    o.write(f.read(n))
";

pub struct AppleSdk;

impl Recipe for AppleSdk {
   fn name(&self) -> &'static str {
      "apple-sdk"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let tmp = stage.build_top.join("x");
      create_dir_all(&tmp)?;
      execute(
         Command::new("bsdtar")
            .arg("-xf")
            .arg(stage.src()?)
            .arg("Payload")
            .current_dir(&tmp),
      )?;
      write_file(&tmp.join("pbzx.py"), PBZX.as_bytes(), None)?;
      let mut python = Command::new("python3")
         .args(["pbzx.py", "Payload"])
         .current_dir(&tmp)
         .stdout(Stdio::piped())
         .spawn()?;
      let stdout = python
         .stdout
         .take()
         .ok_or_else(|| io::Error::other("python3 has no stdout"))?;
      let tar = Command::new("bsdtar")
         .args(["-xf", "-", "*/SDKs/MacOSX*.*.sdk/*"])
         .current_dir(&tmp)
         .stdin(stdout)
         .status()?;
      if !python.wait()?.success() || !tar.success() {
         return Err(io::Error::other("unpacking the SDK payload failed"));
      }
      let sdk = dirs(&format!(
         "{}/Library/Developer/CommandLineTools/SDKs/MacOSX*.*.sdk",
         tmp.display()
      ))?
      .into_iter()
      .next()
      .ok_or_else(|| io::Error::other("no MacOSX SDK in the payload"))?;
      say(&format!(
         "{} -> {}",
         sdk.file_name().unwrap_or_default().display(),
         stage.out.display()
      ));
      for junk in ["usr/share", "usr/bin", "System/Library/Perl"] {
         let path = sdk.join(junk);
         if path.exists() {
            remove_dir_all(path)?;
         }
      }
      // /build and the store are separate filesystems
      copy_tree(&sdk, &stage.out)?;
      cc_facts(&stage.out, &[
         ("include-dirs", &["usr/include"]),
         ("flags", &FLAGS),
         ("cxxflags", &[]),
      ])
   }
}
