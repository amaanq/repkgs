# The nuff.toml steps that are motley's own lints rather than a formatter's. MOTLEY_NO_EVAL=1 skips
# the package-scripts lint, which has no evaluator inside a build.

def main [] {
   print -e "lints/format.nu nu|package-scripts <files>"
   exit 2
}

# recipes and phase modules name their package by directory
def "main package-scripts" [...files: string]: nothing -> nothing {
   if ($env.MOTLEY_NO_EVAL? | default "") == "1" { return }
   let packaged = ($files | where $it =~ '^pkgs/[^/]+/[^/]+/(act\.(toml|nix)|[^/]+\.nu)$')
   if ($packaged | is-empty) { return }
   ^nu --no-config-file lints/package-scripts.nu ...$packaged
}

def "main nu" [...files: string]: nothing -> nothing {
   ^nu --no-config-file lints/nu-typecheck.nu ...$files
   # ast-grep resolves the rules' `ignores` against the directory of its config
   let config = (mktemp --tmpdir-path $env.PWD --suffix .yml)
   {
      ruleDirs: [($env.PWD | path join lints nu)]
      customLanguages: {nu: {libraryPath: $env.TREE_SITTER_NU, extensions: [nu], expandoChar: "_"}}
      languageGlobs: {nu: [motley target-test]}
   } | to json | save -f $config
   let scan = (^ast-grep scan $"--config=($config)" --report-style=short ...$files | complete)
   rm $config
   print -n $scan.stdout
   if $scan.exit_code != 0 { error make --unspanned {msg: "ast-grep failed"} }
   main package-scripts ...$files
}
