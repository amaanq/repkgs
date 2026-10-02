{ platform, ... }:
[
  {
    make.flags = [
      "CC=cc"
      "STRIP="
      "MACHINE=${platform.gnuTriple}"
    ];
  }
]
