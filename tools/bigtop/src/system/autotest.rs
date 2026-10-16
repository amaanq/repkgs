use std::{
   collections::BTreeSet,
   ffi::OsString,
   fs::{
      Permissions,
      create_dir_all,
      read,
      read_dir,
      read_to_string,
      remove_dir_all,
      set_permissions,
   },
   io::{
      self,
      BufRead as _,
      BufReader,
      Read as _,
      Write as _,
      pipe,
   },
   os::unix::fs::{
      PermissionsExt as _,
      symlink,
   },
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      ExitStatus,
   },
   str::FromStr,
   sync::{
      Mutex,
      OnceLock,
   },
   thread::scope,
};

use mummer::{
   files::{
      glob,
      utf8,
      write_file,
   },
   process::{
      Checked as _,
      strict,
   },
   store::normalize,
   tree::note,
};

use crate::{
   attrs::{
      Ctx,
      build_top,
   },
   system::{
      command::{
         run,
         strs,
      },
      make::{
         self,
         AutomakeTest,
      },
   },
   test_cache,
};

const PREFIX: &str = "autotest\t";

/// A test the cache can pick in an automake tree with autotest suites: one
/// the parallel harness ran, or one autotest group bigtop ran by itself.
pub enum Unit {
   Automake(AutomakeTest),
   Group(String),
}

impl FromStr for Unit {
   type Err = String;

   fn from_str(text: &str) -> Result<Self, Self::Err> {
      text.strip_prefix(PREFIX).map_or_else(
         || text.parse().map(Self::Automake),
         |name| Ok(Self::Group(name.to_owned())),
      )
   }
}

/// One autotest suite as `make check` ran it, in `cwd` with `args`.
struct Suite {
   cwd:     PathBuf,
   script:  PathBuf,
   args:    Vec<String>,
   single:  Vec<String>,
   tag:     String,
   startup: Mutex<()>,
}

struct Group<'suite> {
   suite:  &'suite Suite,
   number: u32,
}

impl Group<'_> {
   /// autotest names its log and its directory after the script's basename,
   /// so each group runs as a link of its own and none share either.
   fn name(&self) -> String {
      format!("{}-{}", self.suite.tag, self.number)
   }
}

fn home() -> Option<&'static Path> {
   static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
   HOME
      .get_or_init(|| {
         build_top()
            .ok()
            .map(|top| Path::new(&top).join(".bigtop-autotest"))
      })
      .as_deref()
}

fn home_dir() -> io::Result<&'static Path> {
   home().ok_or_else(|| io::Error::other("NIX_BUILD_TOP is not set"))
}

/// Whether configure set up an autotest suite, whose `atconfig` it writes.
pub fn present(build: &Path) -> io::Result<bool> {
   Ok(glob(&format!("{}/**/atconfig", utf8(build)?))?
      .iter()
      .any(|path| path.is_file()))
}

/// The group a process running `args` is, the link bigtop started it as.
pub fn unit(args: &[OsString]) -> Option<String> {
   let links = home()?.join("links");
   args.iter().find_map(|arg| {
      let path = Path::new(arg);
      if path.parent()? != links {
         return None;
      }
      Some(format!("{PREFIX}{}", path.file_name()?.to_str()?))
   })
}

/// bigtop's shim, records and links, rewritten on every run. What a group
/// reads through its link the traced `--list` of the same script reads too.
pub fn state(path: &Path) -> bool {
   home().is_some_and(|dir| path.starts_with(dir))
}

pub fn test(ctx: &Ctx, flags: &[String], targets: &[String]) -> io::Result<()> {
   let home = home_dir()?;
   let shell = shell()?;
   let shim = prepare(home, &shell)?;
   let mut shimmed = flags.to_vec();
   shimmed.push(format!("TESTSUITE={}", utf8(&shim)?));
   let jobs = format!("-j{}", ctx.test_jobs()?);

   let Some(selected) = test_cache::selected::<Unit>()? else {
      check(&jobs, targets, &shimmed, false)?;
      let suites = recorded(home, flags)?;
      let groups = listed(&shell, &suites)?;
      return groups_run(ctx, home, &shell, &groups);
   };
   let mut automake = Vec::new();
   let mut chosen = BTreeSet::new();
   for unit in selected {
      match unit {
         Unit::Automake(test) => automake.push(test),
         Unit::Group(name) => {
            chosen.insert(name);
         },
      }
   }
   if !automake.is_empty() {
      make::automake(ctx, &shimmed, targets, automake)?;
   }
   if chosen.is_empty() {
      return Ok(());
   }
   remove_dir_all(home.join("records"))?;
   create_dir_all(home.join("records"))?;
   check(&jobs, targets, &shimmed, true)?;
   let suites = recorded(home, flags)?;
   let groups = listed(&shell, &suites)?;
   let known = groups.iter().map(Group::name).collect::<BTreeSet<_>>();
   if !chosen.is_subset(&known) {
      note(
         "autotest",
         "the selected groups name suites this run has not, running every group",
      );
      return groups_run(ctx, home, &shell, &groups);
   }
   let picked = groups
      .into_iter()
      .filter(|group| chosen.contains(&group.name()))
      .collect::<Vec<_>>();
   groups_run(ctx, home, &shell, &picked)
}

