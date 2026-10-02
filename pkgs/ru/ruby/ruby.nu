use core.nu *

# rbconfig.rb records the build's shell and compiler banner, the latter with clang's install dir
export def relocate-rbconfig []: nothing -> nothing {
  let bash = $"(dep-root bash "rbconfig SHELL")/bin/bash"
  for config in (glob $"((ctx).out)/lib/ruby/*/*/rbconfig.rb") {
    let text = (open --raw $config
      | str replace -r '(CONFIG\["SHELL"\] = ")[^"]*' $"${1}($bash)"
      | str replace -r '(CONFIG\["CC_VERSION_MESSAGE"\] = "[^"]*?)\\nInstalledDir: [^"]*' '${1}')
    $text | save -f $config
  }
}

export def restore-shebangs []: nothing -> nothing {
  fix-shebangs (ctx).src --undo
}
