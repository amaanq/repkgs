# Stock-kernel VM that builds for the isolated store, for hosts whose kernel fails test suites in the
# graph (linux-hardened's 4 GiB PIE base aborts ASan, linux-next restricts AF_ALG binds).
{
  shell,
  publicKey,
  port,
  cores,
  jobs,
  memory,
}:
let
  nixpkgs = import ../nix/nixpkgs.nix;
  system = import (nixpkgs + "/nixos/lib/eval-config.nix") {
    system = "x86_64-linux";
    modules = [
      (nixpkgs + "/nixos/modules/virtualisation/qemu-vm.nix")
      (
        { lib, ... }:
        {
          system.stateVersion = "26.05";
          networking.hostName = "motley-builder";
          virtualisation = {
            inherit cores;
            memorySize = memory;
            diskSize = 400 * 1024;
            graphics = false;
            writableStoreUseTmpfs = false;
            forwardPorts = [
              {
                from = "host";
                host.address = "127.0.0.1";
                host.port = port;
                guest.port = 22;
              }
            ];
          };
          services.openssh = {
            enable = true;
            settings.PasswordAuthentication = false;
          };
          users.users.root.openssh.authorizedKeys.keys = [ publicKey ];
          nix.settings = {
            substituters = lib.mkForce [ ];
            sandbox = true;
            sandbox-fallback = false;
            sandbox-paths = lib.mkForce [ "/bin/sh=${shell}" ];
            extra-sandbox-paths = lib.mkForce [ ];
            max-jobs = jobs;
            cores = cores / jobs;
            max-silent-time = 0;
            timeout = 0;
            system-features = [
              "big-parallel"
              "benchmark"
            ];
          };
        }
      )
    ];
  };
in
system.config.system.build.vm
