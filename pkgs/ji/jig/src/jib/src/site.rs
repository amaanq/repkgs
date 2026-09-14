use std::{
   env::var,
   fs::read_to_string,
   io,
   path::Path,
};

use jig::files::write_file;
use serde_json::json;

use crate::attrs::Ctx;

const HELPER: &str = r#"
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
"#;

pub fn site(ctx: &Ctx, script: &Path) -> io::Result<String> {
   let original = var("CONFIG_SITE").unwrap_or_default();
   if !ctx.cache || original.is_empty() {
      return Ok(original);
   }
   let text = read_to_string(script)?;
   let (Some(header), Some(compile)) = (
      function(&text, "ac_fn_c_check_header_compile"),
      function(&text, "ac_fn_c_try_compile"),
   ) else {
      return Ok(original);
   };
   let sites: Vec<&str> = original.split_whitespace().collect();
   let contents: Vec<Option<String>> = sites.iter().map(|site| read_to_string(site).ok()).collect();
   let context = blake3::hash(&serde_json::to_vec(&json!([header, compile, contents]))?)
      .to_hex()
      .to_string();
   let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
   let prefix: Vec<String> = sites
      .iter()
      .map(|site| format!("if test -r {0}; then . {0}; fi", quote(site)))
      .collect();
   let site_text = [
      prefix.join("\n"),
      format!("CONFIG_SITE={}", quote(&original)),
      HELPER.replace("@CONTEXT@", &context),
      header.replace("ac_fn_c_try_compile", "jig_header_compile"),
      String::new(),
   ]
   .join("\n");
   let destination = format!("{}/jig-config.site", ctx.build);
   write_file(Path::new(&destination), site_text.as_bytes(), None)?;
   Ok(destination)
}

fn function(text: &str, name: &str) -> Option<String> {
   let opening = format!("{name} ()\n{{\n");
   let closing = format!("\n}} # {name}");
   let mut starts = text
      .match_indices(&opening)
      .map(|(at, _)| at)
      .filter(|&at| at == 0 || text.as_bytes()[at - 1] == b'\n');
   let start = starts.next()?;
   if starts.next().is_some() {
      return None;
   }
   let end = text[start..].find(&closing)? + start + closing.len();
   Some(text[start..end].to_owned())
}
