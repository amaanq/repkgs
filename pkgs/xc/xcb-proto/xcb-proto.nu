use core.nu [ctx x]

# automake's py-compile records the install path in each .pyc, so recompile them with the prefix
# stripped, as the python system does
export def recompile []: nothing -> nothing {
   let out = (ctx).out

   x python3 -m compileall -q -f -o 0 -o 1 -o 2 -s $out $"($out)/lib"
}
