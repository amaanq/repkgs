use core.nu [act dep build-dep bin cue rewrite glob-files]

# std for these is freestanding, so it ships with the compiler as nixpkgs does
const freestanding = [wasm32-unknown-unknown wasm32v1-none bpfel-unknown-none bpfeb-unknown-none]

def machine []: nothing -> string {
   ^rustc -vV | lines | parse "host: {triple}" | get triple.0
}

# shebang fixing edits vendored files, and cargo checks every file sum it finds
def drop-file-sums []: nothing -> nothing {
   for sums in (glob-files vendor/*/.cargo-checksum.json) {
      let crate = (open $sums)
      $crate | merge {files: {}} | to json -r | save -f $sums
   }
}

# stage tools and build scripts run on this machine, so a cross build names its compilers too
def this-machine [c: record, extra?: closure]: nothing -> record {
   if not $c.platform.cross { return {} }
   let cc = (bin $env.CC_FOR_BUILD)
   {(machine): ({cc: $cc, cxx: (bin $env.CXX_FOR_BUILD), linker: $cc, ar: (bin llvm-ar)} | merge (if $extra == null { {} } else { do $extra }))}
}

def write-config [c: record, build: record, rust: closure, rest: record]: nothing -> nothing {
   drop-file-sums
   let stage0 = (bin rustc | path dirname -n 2)
   let level  = if $c.spec.debug { 1 } else { 0 }
   {
      change-id: ignore
      profile: dist
      build: ({
         build: (machine)
         rustc: $"($stage0)/bin/rustc"
         cargo: $"($stage0)/bin/cargo"
         docs: false
         vendor: true
         locked-deps: true
         build-dir: $c.build
         jobs: $c.njobs
         optimized-compiler-builtins: false
      } | merge $build)
      install: {prefix: $c.out, sysconfdir: etc}
      rust: ({channel: stable, remap-debuginfo: true, frame-pointers: true, lld: false, llvm-tools: false} | merge (do $rust $level))
      dist: ({compression-formats: [gz], src-tarball: false} | merge ($rest.dist? | default {}))
   }
   | merge ($rest | reject -o dist)
   | to toml
   | save -f bootstrap.toml
}

export def --env configure []: nothing -> nothing {
   let c = (act)
   # targets such as wasm32-unknown-unknown name rustup's bundled rust-lld, which this build does not ship
   for spec in (glob-files compiler/rustc_target/src/spec/*/*.rs) { rewrite $spec { str replace --all '"rust-lld"' '"lld"' } }
   load-env ({PKG_CONFIG_ALLOW_CROSS: "1"} | merge (^bigtop system sys-env cargo | from json))
   # x.py puts `-C linker=<cc>` in the bootstrap tool's RUSTFLAGS, and cargo leaves RUSTFLAGS out of
   # -C metadata only when one remaps a path, else every toolchain move renames every crate
   $env.RUSTFLAGS_BOOTSTRAP = $"--remap-path-prefix=($c.build)=/rustc/build"
   let triple = $c.platform.rustTriple
   let llvm   = (dep llvm22 libLLVM)
   # a cross llvm's bin/llvm-config is a target program, llvm puts one for this machine in host/
   let llvm_config = if $c.platform.cross { $"($llvm)/host/llvm-config" } else { $"($llvm)/bin/llvm-config" }
   let own         = {cc: (bin cc), cxx: (bin c++), linker: (bin cc), ar: (bin ar), ranlib: (bin ranlib), llvm-config: $llvm_config, crt-static: false}
   # rust#132802, optimized wasm builtins need a wasm C compiler
   let bare = ($freestanding | each {|triple| {$triple: {optimized-compiler-builtins: false, profiler: false}} } | into record)

   write-config $c {
      host: [$triple]
      target: ([$triple] ++ $freestanding)
      extended: true
      tools: [cargo clippy rustfmt rustdoc rust-analyzer-proc-macro-srv]
      description: motley
   } {|level|
      {debuginfo-level: $level, llvm-bitcode-linker: false, codegen-backends: [llvm], codegen-tests: false}
   } {
      # without use-libcxx rustc_llvm links -lstdc++, which this toolchain lacks
      llvm: {link-shared: true, download-ci-llvm: false, use-libcxx: true}
      target: ({$triple: $own} | merge (this-machine $c {|| {llvm-config: $"(build-dep llvm22)/bin/llvm-config"} }) | merge $bare)
      dist: {include-mingw-linker: false}
   }
}

export def build []: nothing -> nothing { cue python3 x.py build --stage 2 }

export def install []: nothing -> nothing {
   let out = (act).out
   cue python3 x.py install
   rm -f ...(glob-files $"($out)/lib/rustlib/{install.log,uninstall.sh,manifest-*,components,rust-installer-version}")
}

export def std-configure []: nothing -> nothing {
   let c   = (act)
   let own = {cc: (bin cc), cxx: (bin c++), linker: (bin cc), ar: (bin llvm-ar), ranlib: (bin llvm-ranlib), crt-static: false}

   write-config $c {
      host: []
      target: [$c.platform.rustTriple]
      local-rebuild: true
   } {|level| {debuginfo-level-std: $level} } {
      llvm: {download-ci-llvm: false}
      target: ({$c.platform.rustTriple: $own} | merge (this-machine $c))
   }
}

export def std-build []: nothing -> nothing { cue python3 x.py build --stage 0 library }

# x.py install has no stage 0 path, so std is copied out of the stage 0 build. bootstrap fills the
# stage 0 sysroot from cargo's old target/deps layout only, which leaves just self-contained/ there
export def std-install []: nothing -> nothing {
   let c      = (act)
   let triple = $c.platform.rustTriple
   let stage0 = $"($c.build)/(machine)"
   let lib    = $"($c.out)/lib/rustlib/($triple)/lib"
   mkdir ($lib | path dirname)
   cp -r $"($stage0)/stage0-sysroot/lib/rustlib/($triple)/lib" $lib
   for built in (glob-files $"($stage0)/stage0-std/($triple)/dist/build/*/*/out/*.{rlib,rmeta,so}") { cp $built $"($lib)/" }
   if (glob-files $"($lib)/*.rlib" | is-empty) { error make {msg: $"rust.std-install: no ($triple) rlibs"} }
}
