# A package's output as one self-contained tree (`bigtop bundle`), for tools/upload-bootstrap.nu.
# Nix supplies the closure and checks that no store reference is left.
{ bigtop }:
package:
derivation {
  name = "${package.name}-bundle";
  inherit (package) system;
  __structuredAttrs = true;
  __contentAddressed = true;
  outputHashMode = "recursive";
  outputHashAlgo = "sha256";
  builder = "${bigtop}/bin/bigtop";
  args = [ "bundle" ];
  inherit package;
  exportReferencesGraph.closure = [ package ];
  outputChecks.out.allowedReferences = [ ];
}
