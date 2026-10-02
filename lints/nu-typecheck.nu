# nu's own parser and type checker over each file, with the nu that runs builds. cell-path-types
# gives `$record.field` the field's declared type, so a typed record is checked where it is read
def main [...files: path] {
  let includes = ([builder tests/builder/lib] | each {|dir| $env.PWD | path join $dir } | str join (char record_sep))
  let failed = ($files | par-each {|file|
    let errors = (^$nu.current-exe --no-config-file $"--include-path=($includes)" "--experimental-options=[cell-path-types]" --ide-check 50 $file
      | lines
      | each { from json }
      | where type == diagnostic and severity == Error
      | each {|diagnostic| $"($diagnostic.span.start): ($diagnostic.message)" })
    if ($errors | is-empty) { null } else { $"($file):\n($errors | str join "\n")" }
  } | compact)
  if ($failed | is-not-empty) {
    print -e ($failed | str join "\n")
    exit 1
  }
}
