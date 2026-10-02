# `rustToolchain`: a finished rust in the store standing in for the set's own, so a change to jig
# does not wait on the from-source compiler. Native only, checked for the pieces cargo needs
{
  rustToolchain,
  triple,
}:
let
  path =
    if builtins.isString rustToolchain || builtins.isPath rustToolchain then
      builtins.storePath rustToolchain
    else
      throw "rustToolchain must be an existing store path";
  rust = {
    pname = "rust";
    outPath = toString path;
  };
in
{
  inherit path;
  packages =
    if rustToolchain == null then
      { }
    else
      {
        inherit rust;
        rust-bootstrap = rust;
      };
  missing = builtins.filter (part: !builtins.pathExists "${path}/${part}") [
    "bin/rustc"
    "bin/rustdoc"
    "bin/cargo"
    "lib/rustlib/${triple}/lib"
  ];
}
