//! What ringmasterd and the test runs it traces say to each other. A run opens
//! a session and gets its token, the run's first process joins the session
//! between fork and exec, and the daemon streams every access the process and
//! its descendants make until the run ends the session.

use std::{
   error::Error,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   io::{
      self,
      ErrorKind,
      IoSlice,
      Read,
   },
   mem::MaybeUninit,
   ops::Range,
};

use rustix::{
   fd::OwnedFd,
   net::{
      AddressFamily,
      RecvFlags,
      SendAncillaryBuffer,
      SendAncillaryMessage,
      SendFlags,
      SocketAddrUnix,
      SocketFlags,
      SocketType,
      UCred,
      connect,
      recv,
      sendmsg,
      socket_with,
   },
   process::{
      getgid,
      getpid,
      getuid,
   },
};

/// Where the daemon listens, mapped into every build sandbox.
pub const SOCKET: &str = "/nix/var/nix/ringmasterd/socket";

/// A client's message, its tag byte then, for `Join`, the session token.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Request {
   Open,
   Join(u64),
   End,
}

impl Request {
   const OPEN: u8 = 1;
   const JOIN: u8 = 2;
   const END: u8 = 3;

   /// The bytes on the wire and how many of them count.
   #[inline]
   #[must_use]
   pub const fn encode(self) -> ([u8; 9], usize) {
      let mut bytes = [0_u8; 9];
      match self {
         Self::Open => {
            bytes[0] = Self::OPEN;
            (bytes, 1)
         },
         Self::Join(token) => {
            bytes[0] = Self::JOIN;
            let little = token.to_le_bytes();
            let mut at = 0;
            while at < 8 {
               bytes[at + 1] = little[at];
               at += 1;
            }
            (bytes, 9)
         },
         Self::End => {
            bytes[0] = Self::END;
            (bytes, 1)
         },
      }
   }

   #[inline]
   pub fn read<R>(from: &mut R) -> io::Result<Self>
   where
      R: Read,
   {
      match byte(from)? {
         Self::OPEN => Ok(Self::Open),
         Self::JOIN => Ok(Self::Join(u64::from_le_bytes(array(from)?))),
         Self::END => Ok(Self::End),
         other => Err(invalid(format!("unknown request {other}"))),
      }
   }
}

/// A daemon's message.
#[derive(Debug)]
pub enum Reply {
   Opened(u64),
   Joined,
   Record(Record),
   /// The session is over, `lost` accesses short of everything it made.
   Done {
      lost: Lost,
   },
}

/// Records that never reached a session's client, by where they were dropped.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Lost {
   /// Every scratch slot on the CPU was busy.
   pub slots:    u64,
   /// The kernel ring was full.
   pub ring:     u64,
   /// The record outgrew its scratch buffer.
   pub oversize: u64,
   /// The traced map had no room for a forked child.
   pub forks:    u64,
   /// The client fell too far behind the daemon.
   pub backlog:  u64,
}

impl Lost {
   const FIELDS: usize = 5;

   #[inline]
   #[must_use]
   pub const fn total(&self) -> u64 {
      self.slots + self.ring + self.oversize + self.forks + self.backlog
   }

   const fn fields(&self) -> [u64; Self::FIELDS] {
      [
         self.slots,
         self.ring,
         self.oversize,
         self.forks,
         self.backlog,
      ]
   }
}

impl Display for Lost {
   #[inline]
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      write!(
         f,
         "{} lost, slots {} ring {} oversize {} forks {} backlog {}",
         self.total(),
         self.slots,
         self.ring,
         self.oversize,
         self.forks,
         self.backlog
      )
   }
}

impl Reply {
   const OPENED: u8 = 1;
   const JOINED: u8 = 2;
   const RECORD: u8 = 3;
   const DONE: u8 = 4;

   #[inline]
   #[must_use]
   pub fn opened(token: u64) -> Vec<u8> {
      let mut bytes = vec![Self::OPENED];
      bytes.extend_from_slice(&token.to_le_bytes());
      bytes
   }

   #[inline]
   #[must_use]
   pub const fn joined() -> [u8; 1] {
      [Self::JOINED]
   }

   #[inline]
   #[must_use]
   pub fn done(lost: Lost) -> Vec<u8> {
      let mut bytes = vec![Self::DONE];
      for count in lost.fields() {
         bytes.extend_from_slice(&count.to_le_bytes());
      }
      bytes
   }

