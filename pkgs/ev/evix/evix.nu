# the eval tests open the default local store, which the sandbox has no /nix/var for
export def --env test-store []: nothing -> nothing {
   $env.NIX_REMOTE = $"local?root=($env.NIX_BUILD_TOP)/nix-root"
}
