use core.nu [cue]

# compare.js runs git rev-parse HEAD on import
export def test []: nothing -> nothing {
   cue git init -q
   cue git -c user.name=motley -c user.email=motley@localhost commit -q --allow-empty -m source
   cue pnpm exec vitest run
}
