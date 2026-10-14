use core.nu [build-dep test-locale]

# bare names like pl_PL mean the legacy charset
export def --env test []: nothing -> nothing {
   for locale in [az_AZ de_DE en_GB en_US fa_IR fr_FR gu_IN ja_JP lt_LT pl_PL ru_RU sv_SE th_TH tr_TR] {
      test-locale $locale UTF-8
   }

   let legacy = [
      [en_US ISO-8859-1] [en_GB ISO-8859-1] [de_DE ISO-8859-1] [fr_FR ISO-8859-1] [sv_SE ISO-8859-1]
      [tr_TR ISO-8859-9] [ru_RU ISO-8859-5] [pl_PL ISO-8859-2] [lt_LT ISO-8859-13] [th_TH TIS-620]
      [ja_JP EUC-JP]
   ]

   for pair in $legacy {
      test-locale $pair.0 $pair.1
      ln -s $"($pair.0).($pair.1 | str lowercase | str replace -ar '[^a-z0-9]' '')" $"locale/($pair.0)"
   }

   $env.TZDIR = $"(build-dep tzdata)/share/zoneinfo"
   mkdir $"($env.HOME)/.local"
}
