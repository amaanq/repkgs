use core.nu [act build-dep]

# upstream CI's image targets and expected failures, over DejaVu as on its runners
export def --env test-env []: nothing -> nothing {
   let c     = (act)
   let fonts = (build-dep dejavu-fonts)
   let conf  = $"($c.build)/test-fonts.conf"

   $"<?xml version=\"1.0\"?>
<!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\">
<fontconfig>
  <dir>($fonts)/share/fonts</dir>
  <cachedir>($c.build)/fontconfig-cache</cachedir>
</fontconfig>
" | save -f $conf

   $env.FONTCONFIG_FILE   = $conf
   $env.CAIRO_TEST_TARGET = "image,image16"

   for target in [ image-argb32 image-rgb24 image16-rgb24 ] {
      let names = (open --raw $"($c.src)/.gitlab-ci/ignore-($target).txt" | lines | where $it != "" | str join ",")
      load-env { $"CAIRO_TEST_IGNORE_($target | str replace -a '-' '_')": $names }
   }
}