fn shell() -> io::Result<String> {
   let [(_config, path), _make] = make::shell()?;
   Ok(path)
}

/// A fresh shim `make check` runs as `$(TESTSUITE)`, recording where and
/// with what instead of running the suite.
fn prepare(home: &Path, shell: &str) -> io::Result<PathBuf> {
   if home.exists() {
      remove_dir_all(home)?;
   }
   let records = home.join("records");
   create_dir_all(&records)?;
   create_dir_all(home.join("links"))?;
   let shim = home.join("testsuite");
   let text = format!(
      "#!{shell}\nprintf '%s\\0' \"$(pwd)\" \"$@\" > \"$(mktemp '{}/r.XXXXXX')\"\n",
      utf8(&records)?
   );
   write_file(&shim, text.as_bytes(), None)?;
   set_permissions(&shim, Permissions::from_mode(0o755))?;
   Ok(shim)
}

/// `make check` up to the suites, every prerequisite and every other test,
/// or with `TESTS=` none of the latter.
fn check(jobs: &str, targets: &[String], flags: &[String], quiet: bool) -> io::Result<()> {
   let mut args = vec![jobs, "VERBOSE=1"];
   args.extend(strs(targets));
   args.extend(strs(flags));
   if quiet {
      args.push("TESTS=");
   }
   run("make", &args, &[])
}

/// The suites the shim saw, the script each Makefile's `TESTSUITE` names,
/// remade from its `.at` files as `$(TESTSUITE)` would have been.
fn recorded(home: &Path, flags: &[String]) -> io::Result<Vec<Suite>> {
   let mut records = read_dir(home.join("records"))?
      .map(|entry| entry.map(|found| found.path()))
      .collect::<io::Result<Vec<_>>>()?;
   records.sort();
   let mut suites = Vec::<Suite>::new();
   for record in records {
      let bytes = read(&record)?;
      let mut fields = bytes
         .split(|byte| *byte == 0)
         .map(|field| strict(field, "an autotest record").map(str::to_owned))
         .collect::<io::Result<Vec<_>>>()?;
      fields.pop();
      if fields.is_empty() {
         return Err(io::Error::other(format!("{} is empty", record.display())));
      }
      let cwd = PathBuf::from(fields.remove(0));
      let target = named(&cwd, flags)?;
      let script = normalize(&cwd.join(&target));
      let single = single(&fields);
      let mut hasher = blake3::Hasher::new();
      for part in [utf8(&cwd)?, utf8(&script)?]
         .into_iter()
         .chain(single.iter().map(String::as_str))
      {
         hasher.update(part.as_bytes());
         hasher.update(b"\0");
      }
      let base = script
         .file_name()
         .and_then(|name| name.to_str())
         .ok_or_else(|| io::Error::other(format!("{} has no file name", script.display())))?;
      let tag = format!("{base}-{}", &hasher.finalize().to_hex()[..16]);
      if suites.iter().any(|suite| suite.tag == tag) {
         continue;
      }
      let directory = utf8(&cwd)?;
      let mut remake = vec!["-C", directory, target.as_str()];
      remake.extend(strs(flags));
      run("make", &remake, &[])?;
      suites.push(Suite {
         cwd,
         script,
         args: fields,
         single,
         tag,
         startup: Mutex::new(()),
      });
   }
   if suites.is_empty() {
      note(
         "autotest",
         "no suite ran as $(TESTSUITE), make ran them itself",
      );
   }
   Ok(suites)
}

/// The script `TESTSUITE` names in the Makefile of `cwd`.
fn named(cwd: &Path, flags: &[String]) -> io::Result<String> {
   const GOAL: &str = "bigtop-autotest-suite";
   let rule = format!("{GOAL}: ; @printf '%s' '$(TESTSUITE)'");
   let output = Command::new("make")
      .args(["-s", "--no-print-directory", "--eval", &rule, GOAL])
      .args(flags)
      .current_dir(cwd)
      .checked_output()?;
   let script = strict(&output, "make TESTSUITE")?;
   if script.is_empty() {
      return Err(io::Error::other(format!(
         "{} runs a suite but its Makefile names no TESTSUITE",
         cwd.display()
      )));
   }
   Ok(script.to_owned())
}

/// The arguments that run one group, `args` without the groups, keywords,
/// reruns and job counts that pick or schedule several.
fn single(args: &[String]) -> Vec<String> {
   let mut kept = Vec::new();
   let mut tokens = args.iter();
   while let Some(token) = tokens.next() {
      let text = token.as_str();
      match text {
         "-C" | "--directory" => {
            kept.push(token.clone());
            kept.extend(tokens.next().cloned());
         },
         "-k" | "--keywords" => {
            tokens.next();
         },
         "-j" | "--jobs" | "--recheck" => {},
         _ if text.starts_with("--keywords=")
            || text.starts_with("--jobs=")
            || text.strip_prefix("-j").is_some_and(digits)
            || selects(text) => {},
         _ => kept.push(token.clone()),
      }
   }
   kept
}

