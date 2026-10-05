# NixOS module that runs mummerd as a system service and maps its socket into every build sandbox,
# so every build on the machine uses the compile cache without per-command flags.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.mummerd;
  socket = "/nix/var/nix/mummerd/socket";
in
{
  options.services.mummerd = {
    enable = lib.mkEnableOption "the mummer compile cache daemon";
    package = lib.mkOption {
      type = lib.types.package;
      default = import ./crate.nix {
        inherit pkgs;
        crate = "mummerd";
      };
      description = "The mummerd to run.";
    };
    size = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = 100;
      description = "GiB of compiler results kept before whole packs are evicted, 0 for no limit.";
    };
    remote = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "https://cache.example.org";
      description = "A Bazel HTTP cache, such as bazel-remote, asked on a local miss.";
    };
    remoteTokenFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "A file holding `user:password` for the remote. With one this machine writes to it, without one it only reads. A string, so the secret never lands in the store.";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.mummerd = {
      isSystemUser = true;
      group = "mummerd";
    };
    users.groups.mummerd = { };

    systemd.tmpfiles.rules = [ "d /nix/var/nix/mummerd 0755 mummerd mummerd -" ];

    systemd.services.mummerd = {
      wantedBy = [ "multi-user.target" ];
      after = [ "local-fs.target" ];
      environment = {
        XDG_CACHE_HOME = "/var/cache";
        MUMMERD_SIZE = toString cfg.size;
      }
      // lib.optionalAttrs (cfg.remote != null) { MUMMERD_REMOTE = cfg.remote; }
      // lib.optionalAttrs (cfg.remoteTokenFile != null) {
        MUMMERD_REMOTE_TOKEN = "%d/remote-token";
      };
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/mummerd ${socket}";
        User = "mummerd";
        Group = "mummerd";
        CacheDirectory = "mummerd";
        Restart = "always";
        LimitNOFILE = 524288;
        LoadCredential = lib.optional (cfg.remoteTokenFile != null) "remote-token:${cfg.remoteTokenFile}";
      };
    };

    # not optional, so a dead daemon stops builds instead of letting them compile uncached
    nix.settings.extra-sandbox-paths = [ socket ];
  };
}
