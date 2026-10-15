use core.nu [test-locale]

export def --env test []: nothing -> nothing {
   test-locale en_US UTF-8
}
