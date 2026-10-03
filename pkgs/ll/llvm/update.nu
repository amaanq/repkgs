# mummer bootstrap compiles compiler-rt before any cmake exists, so builtins.toml records, per target,
# the lib/builtins sources compiler-rt's own cmake selects

const targets = [
   [name                  triple                         system ];
   [x86_64                x86_64-unknown-linux-gnu       Linux  ]
   [aarch64               aarch64-unknown-linux-gnu      Linux  ]
   [riscv64               riscv64-unknown-linux-gnu      Linux  ]
   [loongarch64           loongarch64-unknown-linux-gnu  Linux  ]
   [powerpc64le           powerpc64le-unknown-linux-gnu  Linux  ]
   [x86_64-windows-msvc   x86_64-pc-windows-msvc         Windows]
   [x86_64-windows-gnu    x86_64-w64-mingw32             Windows]
   [aarch64-windows-msvc  aarch64-pc-windows-msvc        Windows]
   [aarch64-windows-gnu   aarch64-w64-mingw32            Windows]
]

# cmake reaches Apple SDKs through xcrun, so macOS takes the aarch64 selection plus what
# lib/builtins/CMakeLists.txt adds under if(APPLE), less Darwin-excludes/osx.txt
const macos_atomics = [
   atomic_flag_clear.c atomic_flag_clear_explicit.c atomic_flag_test_and_set.c
   atomic_flag_test_and_set_explicit.c atomic_signal_fence.c atomic_thread_fence.c
]

def --wrapped in-shell [...words: string]: nothing -> string {
   ^nix-shell -p cmake ninja llvmPackages.clang-unwrapped llvmPackages.lld llvmPackages.llvm --run ($words | str join ' ')
}

def selected [tree: path, target: record]: nothing -> list<string> {
   let dir = $"($tree)/out-($target.name)"
   let cpu = ($target.name | split row "-" | first)
   let flags = [
      -G Ninja -DCMAKE_C_COMPILER=clang -DCMAKE_CXX_COMPILER=clang++ -DCMAKE_ASM_COMPILER=clang
      # Windows-Clang.cmake requires a resource compiler even for a static library
      -DCMAKE_RC_COMPILER=llvm-rc
      -DCMAKE_C_COMPILER_WORKS=ON -DCMAKE_CXX_COMPILER_WORKS=ON
      $"-DCMAKE_SYSTEM_NAME=($target.system)" $"-DCMAKE_SYSTEM_PROCESSOR=($cpu)"
      ...([C CXX ASM] | each {|lang| $"-DCMAKE_($lang)_COMPILER_TARGET=($target.triple)" })
      -DCOMPILER_RT_DEFAULT_TARGET_ONLY=ON -DCOMPILER_RT_BUILTINS_HIDE_SYMBOLS=ON
      -DCOMPILER_RT_EXCLUDE_ATOMIC_BUILTIN=OFF -DCOMPILER_RT_BAREMETAL_BUILD=ON -DCOMPILER_RT_BUILD_CRT=OFF
   ]
   in-shell cmake -S $"($tree)/compiler-rt/lib/builtins" -B $dir ...$flags $">($dir).log 2>&1"

   in-shell ninja -C $dir -t targets all
   | parse -r '(?m)^CMakeFiles/(?<unit>[^/]+)\.dir/(?<file>[^:]*)\.o(?:bj)?:'
   | where unit =~ '^clang_rt\.builtins|outline_atomic_helpers'
   | each {|row|
      # each aarch64 outline atomic is its own generated .S, which mummer makes from aarch64/lse.S
      if $row.unit =~ outline_atomic or ($row.file | path basename) =~ '^outline_atomic_' {
         $"@lse/($row.file | path basename)"
      } else {
         $row.file
      }
   }
   | sort
   | uniq
}

def toml-list [name: string, files: list<string>]: nothing -> string {
   let entries = ($files | each {|file| $"  \"($file)\",\n" } | str join)
   $"($name) = [\n($entries)]\n"
}

export def files [entry: record]: nothing -> record {
   let url     = ($entry.sources | where key == default | first | get url)
   let tarball = (^nix store prefetch-file --json $url | from json | get storePath)
   let tree    = (mktemp -d -t llvm-update.XXXX)
   ^tar -xf $tarball -C $tree --strip-components 1 --wildcards "*/compiler-rt" "*/cmake"

   let lists = ($targets | each {|target| {name: $target.name, files: (selected $tree $target)} })
   let darwin_excluded = (open --raw $"($tree)/compiler-rt/lib/builtins/Darwin-excludes/osx.txt" | lines | where $it != "")
   let macos = ($lists | where name == aarch64 | first | get files
      | append $macos_atomics
      | where {|file| ($file | path parse | get stem) not-in $darwin_excluded and $file != enable_execute_stack.c }
      | sort
      | uniq)
   rm -rf $tree

   let all = ($lists | append {name: aarch64-macos, files: $macos} | sort-by name)
   for list in $all { print -e $"  ($list.name): ($list.files | length) builtins sources" }
   let body = ($all | each {|list| toml-list $"\"($list.name)\"" $list.files } | str join "\n")
   {"builtins.toml": $"# written by update.nu\n\n($body)"}
}
