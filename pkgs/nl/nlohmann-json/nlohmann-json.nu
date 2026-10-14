use core.nu [act cue]

# the cmake_fetch_content tests clone the source tree as a git repository
export def test []: nothing -> nothing {
   let git = [ -C (act).src -c user.name=motley -c user.email=motley@localhost ]

   cue git ...$git init -q
   cue git ...$git add -A
   cue git ...$git commit -q -m source
}
