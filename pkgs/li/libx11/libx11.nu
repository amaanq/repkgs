use core.nu [act]

# the locale data goes through a traditional cpp reading stdin, which clang only is when invoked
# as cpp, and `cc -E` without a file argument is not
export def --env rawcpp []: nothing -> nothing {
   let dir = $"((act).build)/rawcpp"

   mkdir $dir
   ^ln -sf (which clang | get 0.path) $"($dir)/cpp"
   $env.RAWCPP = $"($dir)/cpp"
}
