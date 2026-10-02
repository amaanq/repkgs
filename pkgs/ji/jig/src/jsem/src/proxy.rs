//! GHC 9.14 uses a POSIX semaphore while Cabal 3.18 serves socket tokens.
//! Cabal already owns GHC's implicit first token, which needs a jigd slot.
//! Extra capabilities hold both grants, including the semaphore's idle spare.
//! Closing their backing connections refunds both grants after the child exits.
//! Without a reachable jigd the cabal token alone admits a capability.

use std::{
   io::{
      self,
      Read as _,
      Write as _,
   },
   os::unix::net::UnixStream,
   path::{
      Path,
      PathBuf,
   },
};

use crate::sem::Sem;

enum Provider {
   Cabal,
   Daemon,
}

struct Request {
   stream:   UnixStream,
   reply:    [u8; 3],
   received: usize,
   provider: Provider,
}

impl Request {
   fn open(path: &Path, provider: Provider, build: &str) -> io::Result<Self> {
      let mut stream = UnixStream::connect(path)?;

      match provider {
         Provider::Cabal => stream.write_all(b"-")?,
         Provider::Daemon => writeln!(stream, "SLOT {build}")?,
      }

      stream.set_nonblocking(true)?;
      Ok(Self {
         stream,
         reply: [0; 3],
         received: 0,
         provider,
      })
   }

   fn granted(&mut self) -> io::Result<bool> {
      let expected: &[u8] = match self.provider {
         Provider::Cabal => b".",
         Provider::Daemon => b"OK\n",
      };

      match self
         .stream
         .read(&mut self.reply[self.received..expected.len()])
      {
         Ok(0) => {
            return Err(io::Error::new(
               io::ErrorKind::UnexpectedEof,
               "token server disconnected",
            ));
         },
         Ok(count) => self.received += count,
         Err(error)
            if matches!(
               error.kind(),
               io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
         {
            return Ok(false);
         },
         Err(error) => return Err(error),
      }

      if self.received < expected.len() {
         return Ok(false);
      }
      if self.reply[..self.received] != *expected {
         return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "token server refused a grant",
         ));
      }

      Ok(true)
   }

   fn wait(mut self) -> io::Result<UnixStream> {
      self.stream.set_nonblocking(false)?;

      while !self.granted()? {}

      Ok(self.stream)
   }
}

enum Pending {
   Cabal(Request),
   Daemon {
      cabal:   UnixStream,
      request: Request,
   },
}

struct Grant {
   _cabal:  UnixStream,
   _daemon: Option<UnixStream>,
}

pub struct Proxy<'semaphore> {
   semaphore: &'semaphore Sem,
   cabal:     PathBuf,
   daemon:    Option<PathBuf>,
   build:     String,
   pending:   Option<Pending>,
   held:      Vec<Grant>,
   _initial:  Option<UnixStream>,
}

impl<'semaphore> Proxy<'semaphore> {
   pub fn new(
      semaphore: &'semaphore Sem,
      cabal: PathBuf,
      daemon: PathBuf,
      build: String,
   ) -> io::Result<Self> {
      let initial = match Request::open(&daemon, Provider::Daemon, &build) {
         Ok(request) => Some(request.wait()?),
         Err(error)
            if matches!(
               error.kind(),
               io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
         {
            None
         },
         Err(error) => return Err(error),
      };

      Ok(Self {
         semaphore,
         cabal,
         daemon: initial.is_some().then_some(daemon),
         build,
         pending: None,
         held: Vec::new(),
         _initial: initial,
      })
   }

   pub fn tick(&mut self) -> io::Result<()> {
      let mut idle = 0;

      while idle < self.held.len() && self.semaphore.take()? {
         idle += 1;
      }

      if idle > 0 {
         self.held.truncate(self.held.len() - idle + 1);
         self.pending = None;
         return self.semaphore.post();
      }

      let pending = self.pending.take().map_or_else(
         || Request::open(&self.cabal, Provider::Cabal, &self.build).map(Pending::Cabal),
         Ok,
      )?;

      self.pending = match pending {
         Pending::Cabal(mut request) => {
            if !request.granted()? {
               Some(Pending::Cabal(request))
            } else if let Some(ref daemon) = self.daemon {
               Some(Pending::Daemon {
                  cabal:   request.stream,
                  request: Request::open(daemon, Provider::Daemon, &self.build)?,
               })
            } else {
               self.held.push(Grant {
                  _cabal:  request.stream,
                  _daemon: None,
               });
               self.semaphore.post()?;
               None
            }
         },
         Pending::Daemon { cabal, mut request } => {
            if request.granted()? {
               self.held.push(Grant {
                  _cabal:  cabal,
                  _daemon: Some(request.stream),
               });

               self.semaphore.post()?;
               None
            } else {
               Some(Pending::Daemon { cabal, request })
            }
         },
      };

      Ok(())
   }
}
