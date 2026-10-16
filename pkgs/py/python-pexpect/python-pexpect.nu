# mandoc's man searches MANPATH, never PATH, and test_pager_as_cat reads `man sleep`
export def --env test-env []: nothing -> nothing {
   let bin = (which --all sleep | where type == external | get 0.path | path dirname)

   $env.MANPATH = ($bin | path dirname | path join share man)
}
