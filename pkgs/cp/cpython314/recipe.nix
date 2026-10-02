{
  platform,
  pkgs,
  source,
  when,
  ...
}:
[
  {
    autotools.flags = [
      "--disable-test-modules"
      "--without-ensurepip"
      "--with-openssl=${pkgs.openssl}"
      "--with-system-libmpdec"
      "ac_cv_file__dev_ptmx=yes"
      "ac_cv_file__dev_ptc=no"
      "ac_cv_path_LLVM_PROFDATA=llvm-profdata"
      "ac_cv_path_ac_pt_LLVM_PROFDATA=llvm-profdata"
      "ac_cv_path_LLVM_AR=llvm-ar"
      "ac_cv_path_ac_pt_LLVM_AR=llvm-ar"
    ];
  }
  (when platform.cross {
    autotools.flags = [
      "--with-build-python=python3"
      "ac_cv_buggy_getaddrinfo=no"
    ];
    tools = [ "cpython" ];
  })
  (when (platform.libc == "msvc") {
    after."vcxproj.install" = [ "cpython.windows-layout" ];
    before."vcxproj.configure" = [
      "cpython.properties"
      "cpython.frozen-modules"
      "cpython.zlib-ng-headers"
    ];
    bin = [ "python" ];
    links."bin/python" = "python";
    systems = [ "vcxproj" ];
    vcxproj = {
      exclude = [
        "_freeze_module"
        "pylauncher"
        "pywlauncher"
        "pyshellext"
      ];
      install = {
        Application = ".";
        DynamicLibrary = "DLLs";
      };
      projects = [ "PCbuild/pcbuild.proj" ];
      properties = {
        bz2Dir = "${pkgs.bzip2.src}/";
        lzmaDir = "${toString (source "xz")}/";
        mpdecimalDir = "${pkgs.mpdecimal.src}/";
        sqlite3Dir = "${toString (source "sqlite")}/";
        zlibNgDir = "${toString (source "zlib-ng")}/";
        zstdDir = "${pkgs.zstd.src}/";
      };
    };
  })
  (when (platform.libc != "msvc") {
    after."autotools.install" = [ "cpython.build-details" ];
    bin = [ "python3" ];
    needs = [
      "zlib"
      "xz"
      "bzip2"
      "libffi"
      "openssl"
      "expat"
      "sqlite"
      "mpdecimal"
    ];
    systems = [ "autotools" ];
  })
]
