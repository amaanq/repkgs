# Everything after the last phase, in the order `main` lists it.
use core.nu *
use debug.nu split-debug
use implant.nu
use launchers.nu

# --env: the cd must outlive the call
export def --env main [
  --keep-tree  # tests.separate: save source+build tree for the tests derivation
]: nothing -> nothing {
  let c = (ctx)
  install-map $c
  if $keep_tree { save-tree (attrs).outputs.tree }
  if not ($c.out | path exists) { error make {msg: "nothing was installed into $out"} }
  for b in (bins $c) {
    if not ($"($c.out)/bin/($b)($c.platform.ext.exe)" | path exists) { error make {msg: $"bin/($b)($c.platform.ext.exe) missing in output"} }
  }
  let inv = (prune $c.out)
  layout-check $c.out
  # installed copies of source scripts carry the build env's path from prepare: not a dependency
  fix-env-shebangs $c.out --undo
  mkdir (attrs).outputs.debug
  relocate $c $inv
  write-exports $c.out $c.spec $c.platform $c.deps
  note exports (open --raw $"($c.out)/exports.json" | from json | to json -r)
  cd $env.NIX_BUILD_TOP
  to-store $c.out $c.dest
  version-check ($c | update out $c.dest)
  cache-summary
}

# prefix -> store: .pc files, foo-config scripts and absolute links made relative, anything
# still naming the prefix is an error
export def to-store [prefix: string, dest: string]: nothing -> nothing {
  ^chmod -R u+w $prefix # some install -m 0444
  ^jig relativize $prefix $dest ...(if (ctx).platform.os == "macos" { [--macho] } else { [] })
  ^mv $prefix $dest
}

# tests derivation output: a result marker. The package itself is untouched
export def tests []: nothing -> nothing {
  mkdir $env.PKGS_RESULT
  {package: (ctx).out, passed: true} | to json | save $"($env.PKGS_RESULT)/result.json"
  note tests passed
}

# spec.install {"<dest under $out>": glob | [globs]} relative to the source tree, a dest ending
# in / is a directory. spec.links {"<path>": "<target>"}, bin/ entries get the platform's exe suffix
def install-map [c: record]: nothing -> nothing {
  for e in ($c.spec.install? | default {} | transpose dest from) {
    let to = $"($c.out)/($e.dest)"
    let from = ($e.from | each {|g| files --any $g } | flatten)
    if ($from | is-empty) { error make {msg: $"install ($e.dest): nothing matches ($e.from)"} }
    if ($e.dest | str ends-with "/") or ($from | length) > 1 {
      mkdir $to
      for f in $from { ^cp -rp $f $to }
    } else {
      mkdir ($to | path dirname)
      ^cp -rp $from.0 $to
    }
  }
  for e in ($c.spec.links? | default {} | transpose path target) {
    let exe = (if ($e.path | str starts-with "bin/") { $c.platform.ext.exe } else { "" })
    mkdir ($"($c.out)/($e.path)" | path dirname)
    ^ln -sfn $"($e.target)($exe)" $"($c.out)/($e.path)($exe)"
  }
}

# source and build tree as a plain directory: the store and binary caches compress and dedup
# files, a tarball would defeat both. The store sets every mtime to 1, and with all of them
# equal make has nothing to rebuild
def save-tree [tree: path]: nothing -> nothing {
  mkdir $tree
  cd $env.NIX_BUILD_TOP
  x cp -a source build $tree
}

# `bin`, defaulting to the package's name when bin/<name> got installed
def bins [c: record]: nothing -> list<string> {
  $c.spec.bin? | default (if ($"($c.out)/bin/($c.spec.name)($c.platform.ext.exe)" | path exists) { [$c.spec.name] } else { [] })
}

# docs, junk with absolute paths or timestamps removed. Returns the inventory of what is left
export def prune [out: path]: nothing -> table {
  ^jig prune $out | from json
}

# lib/ and bin/ only
export def layout-check [out: path]: nothing -> nothing {
  for d in [lib64 sbin] {
    if ($"($out)/($d)" | path exists) { error make {msg: $"($d)/ in output: configure with --libdir/--sbindir so it installs into lib/ and bin/"} }
  }
}

# prebuilt `true` implants interp + stub, "ldso" stays byte-identical behind a launcher.
# --deny: a cross output must not mention build-machine packages. Debug split, implant and
# launchers are ELF only so far, reloc-fixup does ELF and Mach-O
def relocate [c: record, inv: table]: nothing -> nothing {
  let prebuilt = ($c.spec.prebuilt? | default false)
  if $c.platform.binfmt == "elf" {
    if $c.spec.debug { split-debug $c.out (attrs).outputs.debug $c.njobs ($inv | where type == f) }
    if $prebuilt == true { implant $c }
    launchers $c
  }
  let a = (attrs)
  let deny = (if $c.platform.cross { $a.buildDependencies | where { $in not-in $a.dependencies } | each { [--deny $in] } | flatten } else { [] })
  if $prebuilt != "ldso" { x reloc-fixup $c.out --dest $c.dest ...$deny }
}

