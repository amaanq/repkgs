//! A command traced by ringmasterd, whose kernel programs see every path the
//! command and its descendants reach, through `io_uring` too, and stream them
//! here after the calls return.

use std::{
   collections::{
      BTreeSet,
      HashMap,
   },
   ffi::{
      OsStr,
      OsString,
   },
   io::{
      self,
      BufReader,
      Write as _,
   },
   os::unix::{
      ffi::{
         OsStrExt as _,
         OsStringExt as _,
      },
      net::UnixStream,
      process::CommandExt as _,
   },
   path::{
      Path,
      PathBuf,
   },
   process::Command,
   thread::spawn,
};

use mummer::store::normalize;
use ringmasterd::{
   Access,
   Named,
   Record,
   Reply,
   Request,
   SOCKET,
   join,
};
use rustix::net::SocketAddrUnix;

use crate::trace::{
   Accesses,
   Event,
   Mark,
   Outcome,
   Tests,
   record,
   writes,
};

/// `__FMODE_EXEC` from linux/fs.h, which the kernel's own opens of a program
/// and its interpreter carry.
const FMODE_EXEC: u32 = 0x20;

fn send(stream: &mut UnixStream, request: Request) -> io::Result<()> {
   let (bytes, length) = request.encode();
   stream.write_all(&bytes[..length])
}

/// Where a call's path leads, none for an empty one.
fn resolve(named: &Named) -> Option<PathBuf> {
   if named.name.is_empty() {
      return None;
   }
   let base = Path::new(OsStr::from_bytes(&named.base));
   Some(normalize(&base.join(OsStr::from_bytes(&named.name))))
}

/// The test each traced process belongs to, followed through forks and execs
/// as the records arrive, since the processes may be gone by then.
#[derive(Default)]
struct Processes {
   units:     HashMap<u32, Option<String>>,
   /// What each process belonged to when forked, which an exec of a program
   /// that is no test leaves it in.
   forked:    HashMap<u32, Option<String>>,
   /// Programs a process opened to exec, read by whatever the exec makes it.
   loading:   HashMap<u32, Vec<PathBuf>>,
   clobbered: BTreeSet<PathBuf>,
}

impl Processes {
   /// Records a process's pending program opens as its own reads, once it
   /// does anything but finish the exec they were for.
   fn settle(&mut self, tgid: u32, accesses: &mut Accesses, traced: bool, mark: Mark) {
      let Some(loaded) = self.loading.remove(&tgid) else {
         return;
      };
      let unit = self.units.get(&tgid).cloned().flatten();
      for path in loaded {
         record(accesses, Event::Read(path), unit.as_deref(), traced, mark);
      }
   }

   /// Records a path the process took away. The first time a file it read
   /// goes is the one seccomp would have kept as it was.
   fn clobber(
      &mut self,
      accesses: &mut Accesses,
      path: PathBuf,
      (unit, traced): (Option<&str>, bool),
      mark: Mark,
   ) {
      if self.clobbered.insert(path.clone()) && accesses.paths.contains_key(&path) {
         accesses.replaced.insert(path.clone());
      }
      record(accesses, Event::Looked(path), unit, traced, mark);
   }

