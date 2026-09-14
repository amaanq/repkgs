# meson setup / ninja / meson test / meson install, jig phase meson <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup meson | from json) }
export def workdir []: nothing -> string { ^jig system workdir meson | str trim }
export def configure []: nothing -> nothing { ^jig system phase meson configure }
export def build []: nothing -> nothing { ^jig system phase meson build }
export def test []: nothing -> nothing { ^jig system phase meson test }
export def install []: nothing -> nothing { ^jig system phase meson install }
