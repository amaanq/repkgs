def main [installable: string = ".#bootstrap-llvm", --rust, --mrustc]: nothing -> string {
   if $rust and $mrustc { error make {msg: "select either the Rust chain or mrustc"} }
   let result = (^nix derivation show --recursive $installable | complete)
   if $result.exit_code != 0 { error make {msg: $result.stderr} }

   let document = ($result.stdout | from json)
   if $document.version != 4 { error make {msg: "expected Nix derivation JSON version 4"} }
   let graph = ($document.derivations | transpose path drv)
   let rust_chain = (open ($env.FILE_PWD | path join ../pkgs/pe/peanut/rust-chain.toml) | get release)
   let rust_sources = ($rust_chain | each {|archive|
      {
         name: $"rustc-($archive.version)-src.tar.gz"
         url: $"https://static.rust-lang.org/dist/rustc-($archive.version)-src.tar.gz"
         hash: $archive.hash
      }
   } | sort-by name)
   let rust_version = (open --raw ($env.FILE_PWD | path join ../pkgs/ru/rust/act.lock) | from toml | get version)
   let nu_version = (open --raw ($env.FILE_PWD | path join ../pkgs/nu/nushell/act.lock) | from toml | get version)
   let mummer_crates = (open --raw ($env.FILE_PWD | path join ../pkgs/mu/mummer/src/Cargo.lock) | from toml
      | get package | where source? == "registry+https://github.com/rust-lang/crates.io-index"
      | each {|crate| $"($crate.name)-($crate.version)" })
   let rust_names = (if $rust {
      ($rust_sources | get name) ++ $mummer_crates ++ [
         rustc-1.90.0-src.tar.gz
         $"rust-($rust_version)-source"
         $"nushell-($nu_version)-source"
         cargo-build-hook.sh cargo-check-hook.sh cargo-install-hook.sh cargo-setup-hook.sh
         cargo-vendor-dir
      ]
   } else if $mrustc { [rustc-1.90.0-src.tar.gz] } else { [] })
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
      let found = ($sources | where name in ($rust_sources | get name) | each {|entry|
         {name: $entry.name, url: ($entry.urls | get 0), hash: $entry.outputs.out.hash}
      } | sort-by name)
      let complete_chain = ($graph | any {|entry| $entry.drv.name == $"peanut-rust-($rust_version)"})
      let expected = (if $complete_chain { $rust_sources } else { $rust_sources | first ($found | length) })
      if ($found | is-empty) or $found != $expected {
         error make {msg: "expected a prefix of the pinned Rust source releases"}
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
