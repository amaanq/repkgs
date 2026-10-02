# The one way to list paths by pattern, lints/nu/bare-glob.yml bans `glob` everywhere else. nu's
# glob returns directory order, which leaked into an archive as member order and Nix refused the
# differing rebuild of a content-addressed derivation. Sorted, and regular files unless --dirs
# (directories only) or --any.
export def files [pattern: string, --dirs, --any, --no-symlink, --exclude: list<string> = []]: nothing -> list<string> {
  let skip_dirs = not ($dirs or $any)
  let skip_files = $dirs and not $any
  glob $pattern --no-dir=$skip_dirs --no-file=$skip_files --no-symlink=$no_symlink --exclude $exclude | sort
}
