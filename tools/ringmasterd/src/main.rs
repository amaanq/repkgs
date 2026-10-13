use std::{
   collections::HashMap,
   fs::{
      Permissions,
      remove_file,
      set_permissions,
      symlink_metadata,
   },
   io::{
      self,
      IoSliceMut,
      Write as _,
   },
   mem::MaybeUninit,
   os::{
      fd::AsFd as _,
      unix::{
         fs::{
            FileTypeExt as _,
            PermissionsExt as _,
         },
         net::{
            UnixListener,
            UnixStream,
         },
      },
   },
   path::PathBuf,
};

use aya::{
   Btf,
   BtfError,
   Ebpf,
   include_bytes_aligned,
   maps::{
      HashMap as BpfHashMap,
      MapData,
      RingBuf,
   },
   programs::{
      BtfTracePoint,
      FEntry,
      ProgramError,
   },
};
use misstep::{
   Result,
   ResultExt as _,
   report,
};
use pound::Parse;
use ringmasterd::{
   ESCAPES,
   Reply,
   Request,
   session,
};
use rustix::{
   event::{
      PollFd,
      PollFlags,
      poll,
   },
   io::Errno,
   net::{
      RecvAncillaryBuffer,
      RecvAncillaryMessage,
      RecvFlags,
      recvmsg,
      sockopt::set_socket_passcred,
   },
   rand::{
      GetRandomFlags,
      getrandom,
   },
};

/// Traces the processes of test runs that ask, with BPF programs on the VFS
/// calls every path goes through, and streams what they reach back to them.
#[derive(Parse)]
#[pound(name = "ringmasterd")]
struct Options {
   /// The Unix socket to listen on.
   socket: PathBuf,
}

/// Each fentry program and the kernel functions it may attach to, newest
/// name first, since the VFS renames them between releases.
const ENTRIES: &[(&str, &[&str])] = &[
   ("open", &["do_file_open", "do_filp_open"]),
   ("lookup", &["filename_lookup"]),
   ("unlink", &["filename_unlinkat", "do_unlinkat"]),
   ("rmdir", &["filename_rmdir", "do_rmdir"]),
   ("mkdir", &["filename_mkdirat", "do_mkdirat"]),
   ("mknod", &["filename_mknodat", "do_mknodat"]),
   ("symlink", &["filename_symlinkat", "do_symlinkat"]),
   ("link", &["filename_linkat", "do_linkat"]),
   ("rename", &["filename_renameat2", "do_renameat2"]),
   ("truncate", &["vfs_truncate"]),
   ("listing", &["iterate_dir"]),
];

const TRACEPOINTS: &[(&str, &str)] = &[
   ("fork", "sched_process_fork"),
   ("exec", "sched_process_exec"),
   ("reaped", "sched_process_free"),
   ("syscall", "sys_enter"),
];

struct Kernel {
   /// Kept so the programs stay attached.
   _object: Ebpf,
   traced:  BpfHashMap<MapData, u32, u64>,
   lost:    BpfHashMap<MapData, u64, u64>,
   events:  RingBuf<MapData>,
}

impl Kernel {
   fn load() -> Result<Self> {
      let btf = Btf::from_sys_fs().context("this kernel has no BTF")?;
      let mut object = Ebpf::load(include_bytes_aligned!(concat!(
         env!("OUT_DIR"),
         "/trace.bpf.o"
      )))
      .context("could not load the BPF object")?;
      for &(program, candidates) in ENTRIES {
         let entry: &mut FEntry = object
            .program_mut(program)
            .ok_or_else(|| report!("no program {program}"))?
            .try_into()?;
         let mut target = None;
         for &name in candidates {
            match entry.load(name, &btf) {
               Ok(()) => {
                  target = Some(name);
                  break;
               },
               Err(ProgramError::Btf(BtfError::UnknownBtfTypeName { .. })) => {},
               Err(error) => {
                  return Err(error)
                     .with_context(|| format!("could not load {program} for {name}"));
               },
            }
         }
         let attached = target
            .ok_or_else(|| report!("this kernel has none of {candidates:?} for {program}"))?;
         entry
            .attach()
            .with_context(|| format!("could not attach {program} to {attached}"))?;
      }
      for &(program, tracepoint) in TRACEPOINTS {
         let point: &mut BtfTracePoint = object
            .program_mut(program)
            .ok_or_else(|| report!("no program {program}"))?
            .try_into()?;
         point
            .load(tracepoint, &btf)
            .with_context(|| format!("could not load {program} for {tracepoint}"))?;
         point
            .attach()
            .with_context(|| format!("could not attach {program} to {tracepoint}"))?;
      }
      let mut escapes: BpfHashMap<MapData, u32, u32> = BpfHashMap::try_from(
         object
            .take_map("escapes")
            .ok_or_else(|| report!("no escapes map"))?,
      )?;
      for &(number, _) in ESCAPES {
         escapes.insert(u32::try_from(number)?, 1, 0)?;
      }
      let traced = BpfHashMap::try_from(
         object
            .take_map("traced")
            .ok_or_else(|| report!("no traced map"))?,
      )?;
      let lost = BpfHashMap::try_from(
         object
            .take_map("lost")
            .ok_or_else(|| report!("no lost map"))?,
      )?;
      let events = RingBuf::try_from(
         object
            .take_map("events")
            .ok_or_else(|| report!("no events map"))?,
      )?;
      Ok(Self {
         _object: object,
         traced,
         lost,
         events,
      })
   }

