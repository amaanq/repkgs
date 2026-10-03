# `bigtop bundle` folds a package's closure into one tree with no store references, for
# tools/upload-bootstrap.nu
{ bigtop }:
package:
derivation {
  name = "${package.name}-bundle";
  inherit (package) system;
  inherit package;
  __structuredAttrs = true;
  __contentAddressed = true;
  outputHashMode = "recursive";
  outputHashAlgo = "sha256";
  builder = "${bigtop}/bin/bigtop";
  args = [ "bundle" ];
  exportReferencesGraph.closure = [ package ];
  outputChecks.out.allowedReferences = [ ];
}
