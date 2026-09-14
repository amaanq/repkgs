# yarn install from the fetched tree, run <script>, test, and the pruned tree or compiled bins, jig phase yarn <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup yarn | from json) }
export def workdir []: nothing -> string { ^jig system workdir yarn | str trim }
export def build []: nothing -> nothing { ^jig system phase yarn build }
export def test []: nothing -> nothing { ^jig system phase yarn test }
export def install []: nothing -> nothing { ^jig system phase yarn install }
