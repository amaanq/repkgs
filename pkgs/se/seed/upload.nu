#!/usr/bin/env nu
# Builds the seed nar for each system here (build.nix cross compiles), uploads them to the GitHub
# release seed-<N> and points ./recipe.toml at them. <N> comes from build.nix's seed-<N>-<system>.
#   pkgs/se/seed/upload.nu [--systems x86_64-linux,aarch64-linux,riscv64-linux] [--repo Mic92/repkgs]

def main [
  --systems: string = "x86_64-linux,aarch64-linux,riscv64-linux" # comma separated, the rest keep their entry
  --repo: string = "Mic92/repkgs" # where the seed-<N> releases live
]: nothing -> nothing {
  let here     = ($env.CURRENT_FILE | path dirname)
  let manifest = $"($here)/recipe.toml"
  let builder  = (^nix config show system | str trim)

  let nars = ($systems | split row "," | each {|system|
    print -e $"building the seed nar for ($system)"
    let out  = (^nix-build $"($here)/default.nix" -A nar --argstr system $system --argstr buildSystem $builder --no-out-link | str trim)
    let file = (ls $out | where name =~ '\.nar\.xz$' | first | get name)
    {
      system: $system
      file: $file
      hash: (open --raw $"($out)/nar-hash" | str trim)
      version: ($file | path basename | parse "seed-{version}-{rest}" | first | get version)
    }
  })
  let versions = ($nars | get version | uniq)
  if ($versions | length) != 1 { error make {msg: $"the systems build different seed versions: ($versions)"} }
  let version = ($versions | first)
  let tag     = $"seed-($version)"

  if (^gh release view $tag -R $repo | complete).exit_code != 0 {
    ^gh release create $tag -R $repo --title $tag --notes $"bootstrap seed ($version), built by pkgs/se/seed/build.nix"
  }
  ^gh release upload $tag -R $repo --clobber ...($nars | get file)

  let fresh = ($nars | each {|nar|
    {
      key: $nar.system
      url: $"https://github.com/($repo)/releases/download/seed-{version}/seed-{version}-($nar.system).nar.xz"
      hash: $nar.hash
      unpack: true
      name: "seed"
    }
  })
  let table = (open $manifest)
  let untouched = ($table.source | where key not-in ($nars | get system))
  $table | update source ($fresh ++ $untouched) | update pin {version: $version} | to toml | save -f $manifest
  print -e $"($tag): uploaded ($nars | get system | str join ', '), wrote ($manifest)"
}
