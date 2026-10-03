use core.nu *

# the X11 and AWT libraries need libraries a headless build has none of
export def headless []: nothing -> nothing {
   rm -f lib/libawt_xawt.so lib/libsplashscreen.so lib/libjawt.so
}
