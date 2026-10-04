# The helpers every nu phase may use. bigtop runs each nu phase with this directory in the include
# path, and hands what one phase exported to the next.

export use glob.nu *
export use log.nu *

export def ctx []: nothing -> record<spec: record, out: string, dest: string, deps: list<record<name: string, root: string>>, roots: list<string>, njobs: int, src: string, build: string, platform: record, testsRun: bool, cache: bool> {
   open $"($env.NIX_BUILD_TOP)/ctx.json"
}

export def attrs []: nothing -> record { open $env.NIX_ATTRS_JSON_FILE }

# a build system's options
export def options [system: string]: nothing -> record { (ctx).spec | get $system }

# the store path of the dependency exported as `name`, failing with `why` when it is missing
export def dep-root [name: string, why: string]: nothing -> string {
   let found = ((ctx).deps | where name == $name)
   if ($found | is-empty) { error make {msg: $"($why): pkgs.($name) must be in dependencies"} }
   $found.0.root
}

# the store path of a build dependency
export def tool-root [name: string]: nothing -> string {
   let found = ((attrs).buildDependencies | where { (^bigtop exports $in | from json | first | get name) == $name })
   if ($found | is-empty) { error make {msg: $"buildPkgs.($name) must be in buildDependencies"} }
   $found.0
}

# where `name` resolves on PATH
export def tool [name: string]: nothing -> path {
   let found = (which $name)
   if ($found | is-empty) { error make {msg: $"($name) not on PATH"} }
   $found | first | get path
}

# run an external after printing its command line
export def --wrapped x [command: string, ...args: string]: nothing -> nothing {
   print -e $"+ ($command) ($args | str join ' ')"
   ^$command ...$args
}

# a shared library's file name on this platform
export def shlib [name: string, version?: string]: nothing -> string {
   let platform = (ctx).platform
   let suffix   = if $version == null { "" } else if $platform.binfmt == "coff" { $"-($version)" } else { $".($version)" }

   if $platform.binfmt == "elf" {
      $"lib($name)($platform.ext.shared)($suffix)"
   } else {
      $"lib($name)($suffix)($platform.ext.shared)"
   }
}

# the file `-l<name>` resolves to, the import library on platforms that have them
export def linklib [name: string]: nothing -> string {
   let ext = (ctx).platform.ext
   $"lib($name)($ext.import? | default $ext.shared)"
}

# copy `names` from `dir` into $out/bin
export def install-bins [dir: string, names: list<string>]: nothing -> nothing {
   let out = (ctx).out
   mkdir $"($out)/bin"
   for name in $names { cp $"($dir)/($name)" $"($out)/bin/($name)" }
}

# rewrite a text file in place, read-only ones too
export def edit [file: path, change: closure]: nothing -> nothing {
   let text = (open --raw $file | do $change)
   chmod u+w $file
   $text | save -f $file
}

# drop the store directories from tool paths in records like Config.pm or Makefile.global, so
# whoever reads them later finds the tools on their own PATH
export def tools-by-name []: string -> string {
   str replace --all --regex '/nix/store/[a-z0-9]{32}-[^/]+/bin/' ""
}

# bin/<name> as a launch record, for entry points without a #! line like escripts
export def write-launcher [name: string, program: string, args: list<string>, vars: record = {}]: nothing -> nothing {
   ^bigtop launcher $name $program ...$args --env ($vars | to json -r)
}

# no /usr/bin/env in the sandbox, so build-tree scripts point at the env on PATH until finish undoes it
export def fix-shebangs [dir: path, --undo]: nothing -> nothing {
   ^bigtop shebangs $dir ...(if $undo { [--undo] } else { [] })
}

# a locale for a test suite under ./locale, LOCPATH pointing there. glibc's tools keep their
# build-time interpreter, so the sysroot's localedef runs under the sysroot's loader
export def --env test-locale [locale: string, charmap: string]: nothing -> nothing {
   let sysroot = (ctx).platform.sysroot
   mkdir locale
   with-env {I18NPATH: $"($sysroot)/share/i18n"} {
      (x $"($sysroot)/lib/((ctx).platform.interp | path basename)" --library-path $"($sysroot)/lib"
         $"($sysroot)/bin/localedef" --no-archive -i $locale -f $charmap $"./locale/($locale).($charmap)")
   }
   $env.LOCPATH = $"($env.PWD)/locale"
}
