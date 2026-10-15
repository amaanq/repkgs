# The helpers every nu phase may use. bigtop runs each nu phase with this directory in the include
# path, and hands what one phase exported to the next.

export use glob.nu [glob-files]
export use log.nu [emit]

export def act []: nothing -> record<spec: record, out: string, dest: string, deps: list<record<name: string, root: string>>, roots: list<string>, njobs: int, src: string, build: string, platform: record, testsRun: bool, cache: bool> {
   open $"($env.NIX_BUILD_TOP)/ctx.json"
}

export def drv []: nothing -> record { open $env.NIX_ATTRS_JSON_FILE }

# a build system's options
export def knobs [system: string]: nothing -> record { (act).spec | get $system }

# the store path of the dependency exported as `name`, failing with `why` when it is missing
export def dep [name: string, why: string]: nothing -> string {
   let found = ((act).deps | where name == $name)
   if ($found | is-empty) { error make {msg: $"($why): pkgs.($name) must be in dependencies"} }
   $found.0.root
}

# the store path of a build dependency
export def build-dep [name: string]: nothing -> string {
   let attrs = (drv)
   let roots = ($attrs.buildDependencies | append ($attrs.testDependencies? | default []))
   let found = ($roots | where { (^bigtop exports $in | from json | first | get name) == $name })
   if ($found | is-empty) { error make {msg: $"buildPkgs.($name) must be in buildDependencies"} }
   $found.0
}

# where `name` resolves on PATH
export def bin [name: string]: nothing -> path {
   let found = (which $name)
   if ($found | is-empty) { error make {msg: $"($name) not on PATH"} }
   $found | first | get path
}

# run an external after printing its command line
export def --wrapped cue [command: string, ...args: string]: nothing -> nothing {
   let line = $"+ ($command) ($args | str join ' ')"
   if ($line | str length) <= 200 {
      print -e $line
   } else {
      let start  = ($line | str substring 0..199 | str replace -r ' [^ ]*$' '')
      let out    = ($args | window 2 | where {|pair| $pair.0 == '-o' } | get 0?.1?)
      let output = if $out == null { '' } else { $" -o ($out)" }
      print -e $"($start) …($output) \(($args | length) args\)"
      print -e $"@nix ({action: msg, level: 4, msg: $line} | to json -r)"
   }
   ^$command ...$args
}

# a shared library's file name on this platform
export def shared-name [name: string, version?: string]: nothing -> string {
   let platform = (act).platform
   let suffix   = if $version == null { "" } else if $platform.binfmt == "coff" { $"-($version)" } else { $".($version)" }

   if $platform.binfmt == "elf" {
      $"lib($name)($platform.ext.shared)($suffix)"
   } else {
      $"lib($name)($suffix)($platform.ext.shared)"
   }
}

# the file `-l<name>` resolves to, the import library on platforms that have them
export def link-name [name: string]: nothing -> string {
   let ext = (act).platform.ext
   $"lib($name)($ext.import? | default $ext.shared)"
}

# copy `names` from `dir` into $out/bin
export def place-bins [dir: string, names: list<string>]: nothing -> nothing {
   let out = (act).out
   mkdir $"($out)/bin"
   for name in $names { cp $"($dir)/($name)" $"($out)/bin/($name)" }
}

# rewrite a text file in place, read-only ones too
export def rewrite [file: path, change: closure]: nothing -> nothing {
   let text = (open --raw $file | do $change)
   chmod u+w $file
   $text | save -f $file
}

# drop the store directories from tool paths in records like Config.pm or Makefile.global, so
# whoever reads them later finds the tools on their own PATH
export def unstore []: string -> string {
   str replace --all --regex '/nix/store/[a-z0-9]{32}-[^/]+/bin/' ""
}

# bin/<name> as a launch record, for entry points without a #! line like escripts
export def launcher [name: string, program: string, args: list<string>, vars: record = {}]: nothing -> nothing {
   ^bigtop launcher $name $program ...$args --env ($vars | to json -r)
}

# no /usr/bin/env in the sandbox, so build-tree scripts point at the env on PATH until finish undoes it
export def shebangs [dir: path, --undo]: nothing -> nothing {
   ^bigtop shebangs $dir ...(if $undo { [--undo] } else { [] })
}

# a locale for a test suite under ./locale, LOCPATH pointing there. glibc's tools keep their
# build-time interpreter, so the sysroot's localedef runs under the sysroot's loader. glibc looks
# up the normalized codeset first
export def --env test-locale [
   locale: string
   charmap: string
   --codeset: string # the name a test asks for, where it is an alias of the charmap
]: nothing -> nothing {
   let sysroot = (act).platform.sysroot
   let folded  = $charmap | str lowercase | str replace -ar '[^a-z0-9]' ''
   let suffix  = $codeset | default (if $folded =~ '^[0-9]+$' { $"iso($folded)" } else { $folded })

   mkdir locale
   with-env {I18NPATH: $"($sysroot)/share/i18n"} {
      (cue $"($sysroot)/lib/((act).platform.interp | path basename)" --library-path $"($sysroot)/lib"
         $"($sysroot)/bin/localedef" --no-archive --no-warnings=ascii
         -i $locale -f $charmap $"./locale/($locale).($suffix)")
   }
   $env.LOCPATH = $"($env.PWD)/locale"
}

