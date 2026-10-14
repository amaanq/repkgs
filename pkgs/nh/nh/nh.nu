use core.nu [ctx]

# nh tests run nix eval and copy a flake into the store, so nix needs one it can write here
export def --env nix-store []: nothing -> nothing {
   let root = $"((ctx).build)/nix"

   $env.NIX_CONFIG     = $"store = ($root)/store\nexperimental-features = nix-command flakes"
   $env.NIX_STATE_DIR  = $"($root)/state"
   $env.NIX_CONF_DIR   = $"($root)/conf"
   $env.XDG_CACHE_HOME = $"($root)/cache"
}
