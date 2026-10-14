use core.nu [ctx cue]

export def archive []: nothing -> nothing {
   let c        = (ctx)
   let archives = $"($c.out)/lib/archives"
   load-env {MIX_ENV: "prod", MIX_HOME: $"($c.build)/mix-home", MIX_ARCHIVES: $archives}
   cue mix compile --no-deps-check
   cue mix archive.build -o hex.ez
   mkdir $archives
   cue mix archive.install --force hex.ez
}
