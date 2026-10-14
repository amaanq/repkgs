use core.nu [act dep shebangs glob-files]

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