   /// A record as the kernel wrote it, framed.
   #[inline]
   pub fn record(raw: &[u8]) -> io::Result<Vec<u8>> {
      let length = u32::try_from(raw.len()).map_err(invalid)?;
      let mut bytes = vec![Self::RECORD];
      bytes.extend_from_slice(&length.to_le_bytes());
      bytes.extend_from_slice(raw);
      Ok(bytes)
   }

   #[inline]
   pub fn read<R>(from: &mut R) -> io::Result<Self>
   where
      R: Read,
   {
      match byte(from)? {
         Self::OPENED => Ok(Self::Opened(u64::from_le_bytes(array(from)?))),
         Self::JOINED => Ok(Self::Joined),
         Self::RECORD => {
            let length = u32::from_le_bytes(array(from)?);
            let mut raw = vec![0_u8; usize::try_from(length).map_err(invalid)?];
            from.read_exact(&mut raw)?;
            Ok(Self::Record(Record::try_from(raw.as_slice())?))
         },
         Self::DONE => {
            let mut fields = [0_u64; Lost::FIELDS];
            for field in &mut fields {
               *field = u64::from_le_bytes(array(from)?);
            }
            let [slots, ring, oversize, forks, backlog] = fields;
            Ok(Self::Done {
               lost: Lost {
                  slots,
                  ring,
                  oversize,
                  forks,
                  backlog,
               },
            })
         },
         other => Err(invalid(format!("unknown reply {other}"))),
      }
   }
}

/// A path as a call spelled it, and the directory a relative one resolves
/// against, which is empty for an absolute one.
#[derive(Debug, Default)]
pub struct Named {
   pub base: Vec<u8>,
   pub name: Vec<u8>,
}

/// Calls that change what paths mean or reach files by handle, so a session
/// that makes one may not have seen everything it read.
pub const ESCAPES: &[(libc::c_long, &str)] = &[
   (libc::SYS_mount, "mount"),
   (libc::SYS_umount2, "umount2"),
   (libc::SYS_open_tree, "open_tree"),
   (libc::SYS_move_mount, "move_mount"),
   (libc::SYS_fsopen, "fsopen"),
   (libc::SYS_fsmount, "fsmount"),
   (libc::SYS_fspick, "fspick"),
   (libc::SYS_mount_setattr, "mount_setattr"),
   (libc::SYS_chroot, "chroot"),
   (libc::SYS_pivot_root, "pivot_root"),
   (libc::SYS_unshare, "unshare"),
   (libc::SYS_setns, "setns"),
   (libc::SYS_open_by_handle_at, "open_by_handle_at"),
];

/// Why a session may not have seen everything its processes read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Escape {
   /// A system call that changes what paths mean or reaches files by handle.
   Call(u32),
   /// A call through a compat ABI.
   Foreign,
   /// A path the kernel half could not spell out.
   Unresolved,
}

impl From<u32> for Escape {
   #[inline]
   fn from(detail: u32) -> Self {
      match detail {
         0xFFFF_0002 => Self::Foreign,
         0xFFFF_0003 => Self::Unresolved,
         number => Self::Call(number),
      }
   }
}

impl Escape {
   #[inline]
   #[must_use]
   pub fn reason(self) -> &'static str {
      match self {
         Self::Call(number) => {
            ESCAPES
               .iter()
               .find(|entry| i64::from(number) == entry.0)
               .map_or("an escaping call", |entry| entry.1)
         },
         Self::Foreign => "a call through a compat ABI",
         Self::Unresolved => "a path the kernel could not spell out",
      }
   }
}

/// What one process did.
#[derive(Debug)]
pub enum Access {
   /// Opened a path with these open flags.
   Open {
      named: Named,
      flags: u32,
   },
   /// Looked a path up or changed it without reading it.
   Lookup(Named),
   /// Took away what a path named.
   Clobber(Named),
   /// Listed the directory at this resolved path.
   Listing(Vec<u8>),
   /// Started a program with these NUL-separated arguments, in the working
   /// directory that is the name's base. Arguments too long for a record
   /// come as the last of them that fit whole.
   Exec {
      named: Named,
      args:  Vec<u8>,
   },
   /// Started a child process.
   Fork {
      child: u32,
   },
   Escape(Escape),
   /// Reached a path too deep to spell out, under the directory named by
   /// its first components from the root.
   Deep(Vec<u8>),
}

