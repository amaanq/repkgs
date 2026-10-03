use std::{
   fs::{
      create_dir_all,
      remove_dir_all,
      rename,
   },
   io,
   path::Path,
   process::{
      Command,
      Stdio,
   },
};

use shim::files::write_file;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      cc_facts,
      dirs,
      run_in,
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
      let src = stage.src()?;
      let tmp = format!("{}/x", stage.build_top);
      create_dir_all(&tmp)?;
      let tmp_path = Path::new(&tmp);
      run_in(tmp_path, "bsdtar", &["-xf", src, "Payload"])?;
      write_file(&tmp_path.join("pbzx.py"), PBZX.as_bytes(), None)?;
      let mut python = Command::new("python3")
         .args(["pbzx.py", "Payload"])
         .current_dir(tmp_path)
         .stdout(Stdio::piped())
         .spawn()?;
      let stdout = python
         .stdout
         .take()
         .ok_or_else(|| io::Error::other("python3 has no stdout"))?;
      let tar = Command::new("bsdtar")
         .args(["-xf", "-", "*/SDKs/MacOSX*.*.sdk/*"])
         .current_dir(tmp_path)
         .stdin(stdout)
         .status()?;
      if !python.wait()?.success() || !tar.success() {
         return Err(io::Error::other("unpacking the SDK payload failed"));
      }
      let sdk = dirs(&format!(
         "{tmp}/Library/Developer/CommandLineTools/SDKs/MacOSX*.*.sdk"
      ))?
      .into_iter()
      .next()
      .ok_or_else(|| io::Error::other("no MacOSX SDK in the payload"))?;
      say(&format!(
         "{} -> {}",
         sdk.file_name().unwrap_or_default().to_string_lossy(),
         stage.out()
      ));
      for junk in ["usr/share", "usr/bin", "System/Library/Perl"] {
         let path = sdk.join(junk);
         if path.exists() {
            remove_dir_all(path)?;
         }
      }
      rename(&sdk, stage.out())?;
      cc_facts(stage.out(), &[
         ("include-dirs", &["usr/include"]),
         ("flags", &FLAGS),
         ("cxxflags", &[]),
      ])
   }
}
