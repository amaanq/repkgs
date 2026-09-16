# jib finish steps on an installed prefix the size of a mid-sized package.
use fixtures.nu
use ../builder/core.nu *

# a fresh copy per round: the steps modify the tree
def on-copy [out: path, body: closure]: nothing -> any {
  rm -rf $"($out).run"
  ^cp -rp $out $"($out).run"
  do $body $"($out).run"
}

export def benches [tmp: path, seed: path]: nothing -> table<name: string, note: string, run: closure> {
  let out = (fixtures output $"($tmp)/store/('' | fill -w 32 -c p)-pkg" $seed)
  let deps = (fixtures dep-store $"($tmp)/depstore" | each {|r| {root: $r} })
  let n = (^find $out -type f | lines | length)
  [
    {name: "finish/copy-only", note: "the per-round cp the other finish benches include", run: {|| on-copy $out {|o| null } }}
    {name: "finish/prune", note: $"($n) files: .la, docs, pch, man .gz", run: {|| on-copy $out {|o| ^jib prune $o | ignore } }}
    {name: "finish/fix-env-shebangs-undo", note: $"($n) files", run: {|| on-copy $out {|o| fix-env-shebangs $o --undo } }}
    {name: "finish/split-debug", note: "120 ELF with DWARF", run: {|| on-copy $out {|o| mkdir $"($o)-debug"; ^jib prune $o | ignore; ^jib split-debug $o $"($o)-debug" } }}
    {name: "finish/exports-of", note: "include, lib, pkgconfig scan", run: {|| ^jib exports $out | from json }}
    {name: "finish/write-exports", note: "6 .pc + 1 cmake config resolved against 60 deps", run: {|| {name: pkg, ext: {exe: "", shared: ".so", import: null}, exports: null, deps: $deps.root} | to json -r | ^jib write-exports $out }}
  ]
}