fn digits(text: &str) -> bool {
   !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// `N`, `N-`, `-N` and `N-M`, autotest's group ranges.
fn selects(text: &str) -> bool {
   match text.split_once('-') {
      None => digits(text),
      Some((first, last)) => {
         (digits(first) || first.is_empty()) && (digits(last) || last.is_empty()) && text != "-"
      },
   }
}

/// Each suite's groups as `--list` gives them for the arguments `make check`
/// passed, which keeps its ranges and keywords.
fn listed<'suites>(shell: &str, suites: &'suites [Suite]) -> io::Result<Vec<Group<'suites>>> {
   let mut groups = Vec::new();
   for suite in suites {
      let output = Command::new(shell)
         .arg(&suite.script)
         .args(&suite.args)
         .arg("--list")
         .current_dir(&suite.cwd)
         .checked_output()?;
      for line in strict(&output, "testsuite --list")?.lines() {
         let Some((field, _rest)) = line.trim_start().split_once(": ") else {
            continue;
         };
         if let Ok(number) = field.parse() {
            groups.push(Group { suite, number });
         }
      }
   }
   Ok(groups)
}

fn groups_run(ctx: &Ctx, home: &Path, shell: &str, groups: &[Group<'_>]) -> io::Result<()> {
   let links = home.join("links");
   let workers = ctx.test_jobs()?;
   for group in groups {
      let link = links.join(group.name());
      if !link.exists() {
         symlink(&group.suite.script, &link)?;
      }
   }
   let pending = Mutex::new(groups.iter());
   let failed = Mutex::new(Vec::new());
   scope(|threads| {
      for _worker in 0..workers {
         threads.spawn(|| {
            loop {
               let Some(group) = pending.lock().ok().and_then(|mut next| next.next()) else {
                  return;
               };
               let ran = group_run(shell, &links.join(group.name()), group);
               let passed = ran.as_ref().is_ok_and(|&(status, _)| status.success());
               report(group, &ran, passed);
               if !passed && let Ok(mut list) = failed.lock() {
                  list.push(group.name());
               }
            }
         });
      }
   });
   let failures = failed
      .into_inner()
      .map_err(|_poisoned| io::Error::other("an autotest runner panicked"))?;
   if failures.is_empty() {
      return Ok(());
   }
   Err(io::Error::other(format!(
      "autotest groups failed: {}",
      failures.join(", ")
   )))
}

/// autotest sources `atlocal` in the suite's directory in every process, and
/// bison's writes and removes `conftest.*` there, so a group starts only once
/// the one before it printed the banner that follows the sourcing.
fn group_run(shell: &str, link: &Path, group: &Group<'_>) -> io::Result<(ExitStatus, Vec<u8>)> {
   let (pipe_end, writer) = pipe()?;
   let mut reader = BufReader::new(pipe_end);
   let mut output = Vec::new();
   let mut child = {
      let _turn = group
         .suite
         .startup
         .lock()
         .map_err(|_poisoned| io::Error::other("an autotest runner panicked"))?;
      let child = Command::new(shell)
         .arg(link)
         .args(&group.suite.single)
         .arg(group.number.to_string())
         .current_dir(&group.suite.cwd)
         .stdout(writer.try_clone()?)
         .stderr(writer)
         .spawn()?;
      loop {
         let start = output.len();
         if reader.read_until(b'\n', &mut output)? == 0 || output[start..].starts_with(b"## -") {
            break;
         }
      }
      child
   };
   reader.read_to_end(&mut output)?;
   Ok((child.wait()?, output))
}

/// A group's output in one piece, and on failure the log autotest keeps in
/// the group's directory, named after the script like the suite's.
fn report(group: &Group<'_>, ran: &io::Result<(ExitStatus, Vec<u8>)>, passed: bool) {
   let mut out = io::stdout().lock();
   match *ran {
      Ok((_status, ref output)) => drop(out.write_all(output)),
      Err(ref error) => drop(writeln!(out, "{}: {error}", group.name())),
   }
   if passed {
      return;
   }
   let logs = utf8(&suite_dir(group.suite))
      .ok()
      .and_then(|dir| {
         glob(&format!(
            "{dir}/{name}.dir/*/{name}.log",
            name = group.name()
         ))
         .ok()
      })
      .unwrap_or_default();
   for log in logs {
      if let Ok(text) = read_to_string(&log) {
         drop(writeln!(out, "--- {}\n{text}", log.display()));
      }
   }
}

/// Where the suite runs its groups, `cwd` or the last `-C` it was given.
fn suite_dir(suite: &Suite) -> PathBuf {
   let mut dir = suite.cwd.clone();
   let mut tokens = suite.args.iter();
   while let Some(token) = tokens.next() {
      if let Some(given) = token.strip_prefix("--directory=") {
         dir = suite.cwd.join(given);
      } else if (token == "-C" || token == "--directory")
         && let Some(given) = tokens.next()
      {
         dir = suite.cwd.join(given);
      }
   }
   dir
}
