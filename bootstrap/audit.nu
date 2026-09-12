def main [installable: string = ".#bootstrap-llvm", --rust, --mrustc]: nothing -> string {
  if $rust and $mrustc { error make {msg: "select either Rust stage0 or mrustc"} }
  let result = (^nix derivation show --recursive $installable | complete)
  if $result.exit_code != 0 { error make {msg: $result.stderr} }

  let document = ($result.stdout | from json)
  if $document.version != 4 { error make {msg: "expected Nix derivation JSON version 4"} }
  let graph = ($document.derivations | transpose path drv)
  let rust_manifest = (open ($env.FILE_PWD | path join ../pkgs/ru/rust-bootstrap/sources.toml))
  let rust_sources = ($rust_manifest.source | where key ends-with '-x86_64' | each {|archive|
    {
      name: $"rust-bootstrap-($rust_manifest.pin.version)-($archive.key)"
      url: ($archive.url | str replace '{version}' $rust_manifest.pin.version)
      hash: $archive.hash
    }
  } | sort-by name)
  let rust_version = (open ($env.FILE_PWD | path join ../pkgs/ru/rust/sources.toml) | get pin.version)
  let nu_version = (open ($env.FILE_PWD | path join ../pkgs/nu/nushell/sources.toml) | get pin.version)
  let rust_names = (if $rust {
    ($rust_sources | get name) ++ [
      $"rust-($rust_version)-source"
      $"nushell-($nu_version)-source"
      cargo-build-hook.sh cargo-check-hook.sh cargo-install-hook.sh cargo-setup-hook.sh
    ]
  } else if $mrustc { [rustc-1.90.0-src.tar.gz] } else { [] })
  let forbidden = ($graph | where {|entry|
    (($entry.drv.name =~ '^(rustc?|rustfmt|cargo|nushell|jigd?)(-|$)' and $entry.drv.name not-in $rust_names)
      or $entry.drv.name =~ 'bootstrap-tools'
      or (not $rust and $entry.drv.name =~ '^seed-(rust|nushell)'))
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
    if $found != $rust_sources {
      error make {msg: "expected exactly the pinned x86-64 Rust stage0 components"}
    }
    $found
  } else { [] })

  {
    target: $installable
    derivations: ($graph | length)
    seed: ($seeds | first | get drv | select name outputs)
    rust_bootstrap: $rust_inputs
    fixed_outputs: $sources
  } | to json
}
