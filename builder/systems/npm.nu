# npm install from the fetched tree, run <script>, test, and the pruned tree or compiled bins, jig phase npm <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup npm | from json) }
export def workdir []: nothing -> string { ^jig system workdir npm | str trim }
export def build []: nothing -> nothing { ^jig system phase npm build }
export def test []: nothing -> nothing { ^jig system phase npm test }
export def install []: nothing -> nothing { ^jig system phase npm install }
