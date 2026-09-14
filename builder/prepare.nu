# Everything before the first build-system phase, `jib prepare`: the environment, the platform,
# the source tree (unpacked and patched, or restored for a tests derivation), and the `ctx`
# record every later step reads
use core.nu *

# --env: the environment and the cd must outlive the call
export def --env main [
  systems: record # build system name -> its OPTIONS table, from the generated script
  --from-tree: string = ""  # tests derivation: restore the package's tree instead of unpacking
]: nothing -> nothing {
  $env.PATH = ((attrs).buildDependencies | each { $"($in)/bin" })
  let prepared = (^jib prepare ...(if $from_tree == "" { [] } else { [--from-tree $from_tree] }) | from json)
  load-env $prepared.env
  cd $prepared.cwd
  $env.PKGS_CTX = $prepared.ctx
}
