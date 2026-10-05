{
  "CMakeLists.txt" = ''
    cmake_minimum_required(VERSION 3.20)
    project(hello VERSION 1.2 LANGUAGES C)
    include(GNUInstallDirs)
    add_library(greet SHARED greet.c)
    set_target_properties(greet PROPERTIES VERSION 1.2 SOVERSION 1 PUBLIC_HEADER greet.h)
    # a libc library through find_library: exported by name, never as a sysroot/SDK path
    find_library(MATH_LIBRARY m REQUIRED)
    target_link_libraries(greet PUBLIC ''${MATH_LIBRARY})
    add_executable(hello hello.c)
    target_link_libraries(hello PRIVATE greet)
    configure_file(greet.pc.in greet.pc @ONLY)
    install(TARGETS greet hello EXPORT greet)
    install(EXPORT greet DESTINATION ''${CMAKE_INSTALL_LIBDIR}/cmake/greet FILE greetConfig.cmake)
    install(FILES ''${CMAKE_CURRENT_BINARY_DIR}/greet.pc DESTINATION ''${CMAKE_INSTALL_LIBDIR}/pkgconfig)
  '';
  "greet.c" = ''
    #include "greet.h"
    const char *greet(void) { return "hello 1.2"; }
  '';
  "greet.h" = ''
    const char *greet(void);
  '';
  "greet.pc.in" = ''
    prefix=@CMAKE_INSTALL_PREFIX@
    libdir=''${prefix}/lib
    includedir=''${prefix}/include

    Name: greet
    Version: @PROJECT_VERSION@
    Libs: -L''${libdir} -lgreet
    Cflags: -I''${includedir}
  '';
  "hello.c" = ''
    #include <stdio.h>
    #include "greet.h"
    int main(void) { puts(greet()); return 0; }
  '';
}
