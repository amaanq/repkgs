use std::{
   fmt::Display,
   io::{
      self,
      Write as _,
   },
   time::Instant,
};

use mummer::{
   process::{
      Outcome,
      RunResult,
      Tool,
      diagnostic,
      log,
      log_uncached,
   },
   store::Store,
};
use tightrope::{
   client::Client,
   digest::Digest,
   key::Key,
};

use crate::manifest::{
   Miss,
   lookup,
};

/// A compiler the cache stands in front of, one invocation of it.
pub trait Wrapper {
   const TOOL: Tool;
   /// What a compile leaves for `store`, the scratch files it wrote.
   type Compiled;

   /// The store manifests are built and checked against.
   fn manifests(&mut self) -> &mut Store;
   fn replay(&mut self, cache: &mut Client, result: &Digest) -> Result<i32, Miss>;
   fn compile(&mut self, cache: &mut Client) -> io::Result<(RunResult, Self::Compiled)>;
   /// Stores the result under `request`, false when it is not one to keep.
   fn store(
      &mut self,
      cache: &mut Client,
      request: &Key,
      result: &RunResult,
      compiled: Self::Compiled,
   ) -> io::Result<bool>;
}

/// Answers `request` from the cache, else compiles and stores the result.
pub fn serve<Compiler>(
   job: &mut Compiler,
   cache: &mut Client,
   request: &Key,
   subject: &str,
   started: Instant,
) -> io::Result<i32>
where
   Compiler: Wrapper,
{
   let hit = lookup(cache, job.manifests(), request).and_then(|result| job.replay(cache, &result));
   let miss = match hit {
      Ok(status) => {
         let outcome = if status == 0_i32 {
            Outcome::Cached
         } else {
            Outcome::CachedError
         };
         log(Compiler::TOOL, outcome, subject, started);
         return Ok(status);
      },
      Err(Miss::Failed(error)) => {
         diagnostic(format_args!(
            "mummer cache replay failed, compiling again, {error}"
         ));
         Miss::Failed(error)
      },
      Err(miss) => miss,
   };
   let (result, compiled) = job.compile(cache)?;
   io::stderr().lock().write_all(&result.stderr)?;
   let stored = job
      .store(cache, request, &result, compiled)
      .unwrap_or_else(|error| {
         diagnostic(format_args!(
            "mummer could not store the {} result, {error}",
            Compiler::TOOL
         ));
         false
      });
   let outcome = match (stored, result.status == 0_i32) {
      (true, true) => Outcome::Compiled,
      (true, false) => Outcome::CompiledError,
      (false, true) => Outcome::CompiledUnstored,
      (false, false) => Outcome::Failed,
   };
   log(
      Compiler::TOOL,
      outcome,
      format_args!("{subject} {miss}"),
      started,
   );
   Ok(result.status)
}

/// Logs a compile that ran without the cache, and why.
pub fn uncached<Subject>(
   tool: Tool,
   outcome: Outcome,
   subject: Subject,
   args: &[String],
   started: Instant,
) where
   Subject: Display,
{
   log(tool, outcome, subject, started);
   log_uncached(outcome, args);
}
