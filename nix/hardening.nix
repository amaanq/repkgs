# Hardening flags by their nixpkgs names. `default` is on for every package unless the package
# (`cc.hardening.<name> = false`) or the platform (nix/platforms.nix `hardening`) says otherwise.
# jib env applies the result per package. CPU control-flow flags live in platforms.nix march.
rec {
  flags = {
    fortify = [ "-D_FORTIFY_SOURCE=3" ];
    stackprotector = [ "-fstack-protector-strong" ];
    stackclashprotection = [ "-fstack-clash-protection" ];
    trivialautovarinit = [ "-ftrivial-auto-var-init=zero" ];
    format = [
      "-Wformat"
      "-Wformat-security"
      "-Werror=format-security"
    ];
    strictoverflow = [ "-fwrapv" ];
    strictflexarrays = [ "-fstrict-flex-arrays=1" ];
    zerocallusedregs = [ "-fzero-call-used-regs=used-gpr" ];
    noplt = [ "-fno-plt" ];
    libcxxhardening = [ "-D_LIBCPP_HARDENING_MODE=_LIBCPP_HARDENING_MODE_FAST" ];
    relro = [ "-Wl,-z,relro" ];
    bindnow = [ "-Wl,-z,now" ];
    relr = [ "-Wl,-z,pack-relative-relocs" ];
    noexecstack = [ "-Wl,-z,noexecstack" ];
    asneeded = [ "-Wl,--as-needed" ];
  };
  # which go on the c++ line only, which on the link line. The rest are compile flags
  cxx = [ "libcxxhardening" ];
  link = [
    "relro"
    "bindnow"
    "relr"
    "noexecstack"
    "asneeded"
  ];
  # ELF linker and loader features: lld-link and ld64 have no -z, PE and Mach-O no PLT or RELRO
  elfOnly = link ++ [
    "noplt"
    "stackclashprotection"
  ];
  # name -> bool for one platform, before the cpu's own verdicts (platforms.nix `hardening`)
  forPlatform =
    platform:
    builtins.mapAttrs (name: _: platform.binfmt == "elf" || !(builtins.elem name elfOnly)) flags
    // (platform.hardening or { });
  enabledFlags =
    enabled: names: builtins.concatMap (n: if enabled.${n} or false then flags.${n} else [ ]) names;
}
