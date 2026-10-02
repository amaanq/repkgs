# `completions.<shell> = [ files ]`, sugar for `install`: each file goes where that shell
# looks. bash and zsh find completions by command name (`<cmd>`, `_<cmd>`), fish and nu keep the
# file name. A ./path beside package.nix arrives with a store hash prefix, dropped here
let
  inherit (builtins)
    attrNames
    concatMap
    elemAt
    listToAttrs
    match
    ;
  directories = {
    bash = "share/bash-completion/completions";
    zsh = "share/zsh/site-functions";
    fish = "share/fish/vendor_completions.d";
    nu = "share/nushell/vendor/autoload";
  };
  installedName =
    shell: file:
    let
      parts = match "([a-z0-9]{32}-)?((.*)\\.${shell}|.*)" (baseNameOf file);
      whole = elemAt parts 1;
      stem = if elemAt parts 2 != null then elemAt parts 2 else whole;
    in
    if shell == "bash" then
      stem
    else if shell == "zsh" then
      (if match "_.*" stem != null then stem else "_" + stem)
    else
      whole;
in
shells:
listToAttrs (
  concatMap (
    shell:
    map (file: {
      name = "${directories.${shell}}/${installedName shell file}";
      value = file;
    }) shells.${shell}
  ) (attrNames shells)
)
