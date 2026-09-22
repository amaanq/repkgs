use checks.nu *

let cache = ('# comment
//help text
FOO:INTERNAL=1
BAR:STRING=not internal
"odd:key":INTERNAL=a=b;c
TRAIL:INTERNAL=' + "'v '" + '
P:INTERNAL=/nix/store/x/include;/nix/store/y/include
E:INTERNAL=
CR:INTERNAL=z' + "\r\n")
let file = (mktemp)
$cache | save -f $file
let got = (^jib probe cmake-init $file | lines | parse 'set([[{k}]] [[{v}]] CACHE INTERNAL "")')
rm $file
assert "only INTERNAL entries" (($got | get k) == [FOO "odd:key" TRAIL P E CR])
assert "quoted key with a colon" (($got | where k == "odd:key").0.v == "a=b;c")
assert "single quotes around a trailing blank removed" (($got | where k == TRAIL).0.v == "v ")
assert "paths and lists verbatim" (($got | where k == P).0.v == "/nix/store/x/include;/nix/store/y/include")
assert "empty value" (($got | where k == E).0.v == "")
assert "trailing CR dropped" (($got | where k == CR).0.v == "z")
done
