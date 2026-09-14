# bun install from the fetched tree, run <script>, test, and the pruned tree or compiled bins, jig phase bun <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup bun | from json) }
export def workdir []: nothing -> string { ^jig system workdir bun | str trim }
export def build []: nothing -> nothing { ^jig system phase bun build }
export def test []: nothing -> nothing { ^jig system phase bun test }
export def install []: nothing -> nothing { ^jig system phase bun install }
