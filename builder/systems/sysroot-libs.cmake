# find_library() returns files inside the sysroot, <sysroot>/usr/lib/libm.so or the macOS SDK's
# libz.tbd and Foo.framework. An installed *Config.cmake or .pc that records such a path ties every
# dependent to this build's sysroot, while -lm and -framework Foo resolve in whatever sysroot links
# later. hoist's cmake system injects this through CMAKE_PROJECT_TOP_LEVEL_INCLUDES with -DPKGS_SYSROOT.
function(find_library result)
  _find_library(${result} ${ARGN})
  set(found "${${result}}")
  string(FIND "${found}" "${PKGS_SYSROOT}/" position)
  if(NOT position EQUAL 0)
    return()
  endif()

  if(found MATCHES "/lib([^/]+)\\.(so|a|tbd|dylib)$")
    set(found "-l${CMAKE_MATCH_1}")
  elseif(found MATCHES "/([^/]+)\\.framework$")
    set(found "-framework ${CMAKE_MATCH_1}")
  else()
    return()
  endif()

  get_property(cached CACHE ${result} PROPERTY TYPE)
  if(cached)
    set_property(CACHE ${result} PROPERTY VALUE "${found}")
  else()
    set(${result} "${found}" PARENT_SCOPE)
  endif()
endfunction()
