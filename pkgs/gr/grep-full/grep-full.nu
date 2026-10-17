use core.nu [test-locale]

export def --env locales []: nothing -> nothing {
   for pair in [[en_US UTF-8] [de_DE UTF-8] [fr_FR UTF-8] [fr_FR ISO-8859-1] [cs_CZ UTF-8] [tr_TR UTF-8] [el_GR ISO-8859-7] [ru_RU KOI8-R] [ja_JP EUC-JP] [ja_JP SHIFT_JIS] [zh_CN GB18030] [zh_HK BIG5-HKSCS]] {
      test-locale $pair.0 $pair.1
   }
}
