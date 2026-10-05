use std::{
   env::{
      args_os,
      var,
      var_os,
   },
   ffi::{
      OsStr,
      OsString,
   },
   fmt::{
      Arguments,
      Display,
      Formatter,
      Result as FormatResult,
   },
   fs::{
      OpenOptions,
      read_to_string,
   },
   io::{
      self,
      Write as _,
   },
   mem::take,
   os::unix::process::ExitStatusExt as _,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      ExitCode,
      ExitStatus,
      Stdio,
   },
   str::FromStr,
   time::Instant,
};

use misstep::{
   Result,
   report,
};
use pound::Error as ArgumentError;
use tightrope::client::Client;

use crate::store::STORE_DIR;

pub struct Slot<'client> {
   client: &'client mut Client,
   build:  String,
}

impl Slot<'_> {
   #[inline]
   pub fn acquire(client: &mut Client) -> io::Result<Option<Slot<'_>>> {
      if var("MUMMER_SLOT").is_ok_and(|value| !value.is_empty()) {
         return Ok(None);
      }
      let build = var("NIX_BUILD_TOP").unwrap_or_else(|_| "-".to_owned());
      client.acquire(&build)?;
      Ok(Some(Slot { client, build }))
   }
}

impl Drop for Slot<'_> {
   #[inline]
   fn drop(&mut self) {
      if let Err(error) = self.client.release(&self.build) {
         diagnostic(format_args!(
            "mummer could not release compiler slot, {error}"
         ));
      }
   }
}

pub struct RunResult {
   pub status: i32,
   pub stderr: Vec<u8>,
}

#[inline]
pub fn diagnostic(message: Arguments<'_>) {
   drop(writeln!(io::stderr().lock(), "{message}"));
}

/// The wrapper an invocation log line is from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
   Cc,
   Rustc,
   Ghc,
   Go,
   Header,
}

impl Tool {
   const ALL: [Self; 5] = [Self::Cc, Self::Rustc, Self::Ghc, Self::Go, Self::Header];

   #[must_use]
   #[inline]
   pub const fn name(self) -> &'static str {
      match self {
         Self::Cc => "cc",
         Self::Rustc => "rustc",
         Self::Ghc => "ghc",
         Self::Go => "go",
         Self::Header => "header",
      }
   }
}

impl Display for Tool {
   #[inline]
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      f.write_str(self.name())
   }
}

impl FromStr for Tool {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      Self::ALL
         .into_iter()
         .find(|tool| tool.name() == text)
         .ok_or_else(|| io::Error::other(format!("unknown tool {text}")))
   }
}

/// What became of one invocation, as the build's cache summary counts it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
   Query,
   Linked,
   Uncacheable,
   Unkeyed,
   NoDaemon,
   Cached,
   CachedError,
   Compiled,
   CompiledError,
   CompiledUnstored,
   Failed,
   UnsupportedCompiler,
   UnsupportedOptions,
   CompileHit,
   PreprocessFailed,
   Diagnostics,
   Hit,
   Miss,
}

impl Outcome {
   const ALL: [Self; 18] = [
      Self::Query,
      Self::Linked,
      Self::Uncacheable,
      Self::Unkeyed,
      Self::NoDaemon,
      Self::Cached,
      Self::CachedError,
      Self::Compiled,
      Self::CompiledError,
      Self::CompiledUnstored,
      Self::Failed,
      Self::UnsupportedCompiler,
      Self::UnsupportedOptions,
      Self::CompileHit,
      Self::PreprocessFailed,
      Self::Diagnostics,
      Self::Hit,
      Self::Miss,
   ];

   #[must_use]
   #[inline]
   pub const fn name(self) -> &'static str {
      match self {
         Self::Query => "query",
         Self::Linked => "linked",
         Self::Uncacheable => "uncacheable",
         Self::Unkeyed => "unkeyed",
         Self::NoDaemon => "no-daemon",
         Self::Cached => "cached",
         Self::CachedError => "cached-error",
         Self::Compiled => "compiled",
         Self::CompiledError => "compiled-error",
         Self::CompiledUnstored => "compiled-unstored",
         Self::Failed => "failed",
         Self::UnsupportedCompiler => "unsupported-compiler",
         Self::UnsupportedOptions => "unsupported-options",
         Self::CompileHit => "compile-hit",
         Self::PreprocessFailed => "preprocess-failed",
         Self::Diagnostics => "diagnostics",
         Self::Hit => "hit",
         Self::Miss => "miss",
      }
   }

   /// Answered from the cache, the compiler never ran.
   #[must_use]
   #[inline]
   pub const fn is_cached(self) -> bool {
      matches!(
         self,
         Self::Cached | Self::CachedError | Self::Hit | Self::CompileHit
      )
   }

   /// The compiler ran because the cache had no answer.
   #[must_use]
   #[inline]
   pub const fn is_compiled(self) -> bool {
      matches!(
         self,
         Self::Compiled | Self::CompiledError | Self::CompiledUnstored
      )
   }
}

