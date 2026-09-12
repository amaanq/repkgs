def main [installable: string = ".#bootstrap-llvm"]: nothing -> string {
  let result = (^nix derivation show --recursive $installable | complete)
  if $result.exit_code != 0 { error make {msg: $result.stderr} }

  let document = ($result.stdout | from json)
  if $document.version != 4 { error make {msg: "expected Nix derivation JSON version 4"} }
  let graph = ($document.derivations | transpose path drv)
  let forbidden = ($graph | where {|entry|
    $entry.drv.name =~ '^(rustc?|rustfmt|cargo|nushell|jigd?)(-|$)' or $entry.drv.name =~ 'bootstrap-tools'
  } | get path)
  if ($forbidden | is-not-empty) {
    error make {msg: $"LLVM bootstrap depends on binary toolchains or later stages\n($forbidden | str join (char newline))"}
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

  {
    target: $installable
    derivations: ($graph | length)
    seed: ($seeds | first | get drv | select name outputs)
    fixed_outputs: $sources
  } | to json
}
