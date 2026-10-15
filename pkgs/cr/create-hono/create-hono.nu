export def giget-cache []: nothing -> nothing {
   let cached = $"($env.XDG_CACHE_HOME)/bluwy-giget/gh/honojs-starter"
   mkdir $cached
   cp $env.GIGET_STARTER $"($cached)/($env.GIGET_STARTER_REF).tar.gz"
}
