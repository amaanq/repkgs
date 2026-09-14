# The package builder's shared vocabulary. jib build spawns nu on each nu phase with this
# directory on the include path and hands what the phase exported on to the next one.
# This module is what inline phases and package modules import: ctx, options, x, tool, note, exports.

export use glob.nu *
export use log.nu *
export use exports.nu *
use names.nu

# what build-system and inline phases get to see: {spec out deps njobs src build platform testsRun}
export def ctx []: nothing -> record<spec: record, out: string, dest: string, deps: list<record<name: string, root: string>>, roots: list<string>, njobs: int, src: string, build: string, platform: record, testsRun: bool, cache: bool> { open $"($env.NIX_BUILD_TOP)/ctx.json" }

# a build system's options: its OPTIONS defaults merged with the package's `<bs>.*` (prepare.nu)
export def options [bs: string]: nothing -> record { (ctx).spec | get $bs }

# `given` (the package's `<bs>.*` plus nix/build-systems.nix's root/tool/deps) over a system's
# OPTIONS table: unknown names and wrong types fail here, before any phase runs
export def merge-options [bs: string, table: record, given: record]: nothing -> record {
  let known = (($table | columns) ++ [root tool deps])
  let unknown = ($given | columns | where { $in not-in $known })
  if ($unknown | is-not-empty) { error make {msg: $"unknown option ($bs).($unknown | first) \(have: ($known | str join ' ')\)"} }
  $table | transpose name o | reduce -f $given {|it, acc|
    if $it.name not-in ($acc | columns) { return ($acc | upsert $it.name $it.o.default) }
    let v = ($acc | get $it.name)
    if $v == null and ($it.o.nullable? == true) { return $acc }
    if $v == null { return ($acc | upsert $it.name $it.o.default) }
    let want = ($it.o.type? | default ($it.o.default | describe | str replace -r '<.*' ""))
    let got = ($v | describe | str replace -r '<.*' "")
    if $want != nothing and $got != $want { error make {msg: $"option ($bs).($it.name) is a ($got), expected ($want)"} }
    $acc
  }
}

# the directory a build system works in: the source, or `<bs>.root` below it for monorepos
export def project-dir [bs: string]: nothing -> string {
  [(ctx).src (options $bs).root] | path join
}

# store path of the dependency called `name` (its exports name), or an error saying why it is needed
export def dep-root [name: string, why: string]: nothing -> string {
  let d = ((ctx).deps | where name == $name)
  if ($d | is-empty) { error make {msg: $"($why): pkgs.($name) must be in dependencies"} }
  $d.0.root
}

# store path of the build dependency called `name`, for tools that are not a bin/ on PATH
export def tool-root [name: string]: nothing -> string {
  let r = ((attrs).buildDependencies | where { (exports-of $in).name == $name })
  if ($r | is-empty) { error make {msg: $"buildPkgs.($name) must be in buildDependencies"} }
  $r.0
}


# files `names` of `dir` -> $out/bin. finish checks afterwards that every `bin` of the spec exists
export def install-bins [dir: string, names: list<string>]: nothing -> nothing {
  let c = (ctx)
  mkdir $"($c.out)/bin"
  for b in $names { cp $"($dir)/($b)" $"($c.out)/bin/($b)" }
}

# `tests.parallel = false` -> 1, else njobs
export def test-jobs []: nothing -> string { let c = (ctx); if ($c.spec.tests?.parallel? | default true) { $c.njobs } else { 1 } | into string }

# run an external, echoing the command line first (the build log is the `set -x` of this tree)
export def --wrapped x [cmd: string, ...args: string]: nothing -> nothing {
  print -e $"+ ($cmd) ($args | str join ' ')"
  ^$cmd ...$args
}

# absolute path of a tool on PATH
export def tool [name: string]: nothing -> path {
  let hits = (which $name)
  if ($hits | is-empty) { error make {msg: $"($name) not on PATH"} }
  $hits | first | get path
}

# file names for this build's platform (names.nu)
export def shlib [name: string, version?: string]: nothing -> string { names shlib (ctx).platform $name $version }
export def linklib [name: string]: nothing -> string { names linklib (ctx).platform $name }
export def shlibdir []: nothing -> string { names shlibdir (ctx).platform }

# the derivation's structured attrs (nix/package.nix `common`)
export def attrs []: nothing -> record { open $env.NIX_ATTRS_JSON_FILE }

# rewrite a text file in place, even one installed read-only: `edit $f { str replace a b }`
export def edit [f: path, change: closure]: nothing -> nothing {
  let text = (open --raw $f | do $change)
  chmod u+w $f
  $text | save -f $f
}

# Build records (Config.pm, Makefile.global) name the tools configure found by store path. As
# bare names they resolve to whatever is on PATH where the record is later used
export def tools-by-name []: string -> string { $in | str replace -ar '/nix/store/[a-z0-9]{32}-[^/]+/bin/' "" }

# starts with \x7fELF
export def is-elf [f: path]: nothing -> bool { (open --raw $f | first 4) == 0x[7f 45 4c 46] }
export def is-macho [f: path]: nothing -> bool { let m = (open --raw $f | first 4); $m == 0x[cf fa ed fe] or $m == 0x[ca fe ba be] }

# store paths -> {root}/{store} templates launch expands
export def storerel [p: string, out: string]: nothing -> string {
  $p | str replace -a $out "{root}" | str replace -a $env.NIX_STORE "{store}"
}

# bin/<name> as a launch record (builder/launchers.nu, pkgs/la/launch): `program` with `args`
# before the user's, `env` name -> value set for it. For build systems whose entry points are not
# files with a #! line (deno modules); paths are made package-relative here
export def write-launcher [name: string, program: string, args: list<string>, vars: record = {}]: nothing -> nothing {
  let c = (ctx)
  let rel = {|p| storerel $p $c.out }
  mkdir $"($c.out)/bin"
  {
    program: (do $rel $program), args: ($args | each { do $rel $in })
    env: ($vars | items {|k, v| {$k: {set: (do $rel $v)}} } | into record)
  } | to json -r | save -f $"($c.out)/bin/.($name)($c.platform.ext.exe).launch"
  if $c.platform.binfmt == "coff" { cp $c.platform.launch $"($c.out)/bin/($name).exe" } else { ^ln -sf $c.platform.launch $"($c.out)/bin/($name)" }
  note launcher $"bin/($name) -> ($program)"
}

# no /usr/bin/env in the sandbox: point such scripts at the build PATH's env (build tree only;
# finish turns installed copies back with --undo and bin/ scripts get launchers). mtimes are kept
# in the build tree: a generator script newer than its shipped output makes make regenerate it
# (coreutils' cu-progs.m4 -> aclocal, ruby's prism templates -> baseruby)
export def fix-env-shebangs [dir: path, --undo]: nothing -> nothing {
  ^jib shebangs $dir ...(if $undo { [--undo] } else { [] })
}
