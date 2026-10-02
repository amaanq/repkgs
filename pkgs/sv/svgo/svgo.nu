use core.nu *

export def test []: nothing -> nothing {
  x pnpm exec vitest run --exclude test/regression
}
