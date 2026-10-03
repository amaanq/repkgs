# MSBuild project evaluation through `bigtop msbuild`, for recipes driving Visual Studio projects
export def evaluate [file: path, seeds: record, --itemdefs: record = {}]: nothing -> record {
  {properties: $seeds, itemdefs: $itemdefs} | to json | ^bigtop msbuild $file | from json
}

# a Windows path from a project file, absolute against `dir` unless it already is
export def win-path [path: string, dir: string]: nothing -> string {
  let forward = ($path | str replace -a '\' '/')
  if ($forward | str starts-with "/") { $forward } else { $dir | path join $forward | path expand -n }
}