   fn note(
      &mut self,
      Record { tgid, access }: Record,
      accesses: &mut Accesses,
      tests: Option<&dyn Tests>,
      mark: Mark,
   ) {
      let traced = tests.is_some();
      let execing = matches!(access, Access::Exec { .. })
         || matches!(access, Access::Open { flags, .. } if flags & FMODE_EXEC != 0);
      if !execing {
         self.settle(tgid, accesses, traced, mark);
      }
      let unit = self.units.get(&tgid).cloned().flatten();
      let event = match access {
         Access::Open { named, flags } => {
            let Some(path) = resolve(&named) else {
               return;
            };
            if flags & FMODE_EXEC != 0 {
               self.loading.entry(tgid).or_default().push(path);
               return;
            }
            if writes(u64::from(flags)) {
               self.clobber(accesses, path.clone(), (unit.as_deref(), traced), mark);
            }
            Event::Read(path)
         },
         Access::Lookup(named) => {
            let Some(path) = resolve(&named) else {
               return;
            };
            Event::Looked(path)
         },
         Access::Clobber(named) => {
            if let Some(path) = resolve(&named) {
               self.clobber(accesses, path, (unit.as_deref(), traced), mark);
            }
            return;
         },
         Access::Listing(raw) => Event::Listed(PathBuf::from(OsString::from_vec(raw))),
         Access::Exec { named, args } => {
            let Some(path) = resolve(&named) else {
               return;
            };
            let argv = args
               .split(|byte| *byte == 0)
               .filter(|arg| !arg.is_empty())
               .map(|arg| OsStr::from_bytes(arg).to_owned())
               .collect::<Vec<_>>();
            let cwd = Path::new(OsStr::from_bytes(&named.base));
            let became = tests
               .and_then(|harness| harness.unit(&path, &argv, cwd))
               .or_else(|| self.forked.get(&tgid).cloned().flatten());
            self.units.insert(tgid, became.clone());
            self.settle(tgid, accesses, traced, mark);
            record(accesses, Event::Exec(path), became.as_deref(), traced, mark);
            return;
         },
         Access::Fork { child } => {
            self.loading.remove(&child);
            self.forked.insert(child, unit.clone());
            self.units.insert(child, unit);
            return;
         },
         Access::Escape(escape) => {
            accesses.escaped.get_or_insert_with(|| escape.reason());
            return;
         },
      };
      record(accesses, event, unit.as_deref(), traced, mark);
   }
}

/// Applies the session's records until the daemon says it is over.
fn collect(stream: UnixStream, tests: Option<&dyn Tests>, mark: Mark) -> io::Result<Accesses> {
   let mut reader = BufReader::new(stream);
   let mut accesses = Accesses::default();
   let mut processes = Processes::default();
   loop {
      match Reply::read(&mut reader)? {
         Reply::Record(found) => processes.note(found, &mut accesses, tests, mark),
         Reply::Done { lost } => {
            let pending = processes.loading.keys().copied().collect::<Vec<_>>();
            for tgid in pending {
               processes.settle(tgid, &mut accesses, tests.is_some(), mark);
            }
            if lost > 0 {
               accesses
                  .escaped
                  .get_or_insert("records the kernel ring lost");
            }
            return Ok(accesses);
         },
         Reply::Opened(_) | Reply::Joined => {
            return Err(io::Error::other("ringmasterd replied out of turn"));
         },
      }
   }
}

/// A session open with the daemon, before its command runs.
pub struct Session {
   stream: UnixStream,
   token:  u64,
}

impl Session {
   /// Fails with `NotFound` when no daemon is mapped into the sandbox.
   pub fn open() -> io::Result<Self> {
      let mut stream = UnixStream::connect(SOCKET)?;
      send(&mut stream, Request::Open)?;
      let Reply::Opened(token) = Reply::read(&mut stream)? else {
         return Err(io::Error::other("ringmasterd did not open a session"));
      };
      Ok(Self { stream, token })
   }

   /// Runs a command from `make` traced, or untraced when it cannot join the
   /// session, so an exec failure comes back as the untraced spawn's error.
   pub fn run(
      mut self,
      make: &dyn Fn() -> io::Result<Command>,
      tests: Option<&'static dyn Tests>,
      mark: Mark,
   ) -> io::Result<Outcome> {
      let address = SocketAddrUnix::new(SOCKET)?;
      let token = self.token;
      let reading = self.stream.try_clone()?;
      let collector = spawn(move || collect(reading, tests, mark));
      let mut command = make()?;
      // SAFETY: `join` makes only raw system calls and allocates nothing
      let spawned = unsafe { command.pre_exec(move || join(&address, token)) }.spawn();
      let waited = spawned.map(|mut child| child.wait());
      send(&mut self.stream, Request::End)?;
      let accesses = collector
         .join()
         .map_err(|_panicked| io::Error::other("the ring reader panicked"))??;
      match waited {
         Ok(status) => Ok(Outcome::Traced(status?, accesses)),
         Err(error) => Ok(Outcome::Untraced(make()?.status()?, error)),
      }
   }
}
