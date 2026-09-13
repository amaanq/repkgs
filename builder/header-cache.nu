use core.nu *

const HELPER = r#'
jig_header_compile ()
{
  if test "$ac_ext" != c || test "$ac_compile" != '$CC -c $CFLAGS $CPPFLAGS conftest.$ac_ext >&5'; then
    ac_fn_c_try_compile "$@"
    return $?
  fi
  jig_header_key=$(jig header-key @CONTEXT@ "$ac_c_werror_flag" $CC -c $CFLAGS $CPPFLAGS conftest.$ac_ext 2>&5)
  jig_header_status=$?
  if test "$jig_header_status" = 0; then
    printf '%s' '(shared) ' >&6
    printf '%s\n' 'jig reused a successful header check' >&5
    return 0
  fi
  if ac_fn_c_try_compile "$@"; then
    if test "$jig_header_status" = 3; then
      printf '%s\n' yes >conftest.jig-result
      jig cache put "$jig_header_key" conftest.jig-result >&5 2>&5
      rm -f conftest.jig-result
    fi
    return 0
  fi
  return 1
}
'#

export def site [script: path]: nothing -> string {
  let original = ($env.CONFIG_SITE? | default "")
  if not (ctx).cache or $original == "" { return $original }
  let text = (open --raw $script)
  let header = ($text | parse --regex '(?ms)^(?<body>ac_fn_c_check_header_compile \(\)\n\{.*?^\} # ac_fn_c_check_header_compile)' | get body)
  let compile = ($text | parse --regex '(?ms)^(?<body>ac_fn_c_try_compile \(\)\n\{.*?^\} # ac_fn_c_try_compile)' | get body)
  if ($header | length) != 1 or ($compile | length) != 1 { return $original }
  let sites = ($original | split row --regex '\s+' | compact --empty)
  let context = ([$header $compile ($sites | each {|file| if ($file | path exists) { open --raw $file } else { null } })] | to json -r | hash sha256)
  let quote = {|value: string| "'" + ($value | str replace -a "'" "'\''") + "'" }
  let prefix = ($sites | each {|file|
    let quoted = (do $quote $file)
    $"if test -r ($quoted); then . ($quoted); fi"
  } | str join "\n")
  let patched = ($header | first | str replace ac_fn_c_try_compile jig_header_compile)
  let contents = [$prefix $"CONFIG_SITE=(do $quote $original)" ($HELPER | str replace @CONTEXT@ $context) $patched ""]
  let destination = $"((ctx).build)/jig-config.site"
  $contents | str join "\n" | save --force $destination
  $destination
}
