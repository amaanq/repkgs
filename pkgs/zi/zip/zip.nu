use core.nu *

export def build []: nothing -> nothing {
   let c       = (ctx)
   let defines = [UNIX UIDGID_NOT_16BIT LARGE_FILE_SUPPORT UNICODE_SUPPORT HAVE_DIRENT_H HAVE_TERMIOS_H]
   let quiet   = [implicit-int implicit-function-declaration deprecated-non-prototype]
   let cflags  = ([-O2 -I. ...($defines | each {|d| $"-D($d)" }) -std=gnu89 ...($quiet | each {|w| $"-Wno-($w)" })] | str join " ")
   x make -f unix/Makefile zip CC=cc "CPP=cc -E" $"CFLAGS=($cflags)" OCRCU8=crc32_.o LFLAGS2= $"-j($c.njobs)"
}
