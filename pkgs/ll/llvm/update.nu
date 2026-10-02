# builtins-<target>.txt lists the compiler-rt lib/builtins sources cmake picks for each target, so
# jig bootstrap can compile compiler-rt before there is a cmake

const elf_targets = {
  x86_64: {triple: x86_64-unknown-linux-gnu, system: Linux}
  aarch64: {triple: aarch64-unknown-linux-gnu, system: Linux}
  riscv64: {triple: riscv64-unknown-linux-gnu, system: Linux}
  loongarch64: {triple: loongarch64-unknown-linux-gnu, system: Linux}
  powerpc64le: {triple: powerpc64le-unknown-linux-gnu, system: Linux}
  x86_64-windows-msvc: {triple: x86_64-pc-windows-msvc, system: Windows}
  x86_64-windows-gnu: {triple: x86_64-w64-mingw32, system: Windows}
  aarch64-windows-msvc: {triple: aarch64-pc-windows-msvc, system: Windows}
  aarch64-windows-gnu: {triple: aarch64-w64-mingw32, system: Windows}
}

# compiler-rt configures Apple targets through xcrun and PlistBuddy. Their list is the ELF one
# with the `if(APPLE)` additions of lib/builtins/CMakeLists.txt, minus Darwin-excludes/osx.txt
const apple_targets = {
  aarch64-macos: {
    base: aarch64
    extra: [
      atomic_flag_clear.c atomic_flag_clear_explicit.c atomic_flag_test_and_set.c
      atomic_flag_test_and_set_explicit.c atomic_signal_fence.c atomic_thread_fence.c
    ]
  }
}

def --wrapped with-tools [...command: string]: nothing -> string {
  ^nix-shell -p cmake ninja llvmPackages.clang-unwrapped llvmPackages.lld llvmPackages.llvm --run ($command | str join ' ')
}

def elf-sources [tree: path, name: string, target: record]: nothing -> list<string> {
  let build = $"($tree)/build-($name)"
  let triple = $target.triple
  let defines = [
    -DCMAKE_C_COMPILER=clang -DCMAKE_CXX_COMPILER=clang++ -DCMAKE_ASM_COMPILER=clang
    # cmake's Windows-Clang platform module insists on a resource compiler
    -DCMAKE_RC_COMPILER=llvm-rc
    $"-DCMAKE_C_COMPILER_TARGET=($triple)" $"-DCMAKE_CXX_COMPILER_TARGET=($triple)" $"-DCMAKE_ASM_COMPILER_TARGET=($triple)"
    -DCMAKE_C_COMPILER_WORKS=ON -DCMAKE_CXX_COMPILER_WORKS=ON
    $"-DCMAKE_SYSTEM_NAME=($target.system)" $"-DCMAKE_SYSTEM_PROCESSOR=($name | split row '-' | first)"
    -DCOMPILER_RT_DEFAULT_TARGET_ONLY=ON -DCOMPILER_RT_BUILTINS_HIDE_SYMBOLS=ON
    # the bootstrap's own selection: atomic.c in, the hosted-libc files out, crt objects apart
    -DCOMPILER_RT_EXCLUDE_ATOMIC_BUILTIN=OFF -DCOMPILER_RT_BAREMETAL_BUILD=ON -DCOMPILER_RT_BUILD_CRT=OFF
  ]
  with-tools cmake -S $"($tree)/compiler-rt/lib/builtins" -B $build -G Ninja ...$defines $">($build).log 2>&1"

  with-tools ninja -C $build -t targets all
  | lines
  | parse -r '^(?<object>CMakeFiles/clang_rt\.builtins[^:]*\.o(?:bj)?):'
  | get object
  | each {|object|
    let source = ($object | str replace -r '^CMakeFiles/[^/]+\.dir/(.*)\.o(bj)?$' '$1')
    # aarch64 outline atomics come from one generated .S per op, size and ordering
    if $source =~ 'outline_atomic_helpers\.dir/' or $source =~ '/outline_atomic_' { $"@lse/($source | path basename)" } else { $source }
  }
  | sort
  | uniq
}

def apple-sources [tree: path, base: list<string>, target: record]: nothing -> list<string> {
  let excluded = (open --raw $"($tree)/compiler-rt/lib/builtins/Darwin-excludes/osx.txt" | lines | compact -e)
  [...$base ...$target.extra]
  | where ($it | path parse | get stem) not-in $excluded and $it != "enable_execute_stack.c"
  | sort
  | uniq
}

export def files [entry: record]: nothing -> record {
  let url = ($entry.sources | where key == default | first | get url)
  let tarball = (^nix store prefetch-file --json $url | from json | get storePath)
  let tree = (mktemp -d -t llvm-update.XXXX)
  ^tar -xf $tarball -C $tree --strip-components 1 --wildcards "*/compiler-rt" "*/cmake"

  let elf = ($elf_targets | items {|name, target| {name: $name, sources: (elf-sources $tree $name $target)} })
  let apple = ($apple_targets | items {|name, target|
    let base = ($elf | where name == $target.base | first | get sources)
    {name: $name, sources: (apple-sources $tree $base $target)}
  })
  rm -rf $tree

  $elf ++ $apple | reduce --fold {} {|list, written|
    print -e $"  ($list.name): ($list.sources | length) builtins sources"
    $written | insert $"builtins-($list.name).txt" ($list.sources | each {|source| $"($source)\n" } | str join)
  }
}
