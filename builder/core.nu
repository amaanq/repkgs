# The helpers every nu phase may use. bigtop runs each nu phase with this directory in the include
# path, and hands what one phase exported to the next.

export use glob.nu [files]
export use log.nu [note]

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
      (cue $"($sysroot)/lib/((ctx).platform.interp | path basename)" --library-path $"($sysroot)/lib"
         $"($sysroot)/bin/localedef" --no-archive -i $locale -f $charmap $"./locale/($locale).($charmap)")
   }
   $env.LOCPATH = $"($env.PWD)/locale"
}

# an autoconf cache per configure a build tool runs itself, as hadrian and cabal do, restored
# under `key` and named after the precious variables, so a changed CC starts that one cold. The
# key is kept for store-configures, since this changes CONFIG_SITE, which keys hash
export def --env cache-configures [key: string]: nothing -> nothing {
   let dir = $"((ctx).build)/configure-caches"
   $env.PKGS_CONFIGURES_KEY = $key
   mkdir $dir
   let restored = (try { ^bigtop probe restore-dir $key $dir; true } catch { false })
   if $restored { configure-resolve $dir }
   note configure-caches (if $restored { "restored" } else { "cold" })
   configure-sums $dir | to json | save -f $"($dir).sums"
   let quote = {|text| "'" + ($text | str replace --all "'" "'\\''") + "'" }
   let sites = ($env.CONFIG_SITE? | default "" | split row " " | where $it != "")
   let site = [
      ...($sites | each {|path| $"if test -r (do $quote $path); then . (do $quote $path); fi" })
      $"CONFIG_SITE=(do $quote ($sites | str join ' '))"
      'if [ "$cache_file" = /dev/null ]; then'
      ('  pkgs_cache_base=' + (do $quote $dir) + '/$(pwd | tr / _)')
      '  pkgs_cache_env=$(for pkgs_var in $ac_precious_vars; do eval "printf %s=%s\\\\n \"\$pkgs_var\" \"\${$pkgs_var}\""; done | sed "s|/nix/store/[0-9a-z]\{32\}-|/nix/store/*-|g" | cksum | cut -d" " -f1)'
      '  for pkgs_stale in "$pkgs_cache_base"-*.cache "$pkgs_cache_base"-*.dir; do'
      '    case $pkgs_stale in "$pkgs_cache_base-$pkgs_cache_env".*) ;; *) rm -f "$pkgs_stale" ;; esac'
      '  done'
      '  cache_file=$pkgs_cache_base-$pkgs_cache_env.cache'
      '  pwd > "$pkgs_cache_base-$pkgs_cache_env.dir"'
      'fi'
      ''
   ] | str join "\n"
   $site | save -f $"($dir).site"
   $env.CONFIG_SITE = $"($dir).site"
}

# autoconf aborts when a cached precious variable differs from this run's, and CC names the cc
# toolchain, whose store path moves with every mummer change while probe keys cover what it does
def configure-resolve [dir: string]: nothing -> nothing {
   let roots = ((ctx).roots | append ($env.PKGS_CC? | default "{}" | from json | columns))
   for cache in (files $"($dir)/*.cache") {
      let text = ($roots | reduce --fold (open --raw $cache) {|root, text|
         let name = ($root | path basename | str substring 33.. | str replace --all --regex '([^A-Za-z0-9_-])' '\$1')
         $text | str replace --all --regex $"/nix/store/\\*-($name)\(?![A-Za-z0-9._+?=-]\)" $root
      })
      if ($text | str contains "/nix/store/*-") {
         rm --force $cache ($cache | str replace --regex '[.]cache$' '.dir')
      } else {
         $text | save --force --raw $cache
      }
   }
}

def configure-sums [dir: string]: nothing -> record {
   files $"($dir)/*.cache" | reduce -f {} {|cache, sums| $sums | insert ($cache | path basename) (open --raw $cache | hash sha256) }
}

# a cache whose replayed answers write the same config.status as the run that made it, checked
# the way bigtop's autotools system checks its own, since a cached answer skips the side effects
# of the check that gave it
def configure-replays [cache: string]: nothing -> bool {
   let sidecar = ($cache | str replace --regex '[.]cache$' '.dir')
   if not ($sidecar | path exists) { return false }
   let status = $"(open --raw $sidecar | str trim)/config.status"
   if not ($status | path exists) { return false }
   let before = (open --raw $status)
   let rerun = (do { cd ($status | path dirname); ^sh ./config.status --recheck } | complete)
   let after = (open --raw $status)
   if $after != $before { $before | save --force --raw $status }
   let name = ($cache | path basename)
   if $rerun.exit_code != 0 {
      let error = ($rerun.stderr | lines | last 3 | str join ' | ')
      note configure-caches $"($name) recheck exited ($rerun.exit_code): ($error)"
      return false
   }
   let kept = ($after | lines)
   for line in ($before | lines | where $it not-in $kept | first 3) {
      note configure-caches $"($name) cached run lost ($line | str substring 0..159)"
   }
   $after == $before
}

export def store-configures []: nothing -> nothing {
   let dir = $"((ctx).build)/configure-caches"
   let restored = (open --raw $"($dir).sums" | from json)
   for entry in (configure-sums $dir | transpose name sum) {
      let cache = $"($dir)/($entry.name)"
      if ($restored | get --optional $entry.name) != $entry.sum and not (configure-replays $cache) {
         note configure-caches $"($entry.name) answers change what configure writes, not cached"
         rm --force $cache ($cache | str replace --regex '[.]cache$' '.dir')
      }
   }
   for cache in (files $"($dir)/*.cache") {
      let masked = (open --raw $cache | str replace --all --regex '/nix/store/[0-9a-z]{32}-' '/nix/store/*-')
      $masked | save --force --raw $cache
   }
   ^bigtop probe store-dir $env.PKGS_CONFIGURES_KEY $dir
}
