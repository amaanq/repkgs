use core.nu [act cue]

# the Makefile has no msvc target. etc/luavs.bat builds lua54.dll, lua.exe against it, and luac.exe
# static because it reaches internals the dll does not export
export def msvc []: nothing -> nothing {
   cd $"((act).src)/src"
   let library = (open --raw Makefile | parse -r '(?m)^(?:CORE_O|LIB_O)=\s*(.*)$' | get capture0 | split row -r '\s+' | where $it != "")
   for object in [...$library lua.o luac.o] {
      cue cc -O2 -DNDEBUG -DLUA_BUILD_AS_DLL -c ($object | str replace .o .c) -o $object
   }
   cue cc -shared -o lua54.dll ...$library -Wl,-implib:lua54.lib
   cue cc -o lua.exe lua.o lua54.lib
   cue cc -o luac.exe luac.o ...$library
}

export def pc []: nothing -> nothing {
   let version = (act).spec.version
   let short   = ($version | split row "." | first 2 | str join ".")
   let dir     = $"((act).out)/lib/pkgconfig"
   let text    = [
      "prefix=${pcfiledir}/../.."
      "libdir=${prefix}/lib"
      "includedir=${prefix}/include"
      "INSTALL_BIN=${prefix}/bin"
      "INSTALL_INC=${includedir}"
      "INSTALL_LIB=${libdir}"
      "INSTALL_MAN=${prefix}/share/man/man1"
      ""
      "Name: Lua"
      "Description: An Extensible Extension Language"
      $"Version: ($version)"
      "Requires:"
      "Libs: -L${libdir} -llua"
      "Cflags: -I${includedir}"
      ""
   ] | str join "\n"

   mkdir $dir
   $text | save -f $"($dir)/lua.pc"
   for name in [$"lua-($short)" $"lua($short)" $"lua($short | str replace . '')"] {
      ^ln -sf lua.pc $"($dir)/($name).pc"
   }
}
