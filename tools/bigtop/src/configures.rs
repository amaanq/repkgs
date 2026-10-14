//! Autoconf caches for the configures a build tool runs itself, like hadrian.

use std::{
   collections::{
      BTreeMap,
      HashSet,
   },
   env::var,
   ffi::OsStr,
   fs::{
      create_dir_all,
      read,
      read_to_string,
      remove_file,
   },
   io,
   os::unix::ffi::OsStrExt as _,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use mummer::{
   config::package_flags,
   files::{
      glob,
      utf8,
      write_file,
   },
   process::loose,
   store::mask_hashes,
   tree::note,
};
use tightrope::key::Key;

use crate::{
   attrs::{
      Ctx,
      build_top,
   },
   probe::{
      resolved,
      restore_dir,
      store_dir,
   },
};

const KIND: &str = "configure-caches";

struct Caches {
   dir: PathBuf,
}

impl Caches {
   fn of(ctx: &Ctx) -> Self {
      Self {
         dir: ctx.build.join(KIND),
      }
   }

   fn sums_file(&self) -> PathBuf {
      self.dir.with_extension("sums")
   }

   fn caches(&self) -> io::Result<Vec<PathBuf>> {
      glob(&format!("{}/*.cache", utf8(&self.dir)?))
   }

   fn sums(&self) -> io::Result<BTreeMap<String, String>> {
      self
         .caches()?
         .iter()
         .map(|cache| {
            let name = utf8(Path::new(cache.file_name().unwrap_or_default()))?.to_owned();
            Ok((name, blake3::hash(&read(cache)?).to_hex().to_string()))
         })
         .collect()
   }
}

fn sidecars(cache: &Path) -> [PathBuf; 3] {
   [
      cache.to_path_buf(),
      cache.with_extension("dir"),
      cache.with_extension("env"),
   ]
}

fn drop_cache(cache: &Path) -> io::Result<()> {
   for path in sidecars(cache) {
      match remove_file(&path) {
         Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
         _ => {},
      }
   }
   Ok(())
}

/// Returns the site script that points each configure at its cache.
pub fn restore(key: &Key, socket: &Path) -> io::Result<PathBuf> {
   let ctx = Ctx::load(&build_top()?)?;
   let caches = Caches::of(&ctx);
   create_dir_all(&caches.dir)?;
   let restored = restore_dir(key, &caches.dir, socket)?;
   if restored {
      // autoconf aborts on a changed CC, and its store path moves with mummer
      let mut roots = ctx.roots;
      roots.extend(package_flags()?.into_keys());
      for cache in caches.caches()? {
         match resolved(&roots, &read_to_string(&cache)?) {
            Some(text) => write_file(&cache, text.as_bytes(), None)?,
            None => drop_cache(&cache)?,
         }
      }
   }
   note(KIND, if restored { "restored" } else { "cold" });
   write_file(
      &caches.sums_file(),
      &serde_json::to_vec(&caches.sums()?)?,
      None,
   )?;

   let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
   let original = var("CONFIG_SITE").unwrap_or_default();
   let mut site = original
      .split_whitespace()
      .map(|path| format!("if test -r {0}; then . {0}; fi", quote(path)))
      .collect::<Vec<String>>();
   site.extend([
      format!("CONFIG_SITE={}", quote(&original)),
      "if [ \"$cache_file\" = /dev/null ]; then".to_owned(),
      format!(
         "  pkgs_cache_base={}/$(pwd | tr / _)",
         quote(utf8(&caches.dir)?)
      ),
      r#"  pkgs_cache_env=$(for pkgs_var in $ac_precious_vars; do eval "printf %s=%s\\\\n \"\$pkgs_var\" \"\${$pkgs_var}\""; done | sed "s|/nix/store/[0-9a-z]\{32\}-|/nix/store/*-|g" | cksum | cut -d" " -f1)"#.to_owned(),
      r#"  for pkgs_stale in "$pkgs_cache_base"-*.cache "$pkgs_cache_base"-*.dir "$pkgs_cache_base"-*.env; do"#.to_owned(),
      r#"    case $pkgs_stale in "$pkgs_cache_base-$pkgs_cache_env".*) ;; *) rm -f "$pkgs_stale" ;; esac"#.to_owned(),
      "  done".to_owned(),
      "  cache_file=$pkgs_cache_base-$pkgs_cache_env.cache".to_owned(),
      r#"  pwd > "$pkgs_cache_base-$pkgs_cache_env.dir""#.to_owned(),
      r#"  env -0 > "$pkgs_cache_base-$pkgs_cache_env.env""#.to_owned(),
      "fi".to_owned(),
      String::new(),
   ]);
   let path = caches.dir.with_extension("site");
   write_file(&path, site.join("\n").as_bytes(), None)?;
   Ok(path)
}

fn replays(cache: &Path) -> io::Result<bool> {
   let [_, sidecar, recorded] = sidecars(cache);
   let Ok(written) = read_to_string(&sidecar) else {
      return Ok(false);
   };
   let directory = PathBuf::from(written.trim());
   let status = directory.join("config.status");
   let Ok(before) = read(&status) else {
      return Ok(false);
   };

   let mut recheck = Command::new("sh");
   recheck
      .args(["./config.status", "--recheck"])
      .current_dir(&directory);
   // cabal hands package flags to configure as CABAL_FLAG_* variables
   if let Ok(environment) = read(&recorded) {
      recheck
         .env_clear()
         .envs(environment.split(|&byte| byte == 0).filter_map(|pair| {
            let at = pair.iter().position(|&byte| byte == b'=')?;
            Some((
               OsStr::from_bytes(&pair[..at]),
               OsStr::from_bytes(&pair[at + 1..]),
            ))
         }));
   }
   let output = recheck.output()?;
   let after = read(&status)?;
   if after != before {
      write_file(&status, &before, None)?;
   }

   let name = cache.file_name().unwrap_or_default().display();
   if !output.status.success() {
      let error = loose(&output.stderr);
      let tail = error.lines().rev().take(3).collect::<Vec<&str>>();
      note(
         KIND,
         &format!(
            "{name} recheck exited {}: {}",
            output.status,
            tail.join(" | ")
         ),
      );
      return Ok(false);
   }
   note_lost_lines(KIND, &format!("{name} "), &before, &after);
   Ok(after == before)
}

pub fn store(key: &Key, socket: &Path) -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   let caches = Caches::of(&ctx);
   let restored = serde_json::from_slice::<BTreeMap<String, String>>(&read(caches.sums_file())?)?;
   for (name, sum) in caches.sums()? {
      let cache = caches.dir.join(&name);
      if restored.get(&name) != Some(&sum) && !replays(&cache)? {
         note(
            KIND,
            &format!("{name} answers change what configure writes, not cached"),
         );
         drop_cache(&cache)?;
      }
   }

   for recorded in glob(&format!("{}/*.env", utf8(&caches.dir)?))? {
      remove_file(recorded)?;
   }
   for cache in caches.caches()? {
      let Ok(text) = read_to_string(&cache) else {
         note(
            KIND,
            &format!("{} is not UTF-8, not cached", cache.display()),
         );
         drop_cache(&cache)?;
         continue;
      };
      write_file(&cache, mask_hashes(&text).as_bytes(), None)?;
   }
   store_dir(key, &caches.dir, socket)
}

/// A lost line points at a check whose cached answer skips a side effect.
pub fn note_lost_lines(kind: &str, prefix: &str, first: &[u8], second: &[u8]) {
   let fresh = loose(first);
   let replayed = loose(second);
   let kept = replayed.lines().collect::<HashSet<_>>();
   for line in fresh.lines().filter(|line| !kept.contains(line)).take(3) {
      note(
         kind,
         &format!(
            "{prefix}cached run lost {}",
            line.chars().take(160).collect::<String>()
         ),
      );
   }
}
