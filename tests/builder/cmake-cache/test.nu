# bigtop probe cmake-init turns a CMakeCache.txt into `set(... CACHE INTERNAL "")` lines
use checks.nu *

let cache = [
  "# comment"
  "//help text"
  "FOO:INTERNAL=1"
  "BAR:STRING=not internal"
  '"odd:key":INTERNAL=a=b;c'
  "TRAIL:INTERNAL='v '"
  "P:INTERNAL=/nix/store/x/include;/nix/store/y/include"
  "E:INTERNAL="
  "CR:INTERNAL=z\r"
] | str join "\n"
let file = (mktemp)
$cache + "\n" | save -f $file
let entries = (^bigtop probe cmake-init $file | lines | parse 'set([[{key}]] [[{value}]] CACHE INTERNAL "")')
rm $file
let value = {|key| ($entries | where key == $key).0.value }

expect "only INTERNAL entries survive, in order" (($entries | get key) == [FOO "odd:key" TRAIL P E CR])
expect "a quoted key may hold a colon"           ((do $value "odd:key") == "a=b;c")
expect "quotes protecting a trailing blank go"   ((do $value TRAIL) == "v ")
expect "store paths and lists pass through"      ((do $value P) == "/nix/store/x/include;/nix/store/y/include")
expect "an empty value stays empty"              ((do $value E) == "")
expect "a CRLF line ending goes"                 ((do $value CR) == "z")
pass
