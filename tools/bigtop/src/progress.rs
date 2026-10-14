use std::{
   env::var_os,
   ffi::OsString,
   fs::{
      File,
      metadata,
   },
   io::{
      self,
      PipeReader,
      Read as _,
      Seek as _,
      SeekFrom,
      Write as _,
      pipe,
   },
   os::fd::AsFd as _,
   sync::{
      Arc,
      Mutex,
      PoisonError,
      mpsc::{
         RecvTimeoutError,
         channel,
      },
   },
   thread::{
      scope,
      spawn,
   },
   time::Duration,
};

use mummer::{
   process::logged,
   tree::{
      note,
      note_event,
      phase_event,
      set_phase,
   },
};
use rustix::stdio::{
   dup2_stderr,
   dup2_stdout,
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

/// Copies a phase's output to `out` whole lines at a time, so an event written
/// between them never lands inside a line a child left unfinished.
fn relay(mut reader: PipeReader, out: &Mutex<File>) -> io::Result<()> {
   const CHUNK: usize = 1 << 16;

   let mut pending = Vec::new();
   let mut chunk = vec![0; CHUNK];
   loop {
      let read = reader.read(&mut chunk)?;
      if read == 0 {
         break;
      }
      pending.extend(chunk.iter().take(read));
      let end = match pending.iter().rposition(|&byte| byte == b'\n') {
         Some(at) => at + 1,
         None if pending.len() >= CHUNK => pending.len(),
         None => continue,
      };
      let lines = pending.drain(..end).collect::<Vec<u8>>();
      out.lock()
         .unwrap_or_else(PoisonError::into_inner)
         .write_all(&lines)?;
   }
   out.lock()
      .unwrap_or_else(PoisonError::into_inner)
      .write_all(&pending)
}

fn emit(out: &Mutex<File>, mut event: Vec<u8>) -> io::Result<()> {
   event.push(b'\n');
   out.lock()
      .unwrap_or_else(PoisonError::into_inner)
      .write_all(&event)
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

   let stdout = io::stdout().as_fd().try_clone_to_owned()?;
   let out = Arc::new(Mutex::new(File::from(
      io::stderr().as_fd().try_clone_to_owned()?,
   )));
   let (reader, writer) = pipe()?;
   dup2_stdout(&writer)?;
   dup2_stderr(&writer)?;
   drop(writer);

   let (relayed, relay_ended) = channel::<io::Result<()>>();
   let relay_out = Arc::clone(&out);
   spawn(move || drop(relayed.send(relay(reader, &relay_out))));

   let (finished, ticks) = channel::<()>();
   let ticker_out = Arc::clone(&out);
   let result = scope(|threads| {
      threads.spawn(move || {
         while ticks.recv_timeout(Duration::from_secs(1)) == Err(RecvTimeoutError::Timeout) {
            let shown = match tally.advance() {
               Ok(false) => continue,
               Ok(true) => {
                  emit(
                     &ticker_out,
                     phase_event(&format!(
                        "{name} ({} compiled, {} cached)",
                        tally.compiled, tally.cached
                     )),
                  )
               },
               Err(error) => {
                  drop(emit(
                     &ticker_out,
                     note_event(
                        "progress",
                        &format!("stopped reading the mummer log, {error}"),
                     ),
                  ));
                  return;
               },
            };
            if shown.is_err() {
               return;
            }
         }
      });
      let result = body();
      drop(finished);
      result
   });

   dup2_stdout(&stdout)?;
   dup2_stderr(&*out.lock().unwrap_or_else(PoisonError::into_inner))?;

   // a daemon the phase started can hold the pipe open forever
   if let Ok(Err(error)) = relay_ended.recv_timeout(Duration::from_secs(2)) {
      note(
         "progress",
         &format!("relaying the phase output failed, {error}"),
      );
   }
   result
}
