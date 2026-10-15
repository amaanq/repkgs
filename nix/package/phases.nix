# Turns a recipe's `phases` into the plan bigtop runs. `phases` is either a full list or edits to
# the first build system's list, as in { before.<phase>, after.<phase>, replace.<phase>, remove }.
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
    concatStringsSep
    elem
    elemAt
    filter
    foldl'
    head
    isAttrs
    isList
    length
    match
    ;

  # edits may name one phase or a list
  asList = value: if isList value then value else [ value ];

  # a phase name split into prefix and verb, null for inline or malformed phases
  parts =
    phase: if isAttrs phase then null else match "([a-z][a-z0-9-]*)\\.([a-zA-Z][a-zA-Z0-9-]*)" phase;

  # the prefix, a build system or one of the package's .nu files
  owner = phase: head (parts phase);

  # the part after the dot, or an inline phase's name
  verb =
    phase:
    if isAttrs phase then
      phase.name
    else if parts phase == null then
      null
    else
      elemAt (parts phase) 1;

  # test phases are the ones the test cache can skip
  isTest = phase: verb phase == "test";

  known = buildSystems.${head uses}.phases;

  edits = if isAttrs (written.phases or [ ]) then written.phases else { };
  before = edits.before or { };
  after = edits.after or { };
  replace = edits.replace or { };
  remove = edits.remove or [ ];

  # an edit gated off with `on cond [ … ]` names a phase of the other platform's build system
  nonEmpty = set: filter (phase: set.${phase} != [ ]) (attrNames set);
  named = nonEmpty before ++ nonEmpty after ++ attrNames replace ++ remove;
  unknown = filter (phase: !elem phase known) named;

  editKeys = [
    "before"
    "after"
    "replace"
    "remove"
  ];
  badEdits = filter (key: !elem key editKeys) (attrNames edits);

  # a build system phase with its before, replace and after edits, or nothing if removed
  expand =
    phase:
    if elem phase remove then
      [ ]
    else
      asList (before.${phase} or [ ])
      ++ asList (replace.${phase} or phase)
      ++ asList (after.${phase} or [ ]);

  edited =
    if badEdits != [ ] then
      fail "phases: unknown edit ${head badEdits} (before, after, replace, remove)"
    else if unknown != [ ] then
      fail "phases: ${head unknown} is not a phase of ${head uses} (${toString known})"
    else
      concatMap expand known;

  list =
    if (written.phases or [ ]) == [ ] then
      if length uses == 1 then
        known
      else if uses == [ ] && (args ? install || args ? links) then
        [ ]
      else
        fail "'phases' is required with more than one build system (a list, or edits to the first one's)"
    else if isList written.phases then
      written.phases
    else
      edited;

  # steps before or in place of a test phase only run with the tests
  setups =
    if edits == { } then
      [ ]
    else
      concatMap (
        phase:
        asList (before.${phase} or [ ]) ++ filter (step: !isTest step) (asList (replace.${phase} or [ ]))
      ) (filter isTest known);

  # build steps always run, setup and test steps only when the tests do
  role =
    phase:
    if elem phase setups then
      "setup"
    else if isTest phase then
      "test"
    else
      "build";

  # a build system's phase runs inside bigtop, the rest run as nu, keeping env but not cwd
  step =
    phase:
    if isAttrs phase then
      {
        kind = "nu";
        inherit (phase) name;
        role = role phase;
        body = "do --env {\nlet c = (act)\n${phase.run}\n}";
      }
    else if parts phase == null then
      fail "phase '${phase}' is not <build system or module>.<phase>"
    else if elem (owner phase) uses then
      {
        kind = "system";
        name = phase;
        system = owner phase;
        verb = verb phase;
        role = role phase;
      }
    else
      {
        kind = "nu";
        name = phase;
        role = role phase;
        body = "${owner phase} ${verb phase}";
      };

  steps = map step list;

  # prefixes that aren't build systems name the package's own .nu files beside act.toml
  modules =
    let
      owners = map owner (filter (phase: parts phase != null) list);
      addNew = seen: name: if elem name seen || elem name uses then seen else seen ++ [ name ];
    in
    if args ? phases then foldl' addNew [ ] owners else [ ];

  # keep the file name, or nu would name the module <hash>-<name> instead of <name>
  module =
    name:
    let
      found = filter (dir: builtins.pathExists (dir + "/${name}.nu")) dirs;

      copy = builtins.path {
        path = head found;
        name = "module";
        filter = path: _: baseNameOf path == "${name}.nu";
      };
    in
    if found == [ ] then fail "phases: no ${name}.nu beside act.toml" else "${copy}/${name}.nu";

  header = concatStringsSep "\n" ([ prelude ] ++ map (name: "use ${module name}") modules);
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

  # what nu parses of a package
  script = concatStringsSep "\n" (
    [ header ] ++ map (step: step.body) (filter (step: step.kind == "nu") steps)
  );
}
