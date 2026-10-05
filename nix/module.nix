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
      example = "grpcs://cache.example.org:50052/motley";
      description = "A REAPI cache, such as NativeLink, asked on a local miss, `grpc://` or `grpcs://` for TLS, with the instance name as its path.";
    };
    remoteCa = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "The CA a `grpcs://` remote's certificate must chain to, else the web's roots.";
    };
    remoteIdentityFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "A PEM file with this machine's client certificate and key for an mTLS remote. A string, so the key never lands in the store.";
    };
    remoteWrites = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Upload this machine's results to the remote, which only a trusted builder should.";
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
      // lib.optionalAttrs (cfg.remoteCa != null) { MUMMERD_REMOTE_CA = cfg.remoteCa; }
      // lib.optionalAttrs (cfg.remoteIdentityFile != null) {
        MUMMERD_REMOTE_IDENTITY = "%d/remote-identity";
      };
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/mummerd ${socket}${lib.optionalString cfg.remoteWrites " --remote-writes"}";
        User = "mummerd";
        Group = "mummerd";
        CacheDirectory = "mummerd";
        Restart = "always";
        LimitNOFILE = 524288;
        LoadCredential = lib.optional (
          cfg.remoteIdentityFile != null
        ) "remote-identity:${cfg.remoteIdentityFile}";
      };
    };

    # not optional, so a dead daemon stops builds instead of letting them compile uncached
    nix.settings.extra-sandbox-paths = [ socket ];
  };
}
