def --env load-build-env [] {
  let state = (open ($env.NIX_BUILD_TOP | path join dev-env.json))
  for name in $state.unset { hide-env -i $name }
  load-env ($state.env | update PATH { split row (char esep) })
  cd $state.cwd
}

def phases [] {
  open $env.NIX_ATTRS_JSON_FILE | get plan.phases.name
}

def --env phase [name: string@phases] {
  let attrs = (open $env.NIX_ATTRS_JSON_FILE)
  ^$attrs.builder dev phase $name
  if $env.LAST_EXIT_CODE != 0 { error make {msg: $"phase ($name) failed"} }
  load-build-env
}

load-build-env
