def main [installable: string = ".#bootstrap-llvm", --rust, --mrustc]: nothing -> string {
   if $rust and $mrustc { error make {msg: "select either the Rust build or mrustc"} }
   let result = (^nix derivation show --recursive $installable | complete)
   if $result.exit_code != 0 { error make {msg: $result.stderr} }

   let document = ($result.stdout | from json)
   if $document.version != 4 { error make {msg: "expected Nix derivation JSON version 4"} }
   let graph = ($document.derivations | transpose path drv)
   let pinned = (open ($env.FILE_PWD | path join ../pkgs/pe/peanut/mrustc.toml) | get rustc)
   let mrustc_source = {
      name: $"rustc-($pinned.version)-src.tar.gz"
      url: $"https://static.rust-lang.org/dist/rustc-($pinned.version)-src.tar.gz"
      hash: $pinned.hash
   }
   let rust_version = (open --raw ($env.FILE_PWD | path join ../pkgs/ru/rust/act.lock) | from toml | get version)
   let nu_version = (open --raw ($env.FILE_PWD | path join ../pkgs/nu/nushell/act.lock) | from toml | get version)
   let mummer_crates = (open --raw ($env.FILE_PWD | path join ../tools/bootstrap/Cargo.lock) | from toml
      | get package | where source? == "registry+https://github.com/rust-lang/crates.io-index"
      | each {|crate| $"($crate.name)-($crate.version)" })
   let rust_names = (if $rust {
      $mummer_crates ++ [
         $mrustc_source.name
         $"rust-($rust_version)-source"
         $"nushell-($nu_version)-source"
         cargo-build-hook.sh cargo-check-hook.sh cargo-install-hook.sh cargo-setup-hook.sh
         cargo-vendor-dir
      ]
   } else if $mrustc { [$mrustc_source.name] } else { [] })
   let forbidden = ($graph | where {|entry|
      (($entry.drv.name =~ '^(rustc?|rustfmt|cargo|nushell|mummerd?)(-|$)' and $entry.drv.name not-in $rust_names)
         or $entry.drv.name =~ 'bootstrap-tools'
         or (not $rust and $entry.drv.name =~ '^peanut-(rust|nushell)'))
   } | get path)
   if ($forbidden | is-not-empty) {
      error make {msg: $"bootstrap has an unexpected toolchain dependency\n($forbidden | str join (char newline))"}
   }

   let seeds = ($graph | where {|entry|
      $entry.drv.builder == "builtin:fetchurl" and $entry.drv.env.executable? == "1"
   })
   if ($seeds | get drv.name) != [hex0-seed] {
      error make {msg: "expected hex0-seed as the only executable builtin fetch"}
   }

   let sources = ($graph | where {|entry|
      $entry.drv.outputs | values | any {|output| $output.hash? != null }
   } | each {|entry|
      {
         derivation: $entry.path
         name: $entry.drv.name
         outputs: $entry.drv.outputs
         urls: ($entry.drv.structuredAttrs?.urls? | default ($entry.drv.env.urls? | default $entry.drv.env.url?))
      }
   } | sort-by name derivation)

   let rust_inputs = (if $rust {
      let found = ($sources | where name == $mrustc_source.name | each {|entry|
         {name: $entry.name, url: ($entry.urls | get 0), hash: $entry.outputs.out.hash}
      })
      if $found != [$mrustc_source] {
         error make {msg: "expected mrustc's pinned Rust source release"}
      }
      $found
   } else { [] })

   {
      target: $installable
      derivations: ($graph | length)
      peanut: ($seeds | first | get drv | select name outputs)
      rust_sources: $rust_inputs
      fixed_outputs: $sources
   } | to json
}
