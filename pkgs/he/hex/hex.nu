use core.nu *

export def archive []: nothing -> nothing {
  let c = (ctx)
  let archives = $"($c.out)/lib/archives"
  load-env {MIX_ENV: "prod", MIX_HOME: $"($c.build)/mix-home", MIX_ARCHIVES: $archives}
  x mix compile --no-deps-check
  x mix archive.build -o hex.ez
  mkdir $archives
  x mix archive.install --force hex.ez
}