impl Display for Outcome {
   #[inline]
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      f.write_str(self.name())
   }
}

impl FromStr for Outcome {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      Self::ALL
         .into_iter()
         .find(|outcome| outcome.name() == text)
         .ok_or_else(|| io::Error::other(format!("unknown outcome {text}")))
   }
}

/// One line of the invocation log `log` writes.
pub struct Logged<'log> {
   pub tool:    Tool,
   pub outcome: Outcome,
   pub subject: &'log str,
}

/// The invocations a log recorded, less queries and lines it cannot read.
#[inline]
pub fn logged(text: &str) -> impl Iterator<Item = Logged<'_>> {
   text.lines().filter_map(|line| {
      let mut fields = line.split('\t');
      let tool = fields.next()?.parse().ok()?;
      let outcome = fields.next()?.parse().ok()?;
      (outcome != Outcome::Query).then(|| {
         Logged {
            tool,
            outcome,
            subject: fields.next().unwrap_or_default(),
         }
      })
   })
}

#[inline]
pub fn log<Subject>(tool: Tool, outcome: Outcome, subject: Subject, started: Instant)
where
   Subject: Display,
{
   if let Ok(path) = var("MUMMER_LOG")
      && !path.is_empty()
   {
      let subject_text = subject.to_string().replace(['\t', '\r', '\n'], " ");
      let record = format!(
         "{tool}\t{outcome}\t{subject_text}\t{}\n",
         started.elapsed().as_millis()
      );
      let result = OpenOptions::new()
         .create(true)
         .append(true)
         .open(path)
         .and_then(|mut file| file.write_all(record.as_bytes()));
      if let Err(error) = result {
         diagnostic(format_args!(
            "mummer could not write its invocation log, {error}"
         ));
      }
   }
}

#[inline]
pub fn log_uncached(outcome: Outcome, args: &[String]) {
   if outcome == Outcome::Query {
      return;
   }
   if let Ok(path) = var("MUMMER_LOG_ARGS")
      && !path.is_empty()
   {
      let record = format!("{outcome}\t{}\n", args.join(" "));
      let result = OpenOptions::new()
         .create(true)
         .append(true)
         .open(path)
         .and_then(|mut file| file.write_all(record.as_bytes()));
      if let Err(error) = result {
         diagnostic(format_args!(
            "mummer could not write its uncached log, {error}"
         ));
      }
   }
}

impl RunResult {
   #[must_use]
   #[inline]
   /// The compiler exited by itself and placed an error at a source line,
   /// `file:line[:column]: error:`. Anything else, a full disk or a crash, is
   /// the machine and not the source.
   pub fn reported_error(&self) -> bool {
      self.status < 128
         && String::from_utf8_lossy(&self.stderr).lines().any(|line| {
            let Some((place, _)) = line
               .split_once(": error:")
               .or_else(|| line.split_once(": fatal error:"))
            else {
               return false;
            };
            let number =
               |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
            match *place.rsplitn(3, ':').collect::<Vec<_>>() {
               [column, row, file] if number(column) && number(row) => !file.is_empty(),
               [row, file, ..] if number(row) => !file.is_empty(),
               _ => false,
            }
         })
   }
}

#[must_use]
#[inline]
pub fn status_code(status: ExitStatus) -> i32 {
   status
      .code()
      .unwrap_or_else(|| 128_i32 + status.signal().unwrap_or(1_i32))
}

/// Runs a program under a compiler slot when a daemon answers, its output
/// passed through.
#[inline]
pub fn run_slotted<Argument>(
   program: &OsStr,
   args: &[Argument],
   socket: Option<&Path>,
) -> io::Result<RunResult>
where
   Argument: AsRef<OsStr>,
{
   let mut client = socket.and_then(|path| Client::connect(path).ok());
   let slot = client
      .as_mut()
      .and_then(|connected| Slot::acquire(connected).ok().flatten());
   let mut command = Command::new(program);
   command
      .args(args)
      .stdin(Stdio::inherit())
      .stdout(Stdio::inherit());
   if slot.is_some() {
      command.env("MUMMER_SLOT", "1");
   }
   let result = command.status().map(|status| {
      RunResult {
         status: status_code(status),
         stderr: Vec::new(),
      }
   });
   drop(slot);
   started(program, result, |message| {
      diagnostic(format_args!("{message}"));
   })
}