# tests.version: a command whose output must contain the pinned version (true = --version), its
# first word picks the binary. Run in place and from a copy of the closure under another root
# (env -i), where an absolute store path would show. Under dlaudit: a failed dlopen is an error
def version-check [c: record]: nothing -> nothing {
  let bins = (bins $c)
  let line = ($c.spec.tests?.version? | default ($bins | is-not-empty))
  # qemu-user does not follow launch's execve of the real binary, wine's CreateProcess stays inside
  if $line == false or ($c.platform.cross and not $c.testsRun and ($c.platform.os != "windows" or ($c.platform.emulator | is-empty))) { return }
  let words = (if $line == true { [--version] } else { $line | split row " " })
  let cmd = (if $words.0 in $bins { $words } else { $bins | first 1 | append $words })
  let want = ($c.spec.version | str replace -r '-r[0-9]+$' "")
  let audit_out = $"($env.NIX_BUILD_TOP)/dlaudit.txt"
  # via qemu's -E when emulated. The audit libc needs surplus static TLS (librustc_driver),
  # taken from every thread's stack: 64K, tokio workers have 2M
  let audit = ([$"LD_AUDIT=($c.platform.dlaudit)" $"DLAUDIT_OUT=($audit_out)" "GLIBC_TUNABLES=glibc.rtld.optional_static_tls=0x10000"]
    | where { $c.platform.dlaudit != "" }
    | each {|e| if ($c.platform.emulator | is-empty) { [$e] } else { [-E $e] } } | flatten)
  let run = {|root: string|
    cd /
    rm -f $audit_out
    # empty environment but for HOME (rebar3 crashes without) and the emulator's own.
    # bzip2 --version goes on to compress stdin: stdout can be binary
    let keep = ($env | transpose k v | where k =~ '^(WINE|QEMU_)' | each { $"($in.k)=($in.v)" })
    let r = (^env -i $"HOME=($env.NIX_BUILD_TOP)" ...$keep ...($c.platform.emulator) ...$audit $"($root)/bin/($cmd.0)($c.platform.ext.exe)" ...($cmd | skip 1) | complete)
    if $r.exit_code != 0 or not ($"($r.stdout)($r.stderr)" | str contains $want) {
      error make {msg: $"version check: `($cmd | str join ' ')` did not print ($want) \(exit ($r.exit_code))\n($r.stdout)($r.stderr)"}
    }
    let missed = (if ($audit_out | path exists) { open --raw $audit_out | lines | uniq | where { $in not-in ($c.spec.tests?.dlopen? | default []) } } else { [] })
    if ($missed | is-not-empty) {
      error make {msg: $"version check: `($cmd | str join ' ')` dlopens ($missed | str join ', '), not in the closure \(a missing dependency, or tests.dlopen = [names] if optional)"}
    }
  }
  do $run $c.out
  # beside the copy: every store root the build saw, launch (bin/ launchers link to it), and the
  # transitive DLL dirs a PE launcher names (an ELF reaches those through the real store)
  let root = $"($env.NIX_BUILD_TOP)/relocated"
  mkdir $root
  let dlls = ((exports-of $c.out).dllDirs | each { path split | take 4 | path join })
  for d in ($c.roots ++ [($c.platform.launch | path dirname -n 2)] ++ $dlls | uniq) { ^ln -sf $d $root }
  ^cp -r $c.out $root
  do $run $"($root)/($c.out | path basename)"
  rm -rf $root
  note version $"($cmd | str join ' ') -> ($want)"
}

# "jig: cc cached=812/855 (95%) compiled=40 …" from $JIG_LOG (tool, outcome, subject, ms per run)
def cache-summary []: nothing -> nothing {
  if not ($env.JIG_LOG | path exists) { return }
  # `query` (cc -v, -dM, -print-*) is not a build step and stays out of the counts
  let runs = (open --raw $env.JIG_LOG | from tsv --noheaders | rename tool outcome subject | where outcome != query)
  let tools = ($runs | group-by tool --to-table)
  let cached_outcomes = [cached cached-error hit compile-hit]
  let parts = ($tools | each {|t|
    let counts = ($t.items.outcome | uniq -c)
    let total = ($t.items | length)
    let cached = ($counts | where value in $cached_outcomes | get count | append 0 | math sum)
    let rest = ($counts | where value not-in $cached_outcomes | each { $"($in.value)=($in.count)" })
    [$t.tool $"cached=($cached)/($total) \(($cached * 100 // $total)%)" ...$rest] | str join " "
  })
  if ($parts | is-not-empty) { note jig ($parts | str join ", ") }
  # only cc subjects say why ("<source> new-key|inputs-changed:<path>|object-gone")
  for t in $tools {
    let reasons = ($t.items | where outcome starts-with compiled and subject =~ " " | get subject | each { split row " " | last })
    if ($reasons | is-empty) { continue }
    note $"($t.tool)-misses" ($reasons | each { split row ":" | first } | uniq -c | each { $"($in.value)=($in.count)" } | str join " ")
    let stale = ($reasons | where $it starts-with "inputs-changed:" | str substring 15.. | uniq -c | sort-by -r count | first 5 | each { $"($in.value) ×($in.count)" })
    if ($stale | is-not-empty) { note $"($t.tool)-stale" ($stale | str join ", ") }
  }
  # a sample of what jig would not cache
  if ($env.JIG_LOG_ARGS | path exists) {
    for l in (open --raw $env.JIG_LOG_ARGS | lines | shuffle | first 5) { note uncached ($l | str substring 0..300) }
  }
}
