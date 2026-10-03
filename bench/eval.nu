#!/usr/bin/env nu
# What instantiating the packages motley shares with nixpkgs costs in each, from Nix's own counters
# (NIX_SHOW_STATS). A package pairs with the nixpkgs attribute of the same name, or the one named
# in `renames`. A set's fixed cost (its bootstrap, nixpkgs' stdenv) is paid by the first package, so
# the per-package figures divide what all of them cost beyond the first one alone.

const renames = {cpython314: python314, llvm: llvmPackages_23.llvm, clang: llvmPackages_23.clang, lld: llvmPackages_23.lld}

def instantiate [expr: string]: nothing -> record {
  let stats = (mktemp -t eval-stats.XXXX)
  with-env {NIX_SHOW_STATS: "1", NIX_SHOW_STATS_PATH: $stats} {
    ^nix-instantiate --quiet --expr $expr o+e> /dev/null
  }
  let counters = (open --raw $stats | from json)
  rm $stats
  {seconds: $counters.cpuTime, bytes: $counters.gc.totalBytes}
}

def quoted [names: list<string>]: nothing -> string {
  $names | each {|name| $"\"($name)\"" } | str join " "
}

def cost [set: string, runs: int, expr: closure, names: list<string>]: nothing -> record {
  let median = {|text: string| 1..$runs | each { instantiate $text } | math median }
  let all = (do $median (do $expr $names))
  let one = (do $median (do $expr ($names | first 1)))
  let rest = (($names | length) - 1)
  {
    set: $set
    seconds: ($all.seconds | math round -p 2)
    megabytes: ($all.bytes / 1e6 | math round -p 1)
    ms_per_package: (($all.seconds - $one.seconds) * 1000 / $rest | math round -p 2)
    kb_per_package: (($all.bytes - $one.bytes) / 1000 / $rest | math round -p 1)
  }
}

def main [
  --platform: string = "x86_64-linux"
  --runs: int = 5
  --json # one record instead of a table
]: nothing -> any {
  let root = ($env.FILE_PWD | path dirname)
  let nixpkgs = (^nix-instantiate --eval --raw --expr $"toString \(import ($root)/nix/nixpkgs.nix\)")
  let ours_set = $"import ($root) { platform = \"($platform)\"; }"
  let theirs_set = $"import ($nixpkgs) { system = \"($platform)\"; }"
  let lookup = "p: path: p.lib.attrByPath (p.lib.splitString \".\" path) null p"

  let supported = (^nix-instantiate --eval --strict --json --expr $"let s = ($ours_set); in builtins.filter \(n: s.${n}.supported\) \(builtins.attrNames s\)" | from json)
  let wanted = ($supported | each {|name| {ours: $name, theirs: ($renames | get -o $name | default $name)} })
  let present = (^nix-instantiate --eval --strict --json --expr $"let p = ($theirs_set); find = ($lookup); in map \(path: let r = builtins.tryEval \(\(find p path\).drvPath or null\); in r.success && r.value != null\) [(quoted ($wanted | get theirs))]" e> /dev/null | from json)
  let pairs = ($wanted | zip $present | where {|row| $row.1 } | each {|row| $row.0 })

  let ours_expr = {|names: list<string>| $"let s = ($ours_set); in map \(n: s.${n}\) [(quoted $names)]" }
  let theirs_expr = {|names: list<string>| $"let p = ($theirs_set); find = ($lookup); in map \(find p\) [(quoted $names)]" }
  let rows = [
    (cost motley $runs $ours_expr ($pairs | get ours))
    (cost nixpkgs $runs $theirs_expr ($pairs | get theirs))
  ]
  let count = ($pairs | length)
  if $json {
    {date: (date now | format date "%Y-%m-%d"), platform: $platform, packages: $count, runs: $runs, sets: $rows} | to json
  } else {
    print $"($count) packages on ($platform), median of ($runs) runs"
    $rows
  }
}
