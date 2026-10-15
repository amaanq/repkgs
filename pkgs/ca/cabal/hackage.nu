use core.nu [act cue knobs glob-files]

# Cabal-tests' hackage-tests read `$XDG_CACHE_HOME/cabal/packages/<repository>/01-index.tar` for each
# repository in cabal's config, so the vendored hackage set's revisions stand in for Hackage's index
export def --env index []: nothing -> nothing {
   let c      = (act)
   let repo   = (open --raw $"($env.CABAL_DIR)/config" | parse --regex '(?m)^repository (?<name>\S+)' | first | get name)
   let cache  = $"($c.build)/xdg-cache"
   let staged = $"($c.build)/hackage-index"
   let index  = $"($cache)/cabal/packages/($repo)"

   for file in (glob-files $"((knobs cabal).deps)/*.cabal") {
      let id  = ($file | path basename | str replace --regex '\.cabal$' '' | parse --regex '^(?<name>.+)-(?<version>[0-9.]+)$' | first)
      let dir = $"($staged)/($id.name)/($id.version)"
      mkdir $dir
      cp $file $"($dir)/($id.name).cabal"
   }
   mkdir $index
   cue bsdtar --format ustar -cf $"($index)/01-index.tar" -C $staged .

   $env.XDG_CACHE_HOME = $cache
}
