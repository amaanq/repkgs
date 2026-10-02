# `supported` without forcing the derivation (docs/design.md): platforms.{cpu,os,abi,libc,posix,cross},
# a per-cpu tarball in package.toml, and the dependencies' own verdicts. The reason, or null
{
  platform,
  buildSystems,
  name,
  args,
  uses,
  sources,
  sourceKey,
  dependencies,
}:
let
  inherit (builtins) elem filter head;
  wanted = args.platforms or { };
  admits = key: !(wanted ? ${key}) || elem platform.${key} wanted.${key};
  systemReasons = filter (reason: reason != null) (
    map (system: buildSystems.${system}.unsupported) uses
  );
  unsupportedDependencies = filter (dependency: !(dependency.supported or true)) dependencies;
in
if !admits "cpu" then
  "${name}: not for ${platform.cpu} (platforms.cpu)"
else if !admits "os" then
  "${name}: not for ${platform.os} (platforms.os)"
else if !admits "abi" then
  "${name}: not for the ${platform.abi} ABI (platforms.abi)"
else if !admits "libc" then
  "${name}: not on ${platform.libc} (platforms.libc)"
else if (wanted.posix or false) && !platform.posix then
  "${name}: needs a POSIX system (platforms.posix)"
else if (wanted.cross or true) == false && platform.cross then
  "${name}: runs its own binaries while installing, cannot be cross-built (platforms.cross)"
else if systemReasons != [ ] then
  "${name}: ${head systemReasons}"
else if sourceKey != null && !(sources.has sourceKey) then
  "${name}: package.toml has no '${sourceKey}' source"
else if unsupportedDependencies != [ ] then
  "${name} -> ${(head unsupportedDependencies).unsupportedReason}"
else
  null
