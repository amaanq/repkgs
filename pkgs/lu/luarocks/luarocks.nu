use core.nu *

# rocks with C modules then build with the cc on PATH, not the one configure found
export def use-cc []: nothing -> nothing {
  for config in (files $"((ctx).out)/etc/luarocks/config-*.lua") {
    ['' 'variables.CC = "cc"' 'variables.LD = "cc"' ''] | str join "\n" | save --append $config
  }
}
