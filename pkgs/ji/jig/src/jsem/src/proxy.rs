//! semaphore-compat v2 as cabal serves it and ghc speaks it. A stream socket
//! under `$TMPDIR/semaphore-compat/<name>`, one byte per command, `-` waits
//! for a token and answers `.`, `?` tries and answers `.` or `!`, `+` gives
//! one back, and closing a connection returns every token it holds. jsem sits
//! between the two and backs each token ghc asks for with whichever answers
//! first, a token from cabal's socket or a slot from jigd.

use std::{
   fs::remove_file,
   io::{
      self,
      BufReader,
      Read as _,
      Write as _,
   },
   os::unix::net::{
      UnixListener,
      UnixStream,
   },
   path::PathBuf,
   sync::Arc,
   thread::spawn,
};

use jig::process::diagnostic;
use rustix::event::{
   PollFd,
   PollFlags,
   poll,
};
use wire::read_line;

const WAIT: u8 = b'-';
const TRY: u8 = b'?';
const RELEASE: u8 = b'+';
const OK: u8 = b'.';
const FAIL: u8 = b'!';

struct Upstreams {
   cabal:  PathBuf,
   daemon: PathBuf,
   build:  String,
}

pub struct Proxy {
   path: PathBuf,
}

impl Proxy {
   /// Listen on `path` and serve every ghc connection on its own thread.
   pub fn bind(path: PathBuf, cabal: PathBuf, daemon: PathBuf, build: String) -> io::Result<Self> {
      let listener = UnixListener::bind(&path)?;
      let upstreams = Arc::new(Upstreams {
         cabal,
         daemon,
         build,
      });
      spawn(move || {
         for accepted in listener.incoming() {
            let Ok(stream) = accepted else { break };
            let shared = Arc::clone(&upstreams);
            spawn(move || {
               if let Err(error) = serve(stream, &shared) {
                  diagnostic(format_args!("jsem: {error}"));
               }
            });
         }
      });
      Ok(Self { path })
   }
}

impl Drop for Proxy {
   fn drop(&mut self) {
      drop(remove_file(&self.path));
   }
}

fn serve(mut ghc: UnixStream, upstreams: &Upstreams) -> io::Result<()> {
   let mut held: Vec<UnixStream> = Vec::new();
   loop {
      let mut command = [0_u8];
      if ghc.read(&mut command)? == 0 {
         return Ok(());
      }
      let reply = match command[0] {
         WAIT => {
            let Some(token) = first_of_either(upstreams, &ghc)? else {
               return Ok(());
            };

            held.push(token);
            OK
         },
         TRY => {
            try_cabal(upstreams)?.map_or(FAIL, |token| {
               held.push(token);
               OK
            })
         },
         RELEASE if held.pop().is_some() => OK,
         _ => FAIL,
      };
      ghc.write_all(&[reply])?;
   }
}

fn try_cabal(upstreams: &Upstreams) -> io::Result<Option<UnixStream>> {
   let mut token = UnixStream::connect(&upstreams.cabal)?;
   token.write_all(&[TRY])?;
   let mut reply = [0_u8];
   let granted = token.read(&mut reply)? == 1 && reply[0] == OK;
   Ok(granted.then_some(token))
}

/// Ask cabal and jigd at once and keep whichever grants first. Closing the
/// other side cancels its request, or returns the grant if it raced in.
fn first_of_either(upstreams: &Upstreams, ghc: &UnixStream) -> io::Result<Option<UnixStream>> {
   let mut cabal = UnixStream::connect(&upstreams.cabal)
      .and_then(|mut token| token.write_all(&[WAIT]).map(|()| token))
      .ok();
   let mut daemon = UnixStream::connect(&upstreams.daemon)
      .and_then(|mut slot| writeln!(slot, "SLOT {}", upstreams.build).map(|()| slot))
      .ok();
   loop {
      let mut descriptors: Vec<PollFd<'_>> = cabal
         .iter()
         .chain(daemon.iter())
         .map(|stream| PollFd::new(stream, PollFlags::IN))
         .collect();
      if descriptors.is_empty() {
         return Err(io::Error::other(
            "neither cabal's semaphore nor jigd answered",
         ));
      }

      descriptors.push(PollFd::new(ghc, PollFlags::empty()));
      poll(&mut descriptors, None)?;

      if descriptors.last().is_some_and(|client| {
         client
            .revents()
            .intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL)
      }) {
         return Ok(None);
      }

      let mut answers = descriptors
         .iter()
         .map(|descriptor| !descriptor.revents().is_empty());
      let cabal_answered = cabal.is_some() && answers.next() == Some(true);
      let daemon_answered = daemon.is_some() && answers.next() == Some(true);
      drop(answers);
      drop(descriptors);
      if let Some(mut token) = cabal.take() {
         if cabal_answered {
            let mut reply = [0_u8];
            if token.read(&mut reply)? == 1 && reply[0] == OK {
               return Ok(Some(token));
            }
         } else {
            cabal = Some(token);
         }
      }
      if let Some(slot) = daemon.take() {
         if daemon_answered {
            let mut reader = BufReader::new(slot);
            if read_line(&mut reader)?.as_deref() == Some("OK") {
               return Ok(Some(reader.into_inner()));
            }
         } else {
            daemon = Some(slot);
         }
      }
   }
}
