use ../../../bootstrap/lib.nu *

let out = $env.out
mkdir $"($out)/bin"
cp $"($env.prebuilt)/bin/jig" $"($out)/bin/jig"
for name in [clang clang++] { x ln -s jig $"($out)/bin/($name)" }
