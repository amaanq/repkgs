use core.nu [test-locale]

# glibc finds a locale by its normalized codeset, which is how the tests spell el_GR.iso88597
export def --env locales []: nothing -> nothing {
   for pair in [[en_US UTF-8] [en_US ISO-8859-1] [fr_FR UTF-8] [ru_RU UTF-8] [ru_RU KOI8-R] [ja_JP UTF-8] [el_GR ISO-8859-7]] {
      test-locale $pair.0 $pair.1
   }
   mv locale/el_GR.ISO-8859-7 locale/el_GR.iso88597
}
