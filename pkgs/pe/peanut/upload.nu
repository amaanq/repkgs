#!/usr/bin/env nu
# Publishes peanut as a GitHub release and points act.toml and act.lock at it. Run by hand. Each target is
# built on this machine from default.nix's `nar`, a directory holding <name>.nar.xz and nar-hash,
# which is the SRI hash of the uncompressed NAR act.lock pins.

# `key = value` lines aligned the way taplo aligns them, under `header` unless it is empty
def render [header: string, fields: record] {
   let width = $fields | columns | each { str length } | math max
   let lines = $fields | transpose key value | each {|field|
      $"($field.key | fill --width $width) = ($field.value | to json --raw)"
   }
   if $header == "" { $lines | str join "\n" } else { [$header ...$lines] | str join "\n" }
}

def main [
   --systems: list<string> = [x86_64-linux aarch64-linux riscv64-linux] # the rest keep their entries
   --repo: string = "manic-systems/motley"
] {
   let act    = $env.FILE_PWD | path join act.toml
   let lock   = $env.FILE_PWD | path join act.lock
   let local  = ^nix config show system | str trim

   let built = $systems | each {|system|
      print --stderr $"building peanut for ($system) on ($local)"
      let out = (
         ^nix-build --no-out-link ($env.FILE_PWD | path join default.nix) -A nar
            --argstr system $system --argstr buildSystem $local
      ) | str trim
      let archives = ls $out | where name =~ '\.nar\.xz$' | get name
      if ($archives | length) != 1 {
         error make { msg: $"($out) holds ($archives | length) .nar.xz files, expected one" }
      }
      let archive = $archives | first
      let parsed  = $archive | path basename | parse --regex '^peanut-(?<generation>[^-]+)-'
      if ($parsed | is-empty) {
         error make { msg: $"($archive) is not named peanut-<generation>-<system>.nar.xz" }
      }
      {
         system:     $system
         archive:    $archive
         generation: ($parsed | first | get generation)
         hash:       (open --raw ($out | path join nar-hash) | str trim)
      }
   }

   let generations = $built | get generation | uniq
   if ($generations | length) != 1 {
      error make { msg: $"the systems built different generations: ($generations | str join ', ')" }
   }
   let generation = $generations | first
   let tag        = $"peanut-($generation)"

   if (do { ^gh release view $tag --repo $repo } | complete | get exit_code) != 0 {
      ^gh release create $tag --repo $repo --title $tag --notes $"bootstrap peanut ($generation), built by pkgs/pe/peanut/build.nix"
   }
   ^gh release upload $tag ...($built | get archive) --repo $repo --clobber

   # act.toml and act.lock only change once the assets they name exist
   let fresh = $built | each {|entry|
      {
         system: $entry.system
         fetch:  { asset: $"peanut-{version}-($entry.system).nar.xz", unpack: true, name: "peanut" }
      }
   }
   let kept = open $act | get fetch | transpose system fetch | where system not-in $systems
   let tables = [
      (render "" { tag: "peanut-{version}", track: $"github:($repo)" })
      ...($fresh ++ $kept | each {|source| render $"[fetch.($source.system)]" $source.fetch })
   ]
   $tables | str join "\n\n" | $in + "\n" | save --force $act

   let hashes = $built
      | reduce --fold (open --raw $lock | from toml | get hash) {|entry, hash| $hash | upsert $entry.system $entry.hash }
      | sort
   [(render "" { version: $generation }) (render "[hash]" $hashes)] | str join "\n\n" | $in + "\n" | save --force $lock

   print --stderr $"($tag): uploaded ($systems | str join ', ') to ($repo) and updated ($act) and ($lock)"
}
