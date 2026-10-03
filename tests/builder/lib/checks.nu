# helpers for test.nu, with $env.pkg and $env.debug naming the package under test and its debug output
export def expect [claim: string, holds: bool] {
   if not $holds { error make {msg: $"FAIL: ($claim)"} }
}

export def section-in [file: string, section: string]: nothing -> bool {
   ^llvm-readelf -S $"($env.pkg)/($file)" | str contains $section
}

# a split leaves .symtab and no DWARF behind and files the DWARF under the build-id
export def split-debug [file: string] {
   let id = (^llvm-readelf -n $"($env.pkg)/($file)" | parse -r 'Build ID: ([0-9a-f]+)' | get -o capture0.0)
   expect $"($file) carries a build-id" ($id != null)
   expect $"($file) lost its DWARF"     (not (section-in $file .debug_info))
   expect $"($file) still has .symtab"  (section-in $file .symtab)
   let separate = $"($env.debug)/lib/debug/.build-id/($id | str substring 0..<2)/($id | str substring 2..).debug"
   expect $"($file) has DWARF in ($separate | path basename)" (($separate | path exists) and (^llvm-readelf -S $separate | str contains .debug_info))
}

export def pass [] { mkdir $env.out }
