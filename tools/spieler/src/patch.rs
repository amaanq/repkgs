//! Patches act.toml names by url. spieler downloads each one beside the act,
//! records the file in act.lock and checks the whole list still applies, so a
//! build only ever reads files in the tree.
//!
//! A source is a pull request or commit url on GitHub, GitLab, Forgejo or
//! Gitea, `github:` or `gitlab:` followed by such a path, or any https url
//! serving a diff. Forge urls are fetched as `.patch`, so commits keep their
//! headers.

use std::{
   env::temp_dir,
   fs::{
      create_dir_all,
      remove_dir_all,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
      id,
   },
};

/// One url entry of act.toml's `patches`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
   /// Where the patch text comes from.
   pub download: String,
   /// What it is vendored as, beside the act.
   pub file:     String,
}

impl Remote {
   /// The remote patch `raw` names, None for a file beside the act.
   pub fn parse(raw: &str) -> io::Result<Option<Self>> {
      let url = if let Some(path) = raw.strip_prefix("github:") {
         format!("https://github.com/{path}")
      } else if let Some(path) = raw.strip_prefix("gitlab:") {
         format!("https://gitlab.com/{path}")
      } else if raw.contains("://") {
         raw.to_owned()
      } else {
         return Ok(None);
      };
      let Some(rest) = url.strip_prefix("https://") else {
         return Err(io::Error::other(format!(
            "patch {raw} must come over https"
         )));
      };
      let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
      let lowered = authority.to_ascii_lowercase();
      let host = lowered.strip_prefix("www.").unwrap_or(&lowered);
      let (routed, query) = path.split_once(['?', '#']).unwrap_or((path, ""));
      let mut segments = routed
         .split('/')
         .filter(|segment| !segment.is_empty())
         .collect::<Vec<_>>();
      if let Some(last) = segments.last_mut() {
         *last = last
            .strip_suffix(".diff")
            .or_else(|| last.strip_suffix(".patch"))
            .unwrap_or(last);
      }
      let number = |text: &str| {
         text.parse::<u64>().map_err(|error| {
            io::Error::other(format!(
               "patch {raw}: {text} is no pull request number, {error}"
            ))
         })
      };

      // gitlab puts its routes after a `-`, since projects nest in groups
      if let Some(dash) = segments.iter().position(|&segment| segment == "-") {
         let (project, route) = segments.split_at(dash);
         let base = format!("https://{host}/{}/-", project.join("/"));
         let picked = query
            .split('&')
            .find_map(|param| param.strip_prefix("commit_id="));
         return Ok(Some(match (route, picked) {
            (&["-", "merge_requests", _, "diffs", ..], Some(rev))
            | (&["-", "commit", rev, ..], _) => Self::commit(&base, rev),
            (&["-", "merge_requests", text, ..], _) => {
               let pull = number(text)?;
               Self {
                  download: format!("{base}/merge_requests/{pull}.patch"),
                  file:     format!("mr-{pull}.patch"),
               }
            },
            _ => Self::plain(&url, route.last().copied()),
         }));
      }

      let pulls = if host == "github.com" {
         "pull"
      } else {
         "pulls"
      };
      Ok(Some(match *segments.as_slice() {
         [owner, repo, kind, _, "commits", rev, ..] if kind == pulls => {
            Self::commit(&format!("https://{host}/{owner}/{repo}"), rev)
         },
         [owner, repo, "commit", rev, ..] => {
            Self::commit(&format!("https://{host}/{owner}/{repo}"), rev)
         },
         [owner, repo, kind, text, ..] if kind == pulls => {
            let pull = number(text)?;
            Self {
               download: format!("https://{host}/{owner}/{repo}/{pulls}/{pull}.patch"),
               file:     format!("pr-{pull}.patch"),
            }
         },
         _ => Self::plain(&url, segments.last().copied()),
      }))
   }

   fn commit(base: &str, rev: &str) -> Self {
      Self {
         download: format!("{base}/commit/{rev}.patch"),
         file:     format!("{}.patch", rev.get(..12).unwrap_or(rev)),
      }
   }

   fn plain(url: &str, last: Option<&str>) -> Self {
      let served = Path::new(url.split(['?', '#']).next().unwrap_or(url))
         .extension()
         .is_some_and(|ext| ext.eq_ignore_ascii_case("patch") || ext.eq_ignore_ascii_case("diff"));
      // cgit's `patch/?id=` names nothing in its path, so the url tells them
      // apart
      let file = match last {
         Some(name) if !url.contains('?') => {
            if served {
               url.rsplit('/').next().unwrap_or(name).to_owned()
            } else {
               format!("{name}.patch")
            }
         },
         _ => {
            let digest = blake3::hash(url.as_bytes()).to_hex();
            format!("{}.patch", &digest[..12])
         },
      };
      Self {
         download: url.to_owned(),
         file,
      }
   }
}

/// Why a patch list does not apply to a source.
#[derive(Debug)]
pub enum Refusal {
   /// The source already carries the change, so the entry can go.
   Contained(PathBuf),
   Conflict(PathBuf),
}

/// Applies `patches` in order to a copy of `src` as the build does, with no
/// fuzz.
pub fn check(src: &Path, patches: &[PathBuf]) -> io::Result<Result<(), Refusal>> {
   let work = copied(src, "spieler-patch")?;
   let outcome = apply(&work, patches);
   remove_dir_all(&work)?;
   outcome
}

/// A copy of `src` with `patches` applied, which the caller removes.
pub fn patched(src: &Path, patches: &[PathBuf]) -> io::Result<PathBuf> {
   let work = copied(src, "spieler-patched")?;
   match apply(&work, patches) {
      Ok(Ok(())) => Ok(work),
      Ok(Err(Refusal::Contained(file) | Refusal::Conflict(file))) => {
         remove_dir_all(&work)?;
         Err(io::Error::other(format!(
            "{} does not apply",
            file.display()
         )))
      },
      Err(error) => {
         remove_dir_all(&work)?;
         Err(error)
      },
   }
}

fn copied(src: &Path, prefix: &str) -> io::Result<PathBuf> {
   let work = temp_dir().join(format!("{prefix}-{}", id()));
   if work.exists() {
      remove_dir_all(&work)?;
   }
   create_dir_all(&work)?;
   let status = Command::new("cp")
      .args(["-r", "--no-preserve=mode,ownership", "-T"])
      .arg(src)
      .arg(&work)
      .status()?;
   if !status.success() {
      remove_dir_all(&work)?;
      return Err(io::Error::other(format!(
         "could not copy {}",
         src.display()
      )));
   }
   Ok(work)
}

fn apply(work: &Path, patches: &[PathBuf]) -> io::Result<Result<(), Refusal>> {
   let run = |file: &Path, extra: &[&str]| -> io::Result<bool> {
      Ok(Command::new("patch")
         .args(["-p1", "-F0", "--batch", "--quiet"])
         .args(extra)
         .arg("-d")
         .arg(work)
         .arg("-i")
         .arg(file)
         .stdout(Stdio::null())
         .stderr(Stdio::null())
         .status()?
         .success())
   };
   for file in patches {
      if run(file, &["--dry-run"])? && run(file, &[])? {
         continue;
      }
      return Ok(Err(if run(file, &["-R", "--dry-run"])? {
         Refusal::Contained(file.clone())
      } else {
         Refusal::Conflict(file.clone())
      }));
   }
   Ok(Ok(()))
}
