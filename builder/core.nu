# What every nu phase may use. jib build runs nu for each nu phase with this directory on the
# include path and carries what a phase exported into the next one. Inline phases and package
# modules import this module: ctx, options, x, tool, note, exports and the rest below.

export use glob.nu *
export use log.nu *

# the build's context as jib prepare wrote it: {spec out deps njobs src build platform testsRun}
export def ctx []: nothing -> record<spec: record, out: string, dest: string, deps: list<record<name: string, root: string>>, roots: list<string>, njobs: int, src: string, build: string, platform: record, testsRun: bool, cache: bool> {
  open $"($env.NIX_BUILD_TOP)/ctx.json"
}

# a build system's options, defaults already merged by Nix
export def options [system: string]: nothing -> record { (ctx).spec | get $system }

# the store path of dependency `name` (by its exports name), or an error naming `why`
export def dep-root [name: string, why: string]: nothing -> string {
  let found = ((ctx).deps | where name == $name)
  if ($found | is-empty) { error make {msg: $"($why): pkgs.($name) must be in dependencies"} }
  $found.0.root
}

# the store path of build dependency `name`, for tools that are not a bin/ on PATH
export def tool-root [name: string]: nothing -> string {
  let found = ((attrs).buildDependencies | where { (^jib exports $in | from json | first | get name) == $name })
  if ($found | is-empty) { error make {msg: $"buildPkgs.($name) must be in buildDependencies"} }
  $found.0
}

# copy `names` from `dir` into $out/bin. finish checks afterwards that every `bin` of the spec exists
export def install-bins [dir: string, names: list<string>]: nothing -> nothing {
  let out = (ctx).out
  mkdir $"($out)/bin"
  for name in $names { cp $"($dir)/($name)" $"($out)/bin/($name)" }
}

# run an external and print its command line first, the build log is this tree's `set -x`
export def --wrapped x [command: string, ...args: string]: nothing -> nothing {
  print -e $"+ ($command) ($args | str join ' ')"
  ^$command ...$args
}

# absolute path of a tool on PATH
export def tool [name: string]: nothing -> path {
  let found = (which $name)
  if ($found | is-empty) { error make {msg: $"($name) not on PATH"} }
  $found | first | get path
}

# library file names for this build's platform, from the target's `ext` and `binfmt`.
# `shlib z` -> libz.so | libz.dylib | libz.dll, `shlib z 1` -> libz.so.1 | libz.1.dylib | libz-1.dll
export def shlib [name: string, version?: string]: nothing -> string {
  let platform = (ctx).platform
  let suffix = if $version == null { "" } else if $platform.binfmt == "coff" { $"-($version)" } else { $".($version)" }
  if $platform.binfmt == "elf" {
    $"lib($name)($platform.ext.shared)($suffix)"
  } else {
    $"lib($name)($suffix)($platform.ext.shared)"
  }
}

# what `-l<name>` resolves to: the import library where the platform has those, else the shared one
export def linklib [name: string]: nothing -> string {
  let ext = (ctx).platform.ext
  $"lib($name)($ext.import? | default $ext.shared)"
}

# the derivation's structured attrs (nix/package `common`)
export def attrs []: nothing -> record { open $env.NIX_ATTRS_JSON_FILE }

# rewrite a text file in place, even one installed read-only: `edit $f { str replace a b }`
export def edit [file: path, change: closure]: nothing -> nothing {
  let text = (open --raw $file | do $change)
  chmod u+w $file
  $text | save -f $file
}

# Build records (Config.pm, Makefile.global) name the tools configure found by store path. As
# bare names they resolve to whatever is on PATH where the record is later used
export def tools-by-name []: string -> string { $in | str replace -ar '/nix/store/[a-z0-9]{32}-[^/]+/bin/' "" }

# by magic number: \x7fELF, and the two Mach-O ones (thin and fat)
export def is-elf [file: path]: nothing -> bool { (open --raw $file | first 4) == 0x[7f 45 4c 46] }
export def is-macho [file: path]: nothing -> bool {
  let magic = (open --raw $file | first 4)
  $magic == 0x[cf fa ed fe] or $magic == 0x[ca fe ba be]
}

# bin/<name> as a launch record (pkgs/la/launch): `program` with `args` before the user's, `vars`
# name -> value set for it. For entry points that are not files with a #! line (escripts)
export def write-launcher [name: string, program: string, args: list<string>, vars: record = {}]: nothing -> nothing {
  ^jib launcher $name $program ...$args --env ($vars | to json -r)
}

# no /usr/bin/env in the sandbox: point such scripts at the build PATH's env (build tree only;
# finish turns installed copies back with --undo and bin/ scripts get launchers). mtimes are kept
# in the build tree: a generator script newer than its shipped output makes make regenerate it
# (coreutils' cu-progs.m4 -> aclocal, ruby's prism templates -> baseruby)
export def fix-shebangs [dir: path, --undo]: nothing -> nothing {
  ^jib shebangs $dir ...(if $undo { [--undo] } else { [] })
}
