use core.nu [ctx cue]

# automake's py-compile records the install path in each .pyc, so recompile them with the prefix
# stripped, as the python system does
export def recompile []: nothing -> nothing {
   let out = (ctx).out

   cue python3 -m compileall -q -f -o 0 -o 1 -o 2 -s $out $"($out)/lib"
}
