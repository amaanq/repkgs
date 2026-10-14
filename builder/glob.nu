# Paths matching a pattern, sorted, regular files unless --dirs (directories only) or --any.
# lints/nu/bare-glob.yml keeps nu's own glob out of the tree because it lists in directory order,
# which once became an archive's member order and made a content-addressed rebuild differ
export def glob-files [
   pattern: string
   --dirs
   --any
   --no-symlink
   --exclude: list<string> = []
]: nothing -> list<string> {
   let skip_dirs  = not ($dirs or $any)
   let skip_files = $dirs and not $any
   glob $pattern --no-dir=$skip_dirs --no-file=$skip_files --no-symlink=$no_symlink --exclude $exclude | sort
}
