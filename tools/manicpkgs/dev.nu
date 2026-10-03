use nix.nu *

const SHELL = path self dev-shell.nu

def open-workspace [
  directory: path
  configuration: record
  --fresh
  --reset
]: nothing -> nothing {
  if $fresh { rm -rf $directory }
  mkdir $directory
  if $reset {
    for name in [source build prefix outputs] { rm -rf ($directory | path join $name) }
  }

  let stamp = ($directory | path join configuration.json)
  let changed = not ($stamp | path exists) or (open $stamp) != $configuration
  if $changed or $reset {
    rm -f ($directory | path join ctx.json) ($directory | path join dev-env.json)
  }
}

def prepare-build [directory: path, configuration: record]: nothing -> record {
  let exported = (open ($directory | path join inputs attrs.json))
  let outputs = ($exported.devOutputs | reduce --fold {} {|name, result|
    $result | insert $name ($directory | path join outputs $name)
  })
  let prefix = ($directory | path join prefix)
  let attrs = ($exported | upsert outputs $outputs | upsert package $prefix)
  let attrs_file = ($directory | path join attrs.json)
  $attrs | to json | save -f $attrs_file
  mkdir ($directory | path join outputs)

  let environment = {
    HOME: $directory
    TERM: ($env.TERM? | default dumb)
    NIX_BUILD_TOP: $directory
    NIX_ATTRS_JSON_FILE: $attrs_file
    NIX_STORE: /nix/store
    NIX_BUILD_CORES: $configuration.cores
    SHIM_SOCK: $configuration.socket
  }
  let variables = ($environment | transpose name value | where value != null
    | each {|entry| $"($entry.name)=($entry.value)" })
  try { ^env -i ...$variables $attrs.builder dev prepare } catch { fail "development setup failed" }

  $configuration | to json | save -f ($directory | path join configuration.json)
  {variables: $variables, shell: ($attrs.devNu | path join bin nu)}
}

export def "main dev" [
  --for: string
  --crib: path
  --rust-toolchain: path
  --fresh
  --reset
  attr: string
  ...cmds: string
]: nothing -> nothing {
  let evaluated = (^nix-instantiate $"(root)/tools/manicpkgs/dev.nix" ...$FEATURES --argstr attr $attr
    ...(platform-arg $for) ...(crib-arg $crib) ...(rust-toolchain-arg $rust_toolchain) | complete)
  if $evaluated.exit_code != 0 { fail $evaluated.stderr }
  let derivation = ($evaluated.stdout | str trim)
  let configuration = {
    derivation: $derivation
    socket: (shimd-socket)
    cores: ($env.NIX_BUILD_CORES? | default 30)
  }
  let cache = ($env.XDG_CACHE_HOME? | default --empty ("~/.cache" | path expand))
  let directory = ($cache | path join manicpkgs dev $"($attr)-($for | default native)" | path expand)
  open-workspace $directory $configuration --fresh=$fresh --reset=$reset

  try {
    ^nix ...$FEATURES build $"($derivation)^out" --out-link ($directory | path join inputs) -L ...(cache-args)
  } catch {
    fail "could not prepare the development inputs"
  }
  let session = (prepare-build $directory $configuration)
  let command = $"source ($SHELL | to nuon)\n($cmds | str join ' ')"
  let run = (if ($cmds | is-empty) { [-e $command] } else { [-c $command] })
  try {
    ^env -i ...$session.variables $session.shell --no-config-file --experimental-options=[cell-path-types] ...$run
  } catch {
    fail "development command failed"
  }
}
