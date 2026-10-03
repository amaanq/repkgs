# The phase list and the plan hoist runs from it. `install`/`links` alone (prebuilt binaries, data):
# the one phase is copying them into $out. `phases` is the whole list, or edits to the first build
# system's list (README):
# { before.<phase> = [..]; after.<phase> = [..]; replace.<phase> = phase | [..]; remove = [..]; }
{
  buildSystems,
  uses,
  args,
  written,
  fail,
  dirs,
  prelude,
  separate,
}:
let
  inherit (builtins)
    attrNames
    concatMap
    elem
    elemAt
    filter
    head
    isAttrs
    isList
    length
    match
    tail
    ;
  pattern = "([a-z][a-z0-9-]*)\\.([a-zA-Z][a-zA-Z0-9-]*)";
  edits = [
    "before"
    "after"
    "replace"
    "remove"
  ];
  asList = value: if isList value then value else [ value ];
  edited =
    edit:
    let
      known = buildSystems.${head uses}.phases;
      # an edit gated off with `on cond [ … ]` names a phase of the other platform's build system
      nonEmpty = set: filter (phase: set.${phase} != [ ]) (attrNames set);
      named =
        nonEmpty (edit.before or { })
        ++ nonEmpty (edit.after or { })
        ++ attrNames (edit.replace or { })
        ++ (edit.remove or [ ]);
      unknown = filter (phase: !elem phase known) named;
      badEdits = filter (key: !elem key edits) (attrNames edit);
    in
    if badEdits != [ ] then
      fail "phases: unknown edit ${head badEdits} (before, after, replace, remove)"
    else if unknown != [ ] then
      fail "phases: ${head unknown} is not a phase of ${head uses} (${toString known})"
    else
      concatMap (
        phase:
        if elem phase (edit.remove or [ ]) then
          [ ]
        else
          asList (edit.before.${phase} or [ ])
          ++ asList (edit.replace.${phase} or phase)
          ++ asList (edit.after.${phase} or [ ])
      ) known;
  list =
    if (written.phases or [ ]) == [ ] then
      if length uses == 1 then
        buildSystems.${head uses}.phases
      else if uses == [ ] && (args ? install || args ? links) then
        [ ]
      else
        fail "'phases' is required with more than one build system (a list, or edits to the first one's)"
    else if isList written.phases then
      written.phases
    else
      edited written.phases;
  prefix = phase: if isAttrs phase then null else match pattern phase;
  # a phase as a plan step for hoist: a build system's verb runs in hoist, anything else is nu text,
  # inline or a package module's command. A prefix that is neither: nix reports "path …/<bs>.nu
  # does not exist". An inline phase's env changes carry over to later phases, its cwd does not
  step =
    phase:
    let
      parts = prefix phase;
    in
    if isAttrs phase then
      {
        kind = "nu";
        inherit (phase) name;
        test = phase.name == "test";
        body = "do --env {\nlet c = (ctx)\n${phase.run}\n}";
      }
    else if parts == null then
      fail "phase '${phase}' is not <build system or module>.<phase>"
    else if elem (elemAt parts 0) uses then
      {
        kind = "system";
        name = phase;
        system = elemAt parts 0;
        verb = elemAt parts 1;
        test = elemAt parts 1 == "test";
      }
    else
      {
        kind = "nu";
        name = phase;
        test = elemAt parts 1 == "test";
        body = "${elemAt parts 0} ${elemAt parts 1}";
      };
  distinct =
    seen: names:
    if names == [ ] then
      [ ]
    else if elem (head names) seen then
      distinct seen (tail names)
    else
      [ (head names) ] ++ distinct (seen ++ [ (head names) ]) (tail names);
  # a phase "zig.restore" whose prefix is no `uses` entry is the package's own module zig.nu next
  # to package.nix, for phases too long to read inline. It imports the builder by bare name
  modules =
    if !(args ? phases) then
      [ ]
    else
      distinct uses (map head (filter (parts: parts != null) (map prefix list)));
  # <store dir>/<m>.nu: nu names a module after its file, a bare store path would be <hash>-<m>
  module =
    name:
    let
      found = filter (dir: builtins.pathExists (dir + "/${name}.nu")) dirs;
    in
    if found == [ ] then
      fail "phases: no ${name}.nu beside recipe.toml"
    else
      "${
        builtins.path {
          path = head found;
          name = "module";
          filter = path: _: baseNameOf path == "${name}.nu";
        }
      }/${name}.nu";
  steps = map step list;
  header = builtins.concatStringsSep "\n" ([ prelude ] ++ map (name: "use ${module name}") modules);
in
{
  inherit list steps;
  modules = map module modules;
  plan = {
    prelude = header;
    inherit separate;
    setups = uses;
    phases = steps;
  };
  # what nu parses of a package, for lints/package-scripts.nu
  script = builtins.concatStringsSep "\n" (
    [ header ] ++ map (step: step.body) (filter (step: step.kind == "nu") steps)
  );
}