   /// Stops tracing a session's processes and says how many of their
   /// accesses never reached the ring.
   fn forget(&mut self, token: u64) -> u64 {
      let members = self
         .traced
         .iter()
         .filter_map(|entry| entry.ok().filter(|pair| pair.1 == token).map(|pair| pair.0))
         .collect::<Vec<_>>();
      for member in members {
         drop(self.traced.remove(&member));
      }
      let lost = self.lost.get(&token, 0).unwrap_or(0);
      drop(self.lost.remove(&token));
      lost
   }
}

/// How far a client may fall behind before its records are dropped and
/// counted lost, rather than held for it.
const BACKLOG: usize = 1 << 28;

/// A connection, a session's stream once it opened one.
struct Client {
   stream:  UnixStream,
   session: Option<u64>,
   /// Bytes the client has not taken yet, from `sent` on.
   queued:  Vec<u8>,
   sent:    usize,
   /// Records dropped because the client fell too far behind.
   dropped: u64,
   /// Answered for the last time, so closed once `queued` is sent.
   ending:  bool,
}

impl Client {
   const fn new(stream: UnixStream) -> Self {
      Self {
         stream,
         session: None,
         queued: Vec::new(),
         sent: 0,
         dropped: 0,
         ending: false,
      }
   }

   const fn waiting(&self) -> bool {
      self.sent < self.queued.len()
   }

   /// Sends what the socket takes without blocking, false when the client
   /// is gone.
   fn flush(&mut self) -> bool {
      while self.waiting() {
         match self.stream.write(&self.queued[self.sent..]) {
            Ok(0) => return false,
            Ok(written) => self.sent += written,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
            Err(_) => return false,
         }
      }
      if self.sent * 2 >= self.queued.len() {
         self.queued.drain(..self.sent);
         self.sent = 0;
      }
      true
   }
}

struct Daemon {
   kernel:   Kernel,
   listener: UnixListener,
   clients:  Vec<Client>,
   /// Which client each session streams to.
   sessions: HashMap<u64, usize>,
}

impl Daemon {
   /// Sends every record waiting in the ring to its session, dropping those
   /// of sessions already gone.
   fn drain(&mut self) {
      while let Some(item) = self.kernel.events.next() {
         let raw: &[u8] = &item;
         let Some(index) = session(raw).and_then(|token| self.sessions.get(&token).copied()) else {
            continue;
         };
         let client = &mut self.clients[index];
         match Reply::record(raw) {
            Ok(frame) if client.queued.len() - client.sent + frame.len() <= BACKLOG => {
               client.queued.extend_from_slice(&frame);
            },
            _ => client.dropped += 1,
         }
      }
   }

