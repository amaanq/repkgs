# bundle drv: a package's output as one self-contained tree (jib bundle), for
# tools/upload-bootstrap.nu. Nix supplies the closure and checks that no store reference is left.
{ jib }:
drv:
derivation {
  name = "${drv.name}-bundle";
  inherit (drv) system;
  __structuredAttrs = true;
  __contentAddressed = true;
  outputHashMode = "recursive";
  outputHashAlgo = "sha256";
  builder = "${jib}/bin/jib";
  args = [ "bundle" ];
  package = drv;
  exportReferencesGraph.closure = [ drv ];
  outputChecks.out.allowedReferences = [ ];
}
