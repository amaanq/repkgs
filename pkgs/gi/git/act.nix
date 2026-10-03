{ platform, tools, ... }:
let
  unameO =
    {
      windows = "windows";
      macos = "macos";
    }
    .${platform.os} or "GNU/Linux";
in
[
  {
    autotools.makeFlags = [
      "RUNTIME_PREFIX=YesPlease"
      "gitexecdir=libexec/git-core"
      "template_dir=share/git-core/templates"
      "sysconfdir=/etc"
      "RUST_TARGET_DIR=$(CARGO_TARGET_DIR)/${platform.rustTriple}/release"
      "CURL_LDFLAGS=-lcurl"
      "NO_PERL=1"
      "PERL_PATH="
      "NO_PYTHON=1"
      "NO_GETTEXT=1"
      "uname_S=${platform.osNames.cmake}"
      "uname_M=${platform.cpu}"
      "uname_O=${unameO}"
      "uname_R="
      "NO_INSTALL_HARDLINKS=1"
      "INSTALL_SYMLINKS=1"
    ];
    cargo = {
      deps = null;
      tool = tools.rust-bootstrap;
    };
  }
]
