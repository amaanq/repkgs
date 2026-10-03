use core.nu *

def assignments [pairs: record, format: closure]: nothing -> string {
   $pairs | items {|key, value| do $format $key $value } | each {|line| $"($line)\n" } | str join
}

# where perl looks for its own binary, the root of a relocatable @INC. Configure's Linux probe
# greps `ls -l /proc/self/exe` for "/ls", which our coreutils multicall binary never shows
def self-exe [os: string]: nothing -> list<string> {
   match $os {
      "linux" => [-Dd_procselfexe=define '-Dprocselfexe="/proc/self/exe"']
      "macos" => [-Dusensgetexecutablepath=define]
      _ => []
   }
}

# perl-cross carries one patchset per perl release, a newer point release reuses the last one
def borrow-cross-diffs [version: string]: nothing -> nothing {
   let wanted = $"cnf/diffs/perl5-($version)"
   if ($wanted | path exists) { return }
   let series  = ($version | split row "." | first 2 | str join ".")
   let nearest = (ls cnf/diffs | get name | where ($it | path basename | str starts-with $"perl5-($series).") | sort --natural | last)
   cp -r $nearest $wanted
}

export def configure []: nothing -> nothing {
   let c   = (ctx)
   let out = $c.out
   cd $c.src

   assignments {osvers: gnulinux, myuname: pkgs, myhostname: pkgs, cf_by: pkgs, cf_time: "1970-01-01"} {|key, value| $"($key)=\"($value)\"" }
   | save -f config.over

   let zlib = (dep-root zlib "Compress::Raw::Zlib links it")
   assignments {
      BUILD_ZLIB: False
      INCLUDE: $"($zlib)/include"
      LIB: $"($zlib)/lib"
      OLD_ZLIB: False
      GZIP_OS_CODE: AUTO_DETECT
      USE_ZLIB_NG: False
      ZLIB_INCLUDE: $"($zlib)/include"
      ZLIB_LIB: $"($zlib)/lib"
   } {|key, value| $"($key) = ($value)" }
   | save -f cpan/Compress-Raw-Zlib/config.in

   # installed scripts find perl on PATH, the launchers finish writes pin it
   let flags = [
      $"-Dprefix=($out)" -Dcc=cc -Uinstallusrbinperl -Dinstallstyle=lib/perl5 -Duserelocatableinc ...(self-exe $c.platform.os)
      "-Dstartperl=#!/usr/bin/env perl" -Dperlpath=perl
      $"-Dman1dir=($out)/share/man/man1" $"-Dman3dir=($out)/share/man/man3" "-Accflags=-D_GNU_SOURCE -fno-strict-aliasing"
   ]
   if not $c.platform.cross {
      x $env.CONFIG_SHELL ./Configure -des ...$flags
      return
   }

   x cp -r $"($env.PERL_CROSS)/." .
   x patch -p1 -F0 -i $env.PERL_CROSS_PATCH
   borrow-cross-diffs $c.spec.version
   # Errno_pm.PL and h2ph read the target's C headers below --sysroot
   x env AR=llvm-ar RANLIB=llvm-ranlib READELF=llvm-readelf OBJDUMP=llvm-objdump NM=llvm-nm $env.CONFIG_SHELL ./configure $"--target=($c.platform.gnuTriple)" --host-cc=cc-build $"--sysroot=($env.PERL_SYSROOT)" ...$flags
}

# Config.pm, Config_heavy.pl and config.h keep the build's tool paths, sysroot and dependency
# dirs. Configure records -Dprefix even with -Duserelocatableinc, so ours becomes `.../..` too
export def scrub []: nothing -> nothing {
   let out       = (ctx).out
   let arch      = (files $"($out)/lib/perl5/5.*/*/Config_heavy.pl" | first | path dirname)
   let elsewhere = $"\(?:-I|-L|--sysroot=\)?/nix/store/\(?!($out | path basename)\)[^'\" ]+ ?"
   for record in [Config.pm Config_heavy.pl CORE/config.h] {
      edit $"($arch)/($record)" { tools-by-name | str replace -ar $elsewhere "" | str replace -a $out ".../.." }
   }
   let stale = (open --raw $"($arch)/Config_heavy.pl" | lines | where $it =~ '/nix/store/' and $it !~ $out)
   if ($stale | is-not-empty) { error make {msg: $"Config_heavy.pl still names foreign store paths: ($stale | first)"} }
}
