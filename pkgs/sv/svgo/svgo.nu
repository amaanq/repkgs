use core.nu [x]

# compare.js runs git rev-parse HEAD on import
export def test []: nothing -> nothing {
   x git init -q
   x git -c user.name=motley -c user.email=motley@localhost commit -q --allow-empty -m source
   x pnpm exec vitest run
}
