# a Python application from its uv.lock into lib/<name>/site-packages with bin/ entry points, jig phase pyapp <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup pyapp | from json) }
export def workdir []: nothing -> string { ^jig system workdir pyapp | str trim }
export def build []: nothing -> nothing { ^jig system phase pyapp build }
export def test []: nothing -> nothing { ^jig system phase pyapp test }
export def install []: nothing -> nothing { ^jig system phase pyapp install }
