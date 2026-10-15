use core.nu [act build-dep cue glob-files]

# fixtures_spec expects the tree it runs in to be a directory named busted, as a checkout is
export def test []: nothing -> nothing {
   let c     = (act)
   let tree  = ($c.build | path join busted)
   let trees = [$c.out (build-dep luacov) (build-dep moonscript)]
   let lua   = (glob-files --dirs $"($c.out)/share/lua/*" | first | path basename)
   cp -r $c.src $tree
   $env.LUA_PATH  = ($trees | each {|root| $"($root)/share/lua/($lua)/?.lua;($root)/share/lua/($lua)/?/init.lua" } | append ';' | str join ';')
   $env.LUA_CPATH = ($trees | each {|root| $"($root)/lib/lua/($lua)/?.so" } | append ';' | str join ';')
   cd $tree
   cue ($c.out | path join bin busted) --coverage --lpath= --cpath= --output=gtest
}
