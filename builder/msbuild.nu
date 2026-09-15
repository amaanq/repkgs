export def evaluate [file: path, seeds: record, --itemdefs: record = {}]: nothing -> record {
  {properties: $seeds, itemdefs: $itemdefs} | to json | ^jib msbuild $file | from json
}

export def win-path [p: string, dir: string]: nothing -> string {
  let s = ($p | str replace -a '\' '/')
  if ($s | str starts-with "/") { $s } else { $dir | path join $s | path expand -n }
}

