_: [
  {
    cargo.cratePatches = {
      evix = [ ./evix/read-only-schema.patch ];
      nix-bindings-sys = [ ../../ev/evix/nix-bindings-sys-fromattrs.patch ];
    };
  }
]
