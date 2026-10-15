use core.nu [test-locale]

export def --env locales []: nothing -> nothing {
   test-locale en_US UTF-8
   test-locale de_DE UTF-8
   test-locale fr_FR ISO-8859-1
   test-locale zh_TW BIG5
   test-locale ja_JP SHIFT_JIS --codeset sjis
   test-locale ru_RU CP1251
}

export def --env home []: nothing -> nothing {
   mkdir home
   $env.HOME = $"($env.PWD)/home"
}
