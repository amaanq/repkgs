use core.nu *

export def build []: nothing -> nothing {
   let c       = (ctx)
   let defines = [UNIX LARGE_FILE_SUPPORT UNICODE_SUPPORT UNICODE_WCHAR UTF8_MAYBE_NATIVE NO_LCHMOD DATE_FORMAT=DF_YMD]
   let quiet   = [implicit-int implicit-function-declaration deprecated-non-prototype format-security]
   let cflags  = ([-O2 -I. ...($defines | each {|d| $"-D($d)" }) -std=gnu89 ...($quiet | each {|w| $"-Wno-($w)" })] | str join " ")
   x make -f unix/Makefile unzips CC=cc LD=cc AS=cc $"CF=($cflags)" LF2= $"-j($c.njobs)"
}
