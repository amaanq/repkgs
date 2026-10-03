use core.nu *

# the Makefile has no msvc target. etc/luavs.bat builds lua54.dll, lua.exe against it, and luac.exe
# static because it reaches internals the dll does not export
export def msvc []: nothing -> nothing {
   cd $"((ctx).src)/src"
   let library = (open --raw Makefile | parse -r '(?m)^(?:CORE_O|LIB_O)=\s*(.*)$' | get capture0 | split row -r '\s+' | where $it != "")
   for object in [...$library lua.o luac.o] {
      x cc -O2 -DNDEBUG -DLUA_BUILD_AS_DLL -c ($object | str replace .o .c) -o $object
   }
   x cc -shared -o lua54.dll ...$library -Wl,-implib:lua54.lib
   x cc -o lua.exe lua.o lua54.lib
   x cc -o luac.exe luac.o ...$library
}
