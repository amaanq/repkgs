use core.nu [test-locale]

# test_I needs a UTF-8 locale other than C
export def --env locale []: nothing -> nothing {
   test-locale en_US UTF-8
   $env.LC_ALL = "en_US.UTF-8"
}
