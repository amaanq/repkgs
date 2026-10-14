use core.nu [act test-locale]

# the bundled test fonts stand in for the DejaVu and Cantarell upstream's CI installs
export def --env test-env []: nothing -> nothing {
   let c    = (act)
   let conf = $"($c.build)/test-fonts.conf"

   $"<?xml version=\"1.0\"?>
<!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\">
<fontconfig>
  <dir>($c.src)/tests/fonts</dir>
  <cachedir>($c.build)/fontconfig-cache</cachedir>
</fontconfig>
" | save -f $conf

   $env.FONTCONFIG_FILE = $conf
   test-locale en_US UTF-8
}