   fn accept(&mut self) -> io::Result<()> {
      loop {
         match self.listener.accept() {
            Ok((stream, _)) => {
               stream.set_nonblocking(true)?;
               self.clients.push(Client::new(stream));
            },
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error),
         }
      }
   }

   /// Handles what a client sent, false when the connection is done.
   fn serve(&mut self, index: usize) -> bool {
      let mut bytes = [0_u8; 9];
      let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmCredentials(1))];
      let mut control = RecvAncillaryBuffer::new(&mut space);
      let Ok(received) = recvmsg(
         self.clients[index].stream.as_fd(),
         &mut [IoSliceMut::new(&mut bytes)],
         &mut control,
         RecvFlags::empty(),
      ) else {
         return false;
      };
      let sender = control.drain().find_map(|message| {
         if let RecvAncillaryMessage::ScmCredentials(credentials) = message {
            Some(credentials.pid)
         } else {
            None
         }
      });
      let Ok(request) = Request::read(&mut &bytes[..received.bytes]) else {
         return false;
      };
      match (request, self.clients[index].session) {
         (Request::Open, None) => {
            let mut token = [0_u8; 8];
            if getrandom(&mut token, GetRandomFlags::empty()).is_err() {
               return false;
            }
            let opened = u64::from_ne_bytes(token);
            self.sessions.insert(opened, index);
            let client = &mut self.clients[index];
            client.session = Some(opened);
            client.queued.extend_from_slice(&Reply::opened(opened));
            true
         },
         (Request::Join(token), None) => {
            let (Some(pid), true) = (sender, self.sessions.contains_key(&token)) else {
               return false;
            };
            let Ok(tgid) = u32::try_from(pid.as_raw_nonzero().get()) else {
               return false;
            };
            if self.kernel.traced.insert(tgid, token, 0).is_err() {
               return false;
            }
            let client = &mut self.clients[index];
            client.queued.extend_from_slice(&Reply::joined());
            client.ending = true;
            true
         },
         (Request::End, Some(token)) => {
            self.drain();
            let lost = self.kernel.forget(token);
            self.sessions.remove(&token);
            let client = &mut self.clients[index];
            client.session = None;
            client
               .queued
               .extend_from_slice(&Reply::done(lost + client.dropped));
            client.ending = true;
            true
         },
         _ => false,
      }
   }

   /// Closes a connection, ending its session unannounced if it had one.
   fn close(&mut self, index: usize) {
      if let Some(token) = self.clients[index].session.take() {
         self.kernel.forget(token);
         self.sessions.remove(&token);
      }
      self.clients.swap_remove(index);
      if let Some(moved) = self.clients.get(index).and_then(|client| client.session) {
         self.sessions.insert(moved, index);
      }
   }

   fn run(&mut self) -> Result<()> {
      loop {
         let mut polled = vec![
            PollFd::new(&self.listener, PollFlags::IN),
            PollFd::new(&self.kernel.events, PollFlags::IN),
         ];
         polled.extend(self.clients.iter().map(|client| {
            let mut flags = PollFlags::empty();
            flags.set(PollFlags::IN, !client.ending);
            flags.set(PollFlags::OUT, client.waiting());
            PollFd::new(&client.stream, flags)
         }));
         match poll(&mut polled, None) {
            Ok(_) | Err(Errno::INTR) => {},
            Err(error) => return Err(error).context("poll failed"),
         }
         let ready = polled
            .iter()
            .map(|entry| {
               entry
                  .revents()
                  .intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR)
            })
            .collect::<Vec<_>>();
         drop(polled);
         if ready[1] {
            self.drain();
         }
         if ready[0] {
            self.accept().context("could not accept a connection")?;
         }
         for index in (0..ready.len() - 2).rev() {
            if ready[index + 2] && !self.clients[index].ending && !self.serve(index) {
               self.close(index);
            }
         }
         for index in (0..self.clients.len()).rev() {
            let client = &mut self.clients[index];
            if !client.flush() || (client.ending && !client.waiting()) {
               self.close(index);
            }
         }
      }
   }
}

fn bind(path: &PathBuf) -> io::Result<UnixListener> {
   if symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_socket()) {
      remove_file(path)?;
   }
   let listener = UnixListener::bind(path)?;
   set_permissions(path, Permissions::from_mode(0o666))?;
   set_socket_passcred(&listener, true)?;
   listener.set_nonblocking(true)?;
   Ok(listener)
}

fn main() -> Result<()> {
   let options = Options::parse();
   let kernel = Kernel::load()?;
   let listener = bind(&options.socket).context("could not bind the socket")?;
   drop(writeln!(
      io::stderr().lock(),
      "ringmasterd: listening on {}",
      options.socket.display()
   ));
   Daemon {
      kernel,
      listener,
      clients: Vec::new(),
      sessions: HashMap::new(),
   }
   .run()
}
