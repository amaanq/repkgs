# pnpm install from the fetched tree, run <script>, test, and the pruned tree or compiled bins, jig phase pnpm <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup pnpm | from json) }
export def workdir []: nothing -> string { ^jig system workdir pnpm | str trim }
export def build []: nothing -> nothing { ^jig system phase pnpm build }
export def test []: nothing -> nothing { ^jig system phase pnpm test }
export def install []: nothing -> nothing { ^jig system phase pnpm install }
