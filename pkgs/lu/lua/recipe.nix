{ platform, when, ... }:
[
  (when (platform.os == "linux") { make.buildTarget = [ "linux" ]; })
  (when (platform.os == "macos") { make.buildTarget = [ "macosx" ]; })
  (when (platform.libc == "mingw") {
    install."bin/" = [ "src/lua54.dll" ];
    make.buildTarget = [ "mingw" ];
  })
  (when (platform.libc == "msvc") {
    install = {
      "bin/" = [
        "src/lua.exe"
        "src/luac.exe"
        "src/lua54.dll"
      ];
      "include/" = [
        "src/lua.h"
        "src/luaconf.h"
        "src/lualib.h"
        "src/lauxlib.h"
        "src/lua.hpp"
      ];
      "lib/lua54.lib" = "src/lua54.lib";
    };
    phases = [ "lua.msvc" ];
  })
  (when (platform.libc != "msvc") {
    make = {
      flags = [
        "MYCFLAGS=-fPIC"
        "INSTALL_TOP=$(prefix)"
      ];
      programs = [
        "lua"
        "luac"
      ];
      testTarget = [ "test" ];
    };
    systems = [ "make" ];
  })
]
