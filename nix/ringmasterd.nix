# NixOS module that runs ringmasterd, which traces test runs with BPF programs so the test cache
# sees reads seccomp cannot, io_uring's among them, and maps its socket into every build sandbox.
# The kernel needs BTF.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.ringmasterd;
  socket = "/nix/var/nix/ringmasterd/socket";
in
{
  options.services.ringmasterd = {
    enable = lib.mkEnableOption "the ringmasterd BPF test tracer";
    package = lib.mkOption {
      type = lib.types.package;
      default = import ./crate.nix {
        inherit pkgs;
        crate = "ringmasterd";
        features = [ "daemon" ];
        env.RINGMASTERD_CLANG = lib.getExe' pkgs.llvmPackages.clang-unwrapped "clang";
      };
      description = "The ringmasterd to run.";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.ringmasterd = {
      isSystemUser = true;
      group = "ringmasterd";
    };
    users.groups.ringmasterd = { };

    systemd.tmpfiles.rules = [ "d /nix/var/nix/ringmasterd 0755 ringmasterd ringmasterd -" ];

    systemd.services.ringmasterd = {
      wantedBy = [ "multi-user.target" ];
      after = [ "local-fs.target" ];
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/ringmasterd ${socket}";
        User = "ringmasterd";
        Group = "ringmasterd";
        AmbientCapabilities = [
          "CAP_BPF"
          "CAP_PERFMON"
        ];
        CapabilityBoundingSet = [
          "CAP_BPF"
          "CAP_PERFMON"
        ];
        Restart = "always";
      };
    };

    nix.settings.extra-sandbox-paths = [ (dirOf socket) ];
  };
}
