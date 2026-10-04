use core.nu *

# the fixture store names carry the ref fixtures.json pins, tag or branch, and the rest is what
# xtask generate-fixtures and xtask test run, without xtask test's debug profile rebuild
export def test []: nothing -> nothing {
   let c        = (ctx)
   let grammars = $"($c.src)/test/fixtures/grammars"
   let given    = ($env.TREE_SITTER_FIXTURES | split row " " | each {|path| {path: $path, name: ($path | path basename | str substring 33..)} })
   let wanted   = (open $"($c.src)/test/fixtures/fixtures.json" | each {|entry| {grammar: $entry.0, name: $"tree-sitter-($entry.0)-($entry.2? | default $entry.1)"} })

   let missing = ($wanted | where name not-in $given.name | get name)
   let extra   = ($given | where name not-in $wanted.name | get name)
   if ($missing | is-not-empty) or ($extra | is-not-empty) {
      error make {msg: $"act.lock's fixture sources differ from fixtures.json, run spieler rehash tree-sitter, missing: ($missing | str join ', '), extra: ($extra | str join ', ')"}
   }

   mkdir $grammars
   for fixture in $wanted {
      ^cp -r --no-preserve=mode ($given | where name == $fixture.name | first | get path) $"($grammars)/($fixture.grammar)"
   }
   cd $c.src
   x cargo run --release --offline --package xtask -- generate-fixtures
   x cargo test --release --offline --workspace -- --test-threads $"($c.njobs)"
}
