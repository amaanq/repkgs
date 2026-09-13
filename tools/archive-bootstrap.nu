#!/usr/bin/env nu

def capture [program: string, arguments: list<string>]: nothing -> string {
  let result = (^$program ...$arguments | complete)
  if $result.exit_code != 0 { error make {msg: $result.stderr} }
  $result.stdout | str trim
}

def main [
  --state: path = "~/.local/state/repkgs/bootstrap-llvm"
  --destination: path
]: nothing -> nothing {
  let state = ($state | path expand)
  let destination = ($destination | path expand)
  let manifest = ($state | path join fixed-outputs.json)
  if not ($manifest | path exists) { error make {msg: $"missing ($manifest), run the LLVM audit first"} }
  let outputs = (open $manifest)
  let graph = ($state | path join graph.json | open)
  let source_paths = ($outputs | get path | sort | uniq)
  let derivations = ($graph.derivations | transpose path drv | get path | each {|path|
    if ($path | str starts-with "/") { $path } else { $"/nix/store/($path)" }
  } | sort | uniq)
  mkdir $destination
  let store = ($state | path join store)
  let uri = $"file://($destination)"
  print $"Archiving ($source_paths | length) fixed-output inputs"
  ^nix --store $store copy --to $uri --no-check-sigs ...$source_paths
  print $"Archiving ($derivations | length) derivation recipes"
  ^nix --store $store copy --derivation --to $uri --no-check-sigs ...$derivations
  {
    created: (date now)
    llvm_derivation: (open ($state | path join plan.json)).derivation
    fixed_outputs: $outputs
    derivations: $derivations
  } | to json | save --force ($destination | path join repkgs-bootstrap-manifest.json)
  print $"Archive complete at ($destination)"
}
