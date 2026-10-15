use core.nu [act dep build-dep cue shebangs glob-files]

# rbconfig.rb records the build's shell and compiler banner, the latter with clang's install dir
export def relocate-rbconfig []: nothing -> nothing {
   let bash = $"(dep bash "rbconfig SHELL")/bin/bash"
   for config in (glob-files --any $"((act).out)/lib/ruby/*/*/rbconfig.rb") {
      let text = (open --raw $config
         | str replace -r '(CONFIG\["SHELL"\] = ")[^"]*' $"${1}($bash)"
         | str replace -r '(CONFIG\["CC_VERSION_MESSAGE"\] = "[^"]*?)\\nInstalledDir: [^"]*' '${1}')
      $text | save -f $config
   }
}

export def restore-shebangs []: nothing -> nothing {
   shebangs (act).src --undo
}

export def --env test-env []: nothing -> nothing {
   $env.TZDIR = $"(build-dep tzdata)/share/zoneinfo"
   # doc/contributing/building_ruby.md's settings for slow machines. syntax_suggest's search
   # stops after 1 s by default, and mkmf's 30 s subprocess limits overrun under a world build
   $env.RUBY_TEST_TIMEOUT_SCALE = "5"
   $env.SYNTAX_SUGGEST_TIMEOUT  = "600"
   cp $"($env.IANA_ETC)/services" /etc/services

   let c = (act)
   cd $c.build
   (cue ./miniruby $"-I($c.src)/lib" -I. -I.ext/common $"($c.src)/tool/runruby.rb" --extout=.ext -- --disable-gems -C $c.src
      bin/gem install --local --ignore-dependencies --no-document --install-dir .bundle ...($env.RUBY_TEST_GEMS | split row ' '))
}
