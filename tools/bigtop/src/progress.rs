use std::{
   env::var_os,
   ffi::OsString,
   fs::{
      File,
      metadata,
   },
   io::{
      self,
      Seek as _,
      SeekFrom,
   },
   sync::mpsc::{
      RecvTimeoutError,
      channel,
   },
   thread::scope,
   time::Duration,
};

use mummer::{
   process::logged,
   tree::{
      note,
      set_phase,
   },
};

/// Compiles and cache hits mummer logged since the phase began.
struct Tally {
   log:      OsString,
   offset:   u64,
   partial:  Vec<u8>,
   compiled: usize,
   cached:   usize,
}

impl Tally {
   /// Reads the new log lines, true if the counts changed.
   fn advance(&mut self) -> io::Result<bool> {
      let mut file = match File::open(&self.log) {
         Ok(file) => file,
         Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
         Err(error) => return Err(error),
      };
      file.seek(SeekFrom::Start(self.offset))?;

      self.offset += io::copy(&mut file, &mut self.partial)?;

      let Some(end) = self.partial.iter().rposition(|&byte| byte == b'\n') else {
         return Ok(false);
      };
      let lines = self.partial.drain(..=end).collect::<Vec<u8>>();
      let before = (self.compiled, self.cached);

      for run in logged(str::from_utf8(&lines).map_err(io::Error::other)?) {
         if run.outcome.is_compiled() {
            self.compiled += 1;
         } else if run.outcome.is_cached() {
            self.cached += 1;
         }
      }

      Ok((self.compiled, self.cached) != before)
   }
}

/// Runs a phase, adding the compile counts to its name every second.
pub fn phase<Body, Done>(name: &str, body: Body) -> io::Result<Done>
where
   Body: FnOnce() -> io::Result<Done>,
{
   set_phase(name);

   let Some(log) = var_os("MUMMER_LOG").filter(|path| !path.is_empty()) else {
      return body();
   };
   let mut tally = Tally {
      offset: metadata(&log).map_or(0, |found| found.len()),
      log,
      partial: Vec::new(),
      compiled: 0,
      cached: 0,
   };

   let (finished, ticks) = channel::<()>();

   scope(|threads| {
      threads.spawn(move || {
         while ticks.recv_timeout(Duration::from_secs(1)) == Err(RecvTimeoutError::Timeout) {
            match tally.advance() {
               Ok(false) => {},
               Ok(true) => {
                  set_phase(&format!(
                     "{name} ({} compiled, {} cached)",
                     tally.compiled, tally.cached
                  ));
               },
               Err(error) => {
                  note(
                     "progress",
                     &format!("stopped reading the mummer log, {error}"),
                  );
                  return;
               },
            }
         }
      });
      let result = body();
      drop(finished);
      result
   })
}
