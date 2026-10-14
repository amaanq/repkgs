use core.nu [act]
use glob.nu [glob-files]

# upstream ships tests/ as lspci dumps without a runner
export def test []: nothing -> nothing {
   let src    = (act).src
   let dumps  = (glob-files $"($src)/tests/*")
   let modes  = [ [ "-vvvnn" ] [ "-t" ] [ "-mm" ] ]
   let lib    = (glob-files --no-symlink $"($src)/lib/libpci.so.*" | first)
   let soname = (^llvm-readelf -d $lib | parse -r 'Library soname: \[(?<name>[^\]]+)\]' | get 0.name)
   let libs   = (mktemp -d)
   ^ln -s $lib $"($libs)/($soname)"

   let runs = ($dumps | each {|dump|
      $modes | each {|mode|
         let run = (with-env { LD_LIBRARY_PATH: $libs } { ^$"($src)/lspci" -F $dump -i $"($src)/pci.ids.gz" ...$mode | complete })
         { name: $"($dump | path basename) ($mode | str join ' ')", ok: ($run.exit_code == 0 and ($run.stdout | str trim | is-not-empty)), stderr: ($run.stderr | str trim) }
      }
   } | flatten)

   let failed = ($runs | where not ok | each {|run| $"($run.name): ($run.stderr)" })

   print $"lspci decoded ($dumps | length) dumps in ($modes | length) modes"
   if ($failed | is-not-empty) { error make {msg: ($failed | str join "\n")} }
}