/// Runs a compiler with its stderr captured, for the cache to keep.
#[inline]
pub fn run<Argument>(
   program: &OsStr,
   args: &[Argument],
   slot: Option<Slot<'_>>,
   environment: &[(&str, &OsStr)],
) -> io::Result<RunResult>
where
   Argument: AsRef<OsStr>,
{
   let mut command = Command::new(program);
   command
      .args(args)
      .envs(environment.iter().copied())
      .stdin(Stdio::inherit())
      .stdout(Stdio::inherit())
      .stderr(Stdio::piped());
   if slot.is_some() {
      command.env("MUMMER_SLOT", "1");
   }
   let result = command.output().map(|output| {
      RunResult {
         status: status_code(output.status),
         stderr: output.stderr,
      }
   });
   drop(slot);
   started(program, result, |_message| {})
}

/// A program that could not start as exit status 127 with the reason as its
/// stderr, the way a shell reports it.
fn started<Report>(
   program: &OsStr,
   result: io::Result<RunResult>,
   report: Report,
) -> io::Result<RunResult>
where
   Report: FnOnce(&str),
{
   match result {
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
         let message = format!("mummer could not start {}, {error}", program.display());
         report(&message);
         Ok(RunResult {
            status: 127,
            stderr: format!("{message}\n").into_bytes(),
         })
      },
      other => other,
   }
}

/// A child that must succeed, its failure an error naming the command.
pub trait Checked {
   fn checked(&mut self) -> io::Result<()>;
   /// Its stdout, with stderr passed through.
   fn checked_output(&mut self) -> io::Result<Vec<u8>>;
}

impl Checked for Command {
   #[inline]
   fn checked(&mut self) -> io::Result<()> {
      let status = self.status()?;
      if status.success() {
         return Ok(());
      }
      Err(io::Error::other(format!("{self:?} failed, {status}")))
   }

   #[inline]
   fn checked_output(&mut self) -> io::Result<Vec<u8>> {
      let output = self.stderr(Stdio::inherit()).output()?;
      if output.status.success() {
         return Ok(output.stdout);
      }
      Err(io::Error::other(format!(
         "{self:?} failed, {}",
         output.status
      )))
   }
}

#[inline]
pub fn socket() -> PathBuf {
   var_os("MUMMER_SOCK").map_or_else(
      || {
         let store = Path::new(STORE_DIR);
         store
            .parent()
            .unwrap_or_else(|| Path::new("/nix"))
            .join("var/nix/mummerd/socket")
      },
      PathBuf::from,
   )
}

#[inline]
pub fn text_arguments(args: &[OsString]) -> Result<Vec<String>> {
   args
      .iter()
      .map(|arg| {
         arg.to_str()
            .map(str::to_owned)
            .ok_or_else(|| report!("argument {arg:?} is not UTF-8"))
      })
      .collect()
}

#[inline]
pub fn run_main<Dispatch>(dispatch: Dispatch) -> ExitCode
where
   Dispatch: FnOnce() -> Result<i32>,
{
   let asked = args_os().any(|arg| arg == "-h" || arg == "--help" || arg == "--version");
   let result = dispatch().or_else(|error| {
      match error.downcast_ref::<ArgumentError>() {
         Some(parsed) if parsed.is_exit() && asked => {
            writeln!(io::stdout().lock(), "{parsed}")?;
            Ok(0_i32)
         },
         Some(parsed) if parsed.is_exit() => {
            diagnostic(format_args!("{parsed}"));
            Ok(1_i32)
         },
         _ => Err(error),
      }
   });
   match result {
      Ok(status) => ExitCode::from(u8::try_from(status).unwrap_or(1)),
      Err(error) => {
         diagnostic(format_args!("{error:#}"));
         ExitCode::FAILURE
      },
   }
}

#[inline]
pub fn expand_response_files(args: &[String]) -> io::Result<Vec<String>> {
   let mut output = Vec::new();
   for arg in args {
      let Some(path) = arg.strip_prefix('@') else {
         output.push(arg.to_owned());
         continue;
      };
      let text = match read_to_string(path) {
         Ok(text) => text,
         Err(error) if error.kind() == io::ErrorKind::NotFound => {
            output.push(arg.to_owned());
            continue;
         },
         Err(error) => return Err(error),
      };
      let mut word = String::new();
      let mut quoted = None;
      let mut started = false;
      let mut characters = text.chars();
      while let Some(character) = characters.next() {
         if character == '\\' {
            word.push(characters.next().unwrap_or('\\'));
            started = true;
         } else if quoted == Some(character) {
            quoted = None;
         } else if quoted.is_none() && matches!(character, '\'' | '"') {
            quoted = Some(character);
            started = true;
         } else if quoted.is_none() && character.is_whitespace() {
            if started {
               output.push(take(&mut word));
            }
            started = false;
         } else {
            word.push(character);
            started = true;
         }
      }
      if quoted.is_some() {
         return Err(io::Error::other(
            "unterminated quote in compiler response file",
         ));
      }
      if started {
         output.push(word);
      }
   }
   Ok(output)
}
