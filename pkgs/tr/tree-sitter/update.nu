# the grammars xtask fetch-fixtures clones for the tests, a branch pinned to its head commit
export def sources [pin: record]: nothing -> record {
   let fixtures = (get-json $"https://raw.githubusercontent.com/tree-sitter/tree-sitter/($pin.tag)/test/fixtures/fixtures.json")

   $fixtures | reduce --fold {} {|entry, found|
      let repo = $"tree-sitter/tree-sitter-($entry.0)"
      let rev  = if $entry.2 == null {
         $"refs/tags/($entry.1)"
      } else {
         get-json $"https://api.github.com/repos/($repo)/branches/($entry.2)" | get commit.sha
      }
      $found | insert $"fixture-($entry.0)" {
         url:  $"https://github.com/($repo)/archive/($rev).tar.gz"
         name: $"tree-sitter-($entry.0)-($entry.2 | default $entry.1)"
      }
   }
}

def get-json [url: string]: nothing -> any {
   let answer = (^spieler get $url | from json)
   if $answer.status != 200 { error make {msg: $"($url) answered ($answer.status)"} }
   $answer.body
}
