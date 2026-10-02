#!/usr/bin/env nu
# For cpus upstream ships no binaries for, cross builds our own <pkg> here, uploads its bundled
# output as a nar to the GitHub release bootstrap-<pkg>-<version> and adds a <cpu> source to
# <pkg>-bootstrap's recipe.toml. That set then fetches it like any upstream tarball, as a fixed
# output with no tie to this machine's derivations.
#   tools/upload-bootstrap.nu jdk [--cpus riscv64] [--repo Mic92/repkgs] [--store-path /nix/store/…]

def main [
  ...pkgs: string # the packages whose output becomes <pkg>-bootstrap's source
  --cpus: string = "riscv64" # comma separated
  --repo: string = "Mic92/repkgs"
  --store-path: string # an output built elsewhere and `nix copy`ed here, which skips the build
]: nothing -> nothing {
  let root = ($env.CURRENT_FILE | path dirname | path dirname)
  for pkg in $pkgs {
    let manifest = (glob $"($root)/pkgs/*/($pkg)-bootstrap/recipe.toml" | first)
    let version  = (^nix-instantiate --eval --strict --json $root -A $"($pkg).version" | from json)
    let tag      = $"bootstrap-($pkg)-($version)"

    let nars = ($cpus | split row "," | each {|cpu|
      print -e $"($pkg) ($version): cross building for ($cpu)"
      # the bundle copies foreign libraries in and drops store references, which is what a
      # `prebuilt = true` package unpacks
      let out = if $store_path != null { $store_path } else {
        ^nix-build -E $"with import ($root)/nix/set.nix { platform = \"($cpu)-linux\"; }; bundle pkgs.($pkg)" --no-out-link | str trim
      }
      let file = $"($env.TMPDIR? | default /tmp)/($pkg)-($version)-($cpu)-linux.nar.xz"
      ^nix-store --dump $out | ^xz -T0 -9e | save -f --raw $file
      let base32 = (^nix-store -q --hash $out | str trim)
      {cpu: $cpu, file: $file, hash: (^nix hash convert --hash-algo sha256 --to sri $base32 | str trim)}
    })

    if (^gh release view $tag -R $repo | complete).exit_code != 0 {
      let commit = (^git -C $root rev-parse --short HEAD | str trim)
      ^gh release create $tag -R $repo --title $tag --notes $"($pkg) ($version) cross built at repkgs ($commit), for cpus upstream has no binaries for"
    }
    ^gh release upload $tag -R $repo --clobber ...($nars | get file)

    let fresh = ($nars | each {|nar|
      {
        key: $nar.cpu
        url: $"https://github.com/($repo)/releases/download/bootstrap-($pkg)-{version}/($pkg)-{version}-($nar.cpu)-linux.nar.xz"
        hash: $nar.hash
        name: $pkg
      }
    })
    let table = (open $manifest)
    let untouched = ($table.source | where key not-in ($nars | get cpu))
    $table | update source ($untouched ++ $fresh) | to toml | save -f $manifest
    print -e $"($tag): uploaded ($nars | get cpu | str join ', '), wrote ($manifest)"
  }
}