/// An access by the process `tgid`, an id from outside any sandbox that only
/// tells the session's processes apart.
#[derive(Debug)]
pub struct Record {
   pub tgid:   u32,
   pub access: Access,
}

/// `struct record` in trace.bpf.c, padded out to its `u64`'s alignment.
const HEADER: usize = 32;

/// `EXEC_TAIL` in trace.bpf.c.
const EXEC_TAIL: u32 = 1;

impl TryFrom<&[u8]> for Record {
   type Error = io::Error;

   #[inline]
   fn try_from(raw: &[u8]) -> io::Result<Self> {
      let header = raw
         .get(..HEADER)
         .ok_or_else(|| invalid("a record shorter than its header"))?;
      let word = |at: usize| {
         u32::from_ne_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
      };
      let half = |at: usize| usize::from(u16::from_ne_bytes([header[at], header[at + 1]]));
      let kind = word(8);
      let tgid = word(12);
      let detail = word(16);
      let (base_end, name_end) = (half(20), half(20) + half(22));
      let data = &raw[HEADER..];
      let field = |range: Range<usize>| {
         data
            .get(range)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| invalid("a record shorter than its fields"))
      };
      let named = || -> io::Result<Named> {
         Ok(Named {
            base: field(0..base_end)?,
            name: field(base_end..name_end)?,
         })
      };
      let access = match kind {
         1 => {
            Access::Open {
               named: named()?,
               flags: detail,
            }
         },
         2 => Access::Lookup(named()?),
         3 => Access::Clobber(named()?),
         4 => Access::Listing(named()?.name),
         5 => {
            let mut args = field(name_end..name_end + half(24))?;
            if detail == EXEC_TAIL {
               let whole = args
                  .iter()
                  .position(|byte| *byte == 0)
                  .map_or(args.len(), |at| at + 1);
               args.drain(..whole);
            }
            Access::Exec {
               named: named()?,
               args,
            }
         },
         6 => Access::Fork { child: detail },
         7 => Access::Escape(Escape::from(detail)),
         8 => Access::Deep(named()?.name),
         other => return Err(invalid(format!("unknown record kind {other}"))),
      };
      Ok(Self { tgid, access })
   }
}

/// The session token at the head of a raw record.
#[inline]
#[must_use]
pub fn session(raw: &[u8]) -> Option<u64> {
   Some(u64::from_ne_bytes(raw.get(..8)?.try_into().ok()?))
}

/// Joins the session `token` names as the calling process and waits until the
/// daemon traces it. Runs between fork and exec, so it allocates nothing.
#[inline]
pub fn join(address: &SocketAddrUnix, token: u64) -> io::Result<()> {
   let socket: OwnedFd = socket_with(
      AddressFamily::UNIX,
      SocketType::STREAM,
      SocketFlags::CLOEXEC,
      None,
   )?;
   connect(&socket, address)?;
   let credentials = UCred {
      pid: getpid(),
      uid: getuid(),
      gid: getgid(),
   };
   let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmCredentials(1))];
   let mut control = SendAncillaryBuffer::new(&mut space);
   if !control.push(SendAncillaryMessage::ScmCredentials(credentials)) {
      return Err(io::Error::from(ErrorKind::OutOfMemory));
   }
   let (bytes, length) = Request::Join(token).encode();
   sendmsg(
      &socket,
      &[IoSlice::new(&bytes[..length])],
      &mut control,
      SendFlags::empty(),
   )?;
   let mut reply = [0_u8; 1];
   let (got, _) = recv(&socket, &mut reply, RecvFlags::empty())?;
   if got != 1 || reply != Reply::joined() {
      return Err(io::Error::from(ErrorKind::ConnectionRefused));
   }
   Ok(())
}

fn byte<R>(from: &mut R) -> io::Result<u8>
where
   R: Read,
{
   Ok(array::<1, R>(from)?[0])
}

fn array<const N: usize, R>(from: &mut R) -> io::Result<[u8; N]>
where
   R: Read,
{
   let mut bytes = [0_u8; N];
   from.read_exact(&mut bytes)?;
   Ok(bytes)
}

fn invalid(error: impl Into<Box<dyn Error + Send + Sync>>) -> io::Error {
   io::Error::new(ErrorKind::InvalidData, error)
}
