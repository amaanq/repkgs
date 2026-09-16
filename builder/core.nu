# The package builder's shared vocabulary. jib build spawns nu on each nu phase with this
# directory on the include path and hands what the phase exported on to the next one.
# This module is what inline phases and package modules import: ctx, options, x, tool, note, exports.

export use glob.nu *
export use log.nu *

# what build-system and inline phases get to see: {spec out deps njobs src build platform testsRun}
export def ctx []: nothing -> record<spec: record, out: string, dest: string, deps: list<record<name: string, root: string>>, roots: list<string>, njobs: int, src: string, build: string, platform: record, testsRun: bool, cache: bool> { open $"($env.NIX_BUILD_TOP)/ctx.json" }

# Nix merges build-system defaults before jib writes the context.
export def options [bs: string]: nothing -> record { (ctx).spec | get $bs }

# store path of the dependency called `name` (its exports name), or an error saying why it is needed
export def dep-root [name: string, why: string]: nothing -> string {
  let d = ((ctx).deps | where name == $name)
  if ($d | is-empty) { error make {msg: $"($why): pkgs.($name) must be in dependencies"} }
  $d.0.root
}

# store path of the build dependency called `name`, for tools that are not a bin/ on PATH
export def tool-root [name: string]: nothing -> string {
  let r = ((attrs).buildDependencies | where { (^jib exports $in | from json | first | get name) == $name })
  if ($r | is-empty) { error make {msg: $"buildPkgs.($name) must be in buildDependencies"} }
  $r.0
}


# files `names` of `dir` -> $out/bin. finish checks afterwards that every `bin` of the spec exists
export def install-bins [dir: string, names: list<string>]: nothing -> nothing {
  let c = (ctx)
  mkdir $"($c.out)/bin"
  for b in $names { cp $"($dir)/($b)" $"($c.out)/bin/($b)" }
}

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

# file names for this build's platform, from nix/platforms.nix `ext` ({exe shared static import}
# plus binfmt). `shlib z` -> libz.so | libz.dylib | libz.dll, `shlib z 1` -> libz.so.1 | libz.1.dylib | libz-1.dll
export def shlib [name: string, version?: string]: nothing -> string {
  let p = (ctx).platform
  let v = (if $version == null { "" } else if $p.binfmt == "coff" { $"-($version)" } else { $".($version)" })
  if $p.binfmt == "elf" { $"lib($name)($p.ext.shared)($v)" } else { $"lib($name)($v)($p.ext.shared)" }
}

# what `-l<name>` resolves to: the shared library, or its import library where the platform has those
export def linklib [name: string]: nothing -> string {
  let p = (ctx).platform
  $"lib($name)($p.ext.import? | default $p.ext.shared)"
}

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

# bin/<name> as a launch record (pkgs/la/launch): `program` with `args` before the user's, `vars`
# name -> value set for it. For entry points that are not files with a #! line (escripts)
export def write-launcher [name: string, program: string, args: list<string>, vars: record = {}]: nothing -> nothing {
  ^jib launcher $name $program ...$args --env ($vars | to json -r)
}

# no /usr/bin/env in the sandbox: point such scripts at the build PATH's env (build tree only;
# finish turns installed copies back with --undo and bin/ scripts get launchers). mtimes are kept
# in the build tree: a generator script newer than its shipped output makes make regenerate it
# (coreutils' cu-progs.m4 -> aclocal, ruby's prism templates -> baseruby)
export def fix-env-shebangs [dir: path, --undo]: nothing -> nothing {
  ^jib shebangs $dir ...(if $undo { [--undo] } else { [] })
}
