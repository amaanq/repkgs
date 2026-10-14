{ pkgs, ... }:
[ { env.KDIR = "${pkgs.linux-kernel.dev}/lib/modules/${pkgs.linux-kernel.version}/build"; } ]
