//! Compiles the kernel half when the daemon is built, with the clang that
//! `RINGMASTERD_CLANG` names, else the one on PATH.

use std::{
   env::var,
   io,
   path::PathBuf,
   process::Command,
};

fn main() -> io::Result<()> {
   if var("CARGO_FEATURE_DAEMON").is_err() {
      println!("cargo::rerun-if-changed=build.rs");
      return Ok(());
   }
   println!("cargo::rerun-if-changed=src/trace.bpf.c");
   println!("cargo::rerun-if-env-changed=RINGMASTERD_CLANG");
   let arch = match var("CARGO_CFG_TARGET_ARCH")
      .map_err(io::Error::other)?
      .as_str()
   {
      "x86_64" => "x86",
      "aarch64" => "arm64",
      other => return Err(io::Error::other(format!("no BPF target arch for {other}"))),
   };
   let out = PathBuf::from(var("OUT_DIR").map_err(io::Error::other)?).join("trace.bpf.o");
   let clang = var("RINGMASTERD_CLANG").unwrap_or_else(|_unset| "clang".to_owned());
   let status = Command::new(clang)
      .args([
         "-target",
         "bpf",
         "-O2",
         "-g",
         "-fno-stack-protector",
         "-Wall",
         "-Werror",
         &format!("-D__TARGET_ARCH_{arch}"),
         "-c",
         "src/trace.bpf.c",
         "-o",
      ])
      .arg(&out)
      .status()?;
   if !status.success() {
      return Err(io::Error::other(format!("clang failed with {status}")));
   }
   Ok(())
}
