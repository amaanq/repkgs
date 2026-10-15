use core.nu [act cue test-locale]

def utf8-locale []: nothing -> string { if (act).platform.os == "macos" { "en_US.UTF-8" } else { "C.UTF-8" } }

export def configure []: nothing -> nothing {
   let c = (act)
   [
      $"PREFIX=($c.out)"
      $"SBINDIR=($c.out)/bin"
      $"MANDIR=($c.out)/share/man"
      "CC=cc"
      "AR=llvm-ar"
      "HAVE_WCHAR=1"
      $"UTF8_LOCALE=(utf8-locale)"
      ""
   ] | str join "\n" | save configure.local
   cue sh ./configure
}

export def --env test []: nothing -> nothing {
   let parts = utf8-locale | split row '.'
   test-locale $parts.0 $parts.1
}
