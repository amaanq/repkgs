use core.nu [test-locale]

export def --env locales []: nothing -> nothing {
   let locales = [
      [locale   charmap];
      [en_US    UTF-8]
      [en_US    ISO-8859-1]
      [fr_FR    UTF-8]
      [ru_RU    UTF-8]
      [ru_RU    KOI8-R]
      [ja_JP    UTF-8]
      [el_GR    ISO-8859-7]
   ]

   for entry in $locales {
      test-locale $entry.locale $entry.charmap
   }
}
