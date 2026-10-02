use core.nu *

# std for these is freestanding, so it ships with the compiler as nixpkgs does
const freestanding = [wasm32-unknown-unknown wasm32v1-none bpfel-unknown-none bpfeb-unknown-none]

def machine []: nothing -> string {
  ^rustc -vV | lines | parse "host: {triple}" | get triple.0
}

# shebang fixing edits vendored files, and cargo checks every file sum it finds
def drop-file-sums []: nothing -> nothing {
  for sums in (files vendor/*/.cargo-checksum.json) {
    open $sums | merge {files: {}} | to json -r | save -f $sums
  }
}

# stage tools and build scripts run on this machine, so a cross build names its compilers too
def this-machine [c: record, extra?: closure]: nothing -> record {
  if not $c.platform.cross { return {} }
  let cc = (tool $env.CC_FOR_BUILD)
  {(machine): ({cc: $cc, cxx: (tool $env.CXX_FOR_BUILD), linker: $cc, ar: (tool llvm-ar)} | merge (if $extra == null { {} } else { do $extra }))}
}

def write-config [c: record, build: record, rust: closure, rest: record]: nothing -> nothing {
  drop-file-sums
  let stage0 = (tool rustc | path dirname -n 2)
  let level = if $c.spec.debug { 1 } else { 0 }
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
  let c = (ctx)
  load-env ({PKG_CONFIG_ALLOW_CROSS: "1"} | merge (^jib system sys-env cargo | from json))
  let triple = $c.platform.rustTriple
  let llvm = (dep-root llvm22 libLLVM)
  # a cross llvm's bin/llvm-config is a target program, llvm puts one for this machine in host/
  let llvm_config = if $c.platform.cross { $"($llvm)/host/llvm-config" } else { $"($llvm)/bin/llvm-config" }
  let own = {cc: (tool cc), cxx: (tool c++), linker: (tool cc), ar: (tool ar), ranlib: (tool ranlib), llvm-config: $llvm_config, crt-static: false}
  # rust#132802, optimized wasm builtins need a wasm C compiler
  let bare = ($freestanding | each {|triple| {$triple: {optimized-compiler-builtins: false, profiler: false}} } | into record)

  write-config $c {
    host: [$triple]
    target: ([$triple] ++ $freestanding)
    extended: true
    tools: [cargo clippy rustfmt rustdoc rust-analyzer-proc-macro-srv]
    description: repkgs
  } {|level|
    {debuginfo-level: $level, llvm-bitcode-linker: false, codegen-backends: [llvm], codegen-tests: false}
  } {
    # without use-libcxx rustc_llvm links -lstdc++, which this toolchain lacks
    llvm: {link-shared: true, download-ci-llvm: false, use-libcxx: true}
    target: ({$triple: $own} | merge (this-machine $c {|| {llvm-config: $"(tool-root llvm22)/bin/llvm-config"} }) | merge $bare)
    dist: {include-mingw-linker: false}
  }
}

export def build []: nothing -> nothing { x python3 x.py build --stage 2 }

export def install []: nothing -> nothing {
  let out = (ctx).out
  x python3 x.py install
  rm -f ...(files $"($out)/lib/rustlib/{install.log,uninstall.sh,manifest-*,components,rust-installer-version}")
}

export def std-configure []: nothing -> nothing {
  let c = (ctx)
  let own = {cc: (tool cc), cxx: (tool c++), linker: (tool cc), ar: (tool llvm-ar), ranlib: (tool llvm-ranlib), crt-static: false}

  write-config $c {
    host: []
    target: [$c.platform.rustTriple]
    local-rebuild: true
  } {|level| {debuginfo-level-std: $level} } {
    llvm: {download-ci-llvm: false}
    target: ({$c.platform.rustTriple: $own} | merge (this-machine $c))
  }
}

export def std-build []: nothing -> nothing { x python3 x.py build --stage 0 library }

# x.py install has no stage 0 path, so std is copied out of the stage 0 build. bootstrap fills the
# stage 0 sysroot from cargo's old target/deps layout only, which leaves just self-contained/ there
export def std-install []: nothing -> nothing {
  let c = (ctx)
  let triple = $c.platform.rustTriple
  let stage0 = $"($c.build)/(machine)"
  let lib = $"($c.out)/lib/rustlib/($triple)/lib"
  mkdir ($lib | path dirname)
  cp -r $"($stage0)/stage0-sysroot/lib/rustlib/($triple)/lib" $lib
  for built in (files $"($stage0)/stage0-std/($triple)/dist/build/*/*/out/*.{rlib,rmeta,so}") { cp $built $"($lib)/" }
  if (files $"($lib)/*.rlib" | is-empty) { error make {msg: $"rust.std-install: no ($triple) rlibs"} }
}
