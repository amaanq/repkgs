//! A command run under a seccomp filter that reports every path it and its
//! descendants open, execute, stat or list to a thread here, which notes the
//! path and lets the call through unchanged.

use std::{
   collections::{
      BTreeMap,
      BTreeSet,
      HashMap,
      hash_map::Entry,
   },
   ffi::{
      OsStr,
      OsString,
   },
   fs::{
      File,
      Metadata,
      read,
      read_link,
      read_to_string,
      symlink_metadata,
   },
   io::{
      self,
      IoSlice,
      IoSliceMut,
   },
   mem::MaybeUninit,
   os::{
      fd::{
         AsFd as _,
         AsRawFd as _,
         BorrowedFd,
         FromRawFd as _,
         OwnedFd,
         RawFd,
      },
      unix::{
         ffi::OsStrExt as _,
         fs::{
            FileExt as _,
            MetadataExt as _,
         },
         process::CommandExt as _,
      },
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
      Arc,
      Mutex,
   },
   thread::{
      sleep,
      spawn,
   },
   time::Duration,
};

use libc::{
   AT_FDCWD,
   BPF_ABS,
   BPF_JEQ,
   BPF_JGE,
   BPF_JMP,
   BPF_K,
   BPF_LD,
   BPF_RET,
   BPF_W,
   SECCOMP_FILTER_FLAG_NEW_LISTENER,
   SECCOMP_FILTER_FLAG_WAIT_KILLABLE_RECV,
   SECCOMP_IOCTL_NOTIF_RECV,
   SECCOMP_IOCTL_NOTIF_SEND,
   SECCOMP_RET_ALLOW,
   SECCOMP_RET_USER_NOTIF,
   SECCOMP_SET_MODE_FILTER,
   SECCOMP_USER_NOTIF_FLAG_CONTINUE,
   seccomp_notif,
   seccomp_notif_resp,
   sock_filter,
   sock_fprog,
};
use memchr::memchr;
use mummer::store::normalize;
use rustix::{
   event::{
      PollFd,
      PollFlags,
      poll,
   },
   io::Errno,
   net::{
      AddressFamily,
      RecvAncillaryBuffer,
      RecvAncillaryMessage,
      RecvFlags,
      SendAncillaryBuffer,
      SendAncillaryMessage,
      SendFlags,
      SocketFlags,
      SocketType,
      recvmsg,
      sendmsg,
      socketpair,
   },
   thread::set_no_new_privs,
   time::{
      ClockId,
      Timespec,
      clock_gettime,
   },
};

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7;

/// x86-64's x32 calls carry this bit in their number.
#[cfg(target_arch = "x86_64")]
const X32_BIT: u32 = 0x4000_0000;
#[cfg(target_arch = "aarch64")]
const X32_BIT: u32 = 0;

/// Within a page however large the pages are, so a read never crosses into
/// one the tracee has not mapped.
const CHUNK: u64 = 4096;
const PATH_MAX: usize = 4096;

/// A path argument, a relative one under the directory descriptor in
/// argument `dir` when the call takes one.
#[derive(Clone, Copy)]
struct Arg {
   path: usize,
   dir:  Option<usize>,
}

const FIRST: &[Arg] = &[Arg {
   path: 0,
   dir:  None,
}];

const SECOND: &[Arg] = &[Arg {
   path: 1,
   dir:  None,
}];

const AT: &[Arg] = &[Arg {
   path: 1,
   dir:  Some(0),
}];

const BOTH: &[Arg] = &[
   Arg {
      path: 0,
      dir:  None,
   },
   Arg {
      path: 1,
      dir:  None,
   },
];

const BOTH_AT: &[Arg] = &[
   Arg {
      path: 1,
      dir:  Some(0),
   },
   Arg {
      path: 3,
      dir:  Some(2),
   },
];

const LINK_AT: &[Arg] = &[Arg {
   path: 2,
   dir:  Some(1),
}];

/// Numbered alike on every architecture, and missing from libc for aarch64.
const SYS_FCHMODAT2: libc::c_long = 452;

/// `open_how.resolve`'s flag that makes an absolute path relative to the
/// directory descriptor.
const RESOLVE_IN_ROOT: u64 = 0x10;

#[derive(Clone, Copy)]
enum Call {
   /// Looks paths up, to read them or to change them, which also tells the
   /// caller whether they exist.
   Paths(&'static [Arg]),
   /// Opens a path, for writing when the flags in argument `flags` ask to,
   /// and always for creat, which has none.
   Open { arg: Arg, flags: Option<usize> },
   /// openat2, whose `open_how` may root the lookup at the descriptor.
   Open2,
   /// Loads a program.
   Exec(Arg),
   /// Takes away what the paths named before the call, unlinking,
   /// truncating or renaming over them.
   Clobbers(&'static [Arg]),
   /// A directory listing of the descriptor in argument `fd`.
   Listing { fd: usize },
   /// `rt_sigaction`, which may install a handler that does not restart calls.
   Handler,
   /// A call that reaches files the filter cannot see, or sees them through
   /// a different tree.
   Escape(&'static str),
}

const COMMON: &[(libc::c_long, Call)] = &[
   (libc::SYS_openat, Call::Open {
      arg:   AT[0],
      flags: Some(2),
   }),
   (libc::SYS_openat2, Call::Open2),
   (libc::SYS_execve, Call::Exec(FIRST[0])),
   (libc::SYS_execveat, Call::Exec(AT[0])),
   (libc::SYS_newfstatat, Call::Paths(AT)),
   (libc::SYS_statx, Call::Paths(AT)),
   (libc::SYS_faccessat, Call::Paths(AT)),
   (libc::SYS_faccessat2, Call::Paths(AT)),
   (libc::SYS_readlinkat, Call::Paths(AT)),
   (libc::SYS_name_to_handle_at, Call::Paths(AT)),
   (libc::SYS_chdir, Call::Paths(FIRST)),
   (libc::SYS_statfs, Call::Paths(FIRST)),
   (libc::SYS_truncate, Call::Clobbers(FIRST)),
   (libc::SYS_getxattr, Call::Paths(FIRST)),
   (libc::SYS_lgetxattr, Call::Paths(FIRST)),
   (libc::SYS_listxattr, Call::Paths(FIRST)),
   (libc::SYS_llistxattr, Call::Paths(FIRST)),
   (libc::SYS_setxattr, Call::Paths(FIRST)),
   (libc::SYS_lsetxattr, Call::Paths(FIRST)),
   (libc::SYS_removexattr, Call::Paths(FIRST)),
   (libc::SYS_lremovexattr, Call::Paths(FIRST)),
   (libc::SYS_inotify_add_watch, Call::Paths(SECOND)),
   (libc::SYS_mkdirat, Call::Paths(AT)),
   (libc::SYS_mknodat, Call::Paths(AT)),
   (libc::SYS_unlinkat, Call::Clobbers(AT)),
   (libc::SYS_renameat, Call::Clobbers(BOTH_AT)),
   (libc::SYS_renameat2, Call::Clobbers(BOTH_AT)),
   (libc::SYS_linkat, Call::Paths(BOTH_AT)),
   (libc::SYS_symlinkat, Call::Paths(LINK_AT)),
   (libc::SYS_fchmodat, Call::Paths(AT)),
   (SYS_FCHMODAT2, Call::Paths(AT)),
   (libc::SYS_fchownat, Call::Paths(AT)),
   (libc::SYS_utimensat, Call::Paths(AT)),
   (libc::SYS_getdents64, Call::Listing { fd: 0 }),
   (libc::SYS_rt_sigaction, Call::Handler),
   (libc::SYS_io_uring_setup, Call::Escape("io_uring")),
   (
      libc::SYS_open_by_handle_at,
      Call::Escape("open_by_handle_at"),
   ),
   (libc::SYS_mount, Call::Escape("mount")),
   (libc::SYS_umount2, Call::Escape("umount2")),
   (libc::SYS_open_tree, Call::Escape("open_tree")),
   (libc::SYS_move_mount, Call::Escape("move_mount")),
   (libc::SYS_fsopen, Call::Escape("fsopen")),
   (libc::SYS_fsmount, Call::Escape("fsmount")),
   (libc::SYS_fspick, Call::Escape("fspick")),
   (libc::SYS_mount_setattr, Call::Escape("mount_setattr")),
   (libc::SYS_chroot, Call::Escape("chroot")),
   (libc::SYS_pivot_root, Call::Escape("pivot_root")),
   (libc::SYS_unshare, Call::Escape("unshare")),
   (libc::SYS_setns, Call::Escape("setns")),
];

#[cfg(target_arch = "x86_64")]
const LEGACY: &[(libc::c_long, Call)] = &[
   (libc::SYS_open, Call::Open {
      arg:   FIRST[0],
      flags: Some(1),
   }),
   (libc::SYS_creat, Call::Open {
      arg:   FIRST[0],
      flags: None,
   }),
   (libc::SYS_stat, Call::Paths(FIRST)),
   (libc::SYS_lstat, Call::Paths(FIRST)),
   (libc::SYS_access, Call::Paths(FIRST)),
   (libc::SYS_readlink, Call::Paths(FIRST)),
   (libc::SYS_mkdir, Call::Paths(FIRST)),
   (libc::SYS_rmdir, Call::Paths(FIRST)),
   (libc::SYS_unlink, Call::Clobbers(FIRST)),
   (libc::SYS_mknod, Call::Paths(FIRST)),
   (libc::SYS_rename, Call::Clobbers(BOTH)),
   (libc::SYS_link, Call::Paths(BOTH)),
   (libc::SYS_symlink, Call::Paths(SECOND)),
   (libc::SYS_chmod, Call::Paths(FIRST)),
   (libc::SYS_chown, Call::Paths(FIRST)),
   (libc::SYS_lchown, Call::Paths(FIRST)),
   (libc::SYS_utime, Call::Paths(FIRST)),
   (libc::SYS_utimes, Call::Paths(FIRST)),
   (libc::SYS_futimesat, Call::Paths(AT)),
   (libc::SYS_getdents, Call::Listing { fd: 0 }),
];
#[cfg(target_arch = "aarch64")]
const LEGACY: &[(libc::c_long, Call)] = &[];

fn calls() -> impl Iterator<Item = (u32, Call)> {
   COMMON
      .iter()
      .chain(LEGACY)
      .filter_map(|entry| Some((u32::try_from(entry.0).ok()?, entry.1)))
}

/// How a test step's processes divide into tests its harness can run alone,
/// for a build system that has such a harness.
pub trait Tests: Sync {
   /// The test a process running `exe` as `args` in `cwd` is, when it is
   /// one, and not merely run by one.
   fn unit(&self, exe: &Path, args: &[OsString], cwd: &Path) -> Option<String>;

   /// Whether a `test-prepare` phase builds the tests before the traced
   /// run, so that compiling them is no test's reading.
   fn prepares(&self) -> bool {
      false
   }

   /// Whether the harness keeps its own state at `path`, rewritten on every
   /// run and never something a test read.
   fn state(&self, _path: &Path) -> bool {
      false
   }
}

/// Who reached a path, the tests and whether anything outside a test did.
#[derive(Default)]
pub struct Readers {
   pub shared: bool,
   pub units:  BTreeSet<String>,
}

impl Readers {
   /// Reached by nothing but processes outside any test.
   pub const fn outside() -> Self {
      Self {
         shared: true,
         units:  BTreeSet::new(),
      }
   }

   fn add(&mut self, unit: Option<&str>) {
      match unit {
         Some(name) => {
            self.units.insert(name.to_owned());
         },
         None => self.shared = true,
      }
   }
}

/// What a traced run reached, the paths as the calls named them made
/// absolute.
#[derive(Default)]
pub struct Accesses {
   pub paths:         BTreeMap<PathBuf, Readers>,
   pub listed:        BTreeMap<PathBuf, Readers>,
   /// Paths only looked up outside any test, by a harness checking whether
   /// to rebuild what the build already made, which count for whether they
   /// exist and not what they hold, or every test would depend on every
   /// source.
   pub looked_up:     BTreeSet<PathBuf>,
   /// Files the build made, as they were when the run first took them away
   /// after reading them.
   pub prior:         BTreeMap<PathBuf, Vec<u8>>,
   /// Why the paths may not be everything the run read.
   pub escaped:       Option<&'static str>,
   /// Whether a task installed a handler without `SA_RESTART`, the only kind
   /// that makes a call waiting on the trace server fail with EINTR.
   pub interruptible: bool,
}

pub enum Outcome {
   Traced(ExitStatus, Accesses),
   /// Run without the filter, which could not be installed.
   Untraced(ExitStatus, io::Error),
}

/// A point on the clock file timestamps come from, later than every write
/// made before it was taken and no later than any made after.
#[derive(Clone, Copy)]
pub struct Mark(Timespec);

impl Mark {
   /// The coarse clock writes are stamped with lags the fine one by up to a
   /// tick, so the mark is the first coarse reading after the current one.
   pub fn now() -> Self {
      let start = clock_gettime(ClockId::RealtimeCoarse);
      loop {
         sleep(Duration::from_millis(1));
         let now = clock_gettime(ClockId::RealtimeCoarse);
         if (now.tv_sec, now.tv_nsec) > (start.tv_sec, start.tv_nsec) {
            return Self(now);
         }
      }
   }

   /// Whether the file's contents were written at or after the mark.
   pub fn written(&self, meta: &Metadata) -> bool {
      (meta.mtime(), meta.mtime_nsec()) >= (self.0.tv_sec, self.0.tv_nsec)
   }
}

fn statement(code: u32, value: u32) -> sock_filter {
   jump(code, value, 0, 0)
}

fn jump(code: u32, value: u32, taken: u8, skipped: u8) -> sock_filter {
   sock_filter {
      code: u16::try_from(code).unwrap_or_default(),
      jt:   taken,
      jf:   skipped,
      k:    value,
   }
}

/// Notifies on the calls above, any call of another architecture and, on
/// x86-64, any x32 call, and allows the rest.
fn program() -> io::Result<Vec<sock_filter>> {
   let numbers = calls().map(|entry| entry.0).collect::<Vec<u32>>();
   let count = u8::try_from(numbers.len()).map_err(io::Error::other)?;
   let mut program = vec![
      statement(BPF_LD | BPF_W | BPF_ABS, 4),
      jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH, 1, 0),
      statement(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF),
      statement(BPF_LD | BPF_W | BPF_ABS, 0),
   ];
   if X32_BIT != 0 {
      program.push(jump(BPF_JMP | BPF_JGE | BPF_K, X32_BIT, count + 1, 0));
   }
   for (index, number) in (0..count).zip(numbers) {
      program.push(jump(BPF_JMP | BPF_JEQ | BPF_K, number, count - index, 0));
   }
   program.push(statement(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
   program.push(statement(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF));
   Ok(program)
}

/// Runs between fork and exec, so it allocates nothing.
fn install(program: &[sock_filter], channel: RawFd) -> io::Result<()> {
   set_no_new_privs(true)?;
   let filter = sock_fprog {
      len:    u16::try_from(program.len()).map_err(|_overflow| Errno::INVAL)?,
      filter: program.as_ptr().cast_mut(),
   };
   // SAFETY: `filter` points at `program`, which outlives the call, and the
   // kernel copies it before returning
   let returned = unsafe {
      libc::syscall(
         libc::SYS_seccomp,
         SECCOMP_SET_MODE_FILTER,
         SECCOMP_FILTER_FLAG_NEW_LISTENER | SECCOMP_FILTER_FLAG_WAIT_KILLABLE_RECV,
         &raw const filter,
      )
   };
   if returned < 0 {
      return Err(io::Error::last_os_error());
   }
   let raw = RawFd::try_from(returned).map_err(|_overflow| Errno::BADF)?;
   // SAFETY: the kernel just returned this descriptor and nothing else owns it
   let listener = unsafe { OwnedFd::from_raw_fd(raw) };
   // SAFETY: the parent keeps its end of the pair open until spawn returns
   let socket = unsafe { BorrowedFd::borrow_raw(channel) };
   let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
   let mut control = SendAncillaryBuffer::new(&mut space);
   let passed = [listener.as_fd()];
   if !control.push(SendAncillaryMessage::ScmRights(&passed)) {
      return Err(Errno::NOBUFS.into());
   }
   sendmsg(
      socket,
      &[IoSlice::new(b"l")],
      &mut control,
      SendFlags::empty(),
   )?;
   Ok(())
}

fn receive(socket: &OwnedFd) -> Option<OwnedFd> {
   let mut byte = [0_u8; 1];
   let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
   let mut control = RecvAncillaryBuffer::new(&mut space);
   recvmsg(
      socket,
      &mut [IoSliceMut::new(&mut byte)],
      &mut control,
      RecvFlags::CMSG_CLOEXEC,
   )
   .ok()?;
   control.drain().find_map(|message| {
      if let RecvAncillaryMessage::ScmRights(mut fds) = message {
         fds.next()
      } else {
         None
      }
   })
}

#[derive(Default)]
struct Reader {
   memories: HashMap<u32, File>,
}

impl Reader {
   /// The NUL-terminated string at `address` in the task's memory, reopening
   /// it once, since an exec or a reused id leaves a stale handle.
   fn string(&mut self, task: u32, address: u64) -> Option<Vec<u8>> {
      if address == 0 {
         return None;
      }
      if let Some(found) = self.read(task, address) {
         return Some(found);
      }
      self.memories.remove(&task);
      self.read(task, address)
   }

   /// `buffer.len()` bytes at `address`, a small struct an argument points at.
   fn fill(&mut self, task: u32, address: u64, buffer: &mut [u8]) -> Option<()> {
      if address == 0 {
         return None;
      }
      for _attempt in 0..2_u8 {
         let memory = self.memory(task)?;
         if memory.read_at(buffer, address).ok() == Some(buffer.len()) {
            return Some(());
         }
         self.memories.remove(&task);
      }
      None
   }

   fn memory(&mut self, task: u32) -> Option<&File> {
      if self.memories.len() > 1024 {
         self.memories.clear();
      }
      Some(match self.memories.entry(task) {
         Entry::Occupied(found) => found.into_mut(),
         Entry::Vacant(slot) => slot.insert(File::open(format!("/proc/{task}/mem")).ok()?),
      })
   }

   fn read(&mut self, task: u32, address: u64) -> Option<Vec<u8>> {
      let memory = self.memory(task)?;
      let mut found = Vec::new();
      let mut at = address;
      let mut chunk = [0_u8; 4096];
      while found.len() < PATH_MAX {
         let room = usize::try_from(CHUNK - at % CHUNK).ok()?;
         let got = memory.read_at(&mut chunk[..room], at).ok()?;
         if got == 0 {
            return None;
         }
         if let Some(end) = memchr(0, &chunk[..got]) {
            found.extend_from_slice(&chunk[..end]);
            return Some(found);
         }
         found.extend_from_slice(&chunk[..got]);
         at += u64::try_from(got).ok()?;
      }
      None
   }
}

/// The low 32 bits of a register, where the kernel reads an `int` argument.
fn int(register: u64) -> Option<i32> {
   let low = u32::try_from(register & 0xFFFF_FFFF).ok()?;
   Some(i32::from_ne_bytes(low.to_ne_bytes()))
}

/// The directory a descriptor names, the working directory for none or for
/// `AT_FDCWD`.
fn descriptor(task: u32, register: Option<u64>) -> Option<PathBuf> {
   let fd = register.map_or(Some(AT_FDCWD), int)?;
   if fd == AT_FDCWD {
      return read_link(format!("/proc/{task}/cwd")).ok();
   }
   read_link(format!("/proc/{task}/fd/{fd}")).ok()
}

/// The path argument made absolute, against the directory descriptor or the
/// task's working directory.
fn resolve(reader: &mut Reader, request: &seccomp_notif, arg: Arg) -> Option<PathBuf> {
   let data = request.data;
   let raw = reader.string(request.pid, data.args[arg.path])?;
   if raw.is_empty() {
      return None;
   }
   let named = Path::new(OsStr::from_bytes(&raw));
   if named.is_absolute() {
      return Some(normalize(named));
   }
   let base = descriptor(request.pid, arg.dir.map(|index| data.args[index]))?;
   Some(normalize(&base.join(named)))
}

/// What one call did to a path.
enum Event {
   /// Read it, or ran it.
   Read(PathBuf),
   /// Looked it up or changed it without reading it.
   Looked(PathBuf),
   /// Took away what it named, after which reading the path reads something
   /// else.
   Clobbered(PathBuf),
   /// Listed the directory.
   Listed(PathBuf),
   /// Ran the program, which may itself be a test.
   Exec(PathBuf),
}

/// Whether open flags ask for writing or truncation.
fn writes(flags: u64) -> bool {
   let access = flags & u64::try_from(libc::O_ACCMODE).unwrap_or_default();
   access != u64::try_from(libc::O_RDONLY).unwrap_or_default()
      || flags & u64::try_from(libc::O_TRUNC).unwrap_or_default() != 0
}

/// What the call did, or nothing for one the run need not record. An escape
/// or a call from another ABI marks the run instead.
fn note(reader: &mut Reader, request: &seccomp_notif, accesses: &mut Accesses) -> Vec<Event> {
   let data = request.data;
   if data.arch != AUDIT_ARCH {
      accesses.escaped.get_or_insert("foreign-arch call");
      return Vec::new();
   }
   let number = u32::from_ne_bytes(data.nr.to_ne_bytes());
   if X32_BIT != 0 && number & X32_BIT != 0 {
      accesses.escaped.get_or_insert("x32 call");
      return Vec::new();
   }
   let Some(call) = calls().find(|entry| entry.0 == number).map(|entry| entry.1) else {
      return Vec::new();
   };
   let mut events = Vec::new();
   match call {
      Call::Paths(args) => {
         events.extend(
            args
               .iter()
               .filter_map(|arg| resolve(reader, request, *arg))
               .map(Event::Looked),
         );
      },
      Call::Open { arg, flags } => {
         if let Some(found) = resolve(reader, request, arg) {
            if flags.is_none_or(|index| writes(data.args[index])) {
               events.push(Event::Clobbered(found.clone()));
            }
            events.push(Event::Read(found));
         }
      },
      Call::Open2 => {
         let mut how = [0_u8; 24];
         let parsed = reader.fill(request.pid, data.args[2], &mut how).map(|()| {
            let flags = u64::from_ne_bytes(how[..8].try_into().unwrap_or_default());
            let resolve = u64::from_ne_bytes(how[16..].try_into().unwrap_or_default());
            (flags, resolve & RESOLVE_IN_ROOT != 0)
         });
         let Some((flags, false)) = parsed else {
            accesses
               .escaped
               .get_or_insert("openat2 rooted at a descriptor");
            return Vec::new();
         };
         if let Some(found) = resolve(reader, request, AT[0]) {
            if writes(flags) {
               events.push(Event::Clobbered(found.clone()));
            }
            events.push(Event::Read(found));
         }
      },
      Call::Exec(arg) => {
         events.extend(resolve(reader, request, arg).map(Event::Exec));
      },
      Call::Clobbers(args) => {
         events.extend(
            args
               .iter()
               .filter_map(|arg| resolve(reader, request, *arg))
               .map(Event::Clobbered),
         );
      },
      Call::Listing { fd } => {
         events.extend(descriptor(request.pid, Some(data.args[fd])).map(Event::Listed));
      },
      Call::Handler => {
         // the kernel's sigaction opens with the handler then the flags on
         // every architecture, whatever follows them
         let mut action = [0_u8; 16];
         if reader
            .fill(request.pid, data.args[1], &mut action)
            .is_none()
         {
            accesses.interruptible |= data.args[1] != 0;
            return events;
         }
         let handler = u64::from_ne_bytes(action[..8].try_into().unwrap_or_default());
         let flags = u64::from_ne_bytes(action[8..].try_into().unwrap_or_default());
         let default = [libc::SIG_DFL, libc::SIG_IGN]
            .into_iter()
            .any(|value| u64::try_from(value).ok() == Some(handler));
         let restart = u64::try_from(libc::SA_RESTART).unwrap_or_default();
         accesses.interruptible |= !default && flags & restart == 0;
      },
      Call::Escape(why) => {
         accesses.escaped.get_or_insert(why);
      },
   }
   events
}

/// The fields of `/proc/<task>/stat` a lineage needs.
struct Stat {
   parent: u32,
   start:  u64,
}

impl FromStr for Stat {
   type Err = String;

   /// The command name in parentheses may hold anything, so the fields are
   /// counted from the last parenthesis.
   fn from_str(line: &str) -> Result<Self, Self::Err> {
      let fields = line
         .rsplit_once(')')
         .ok_or_else(|| format!("{line} has no command name"))?
         .1
         .split_whitespace()
         .collect::<Vec<_>>();
      let field = |at: usize| {
         fields
            .get(at)
            .ok_or_else(|| format!("{line} has no field {at}"))
      };
      Ok(Self {
         parent: field(1)?
            .parse()
            .map_err(|error| format!("{line}: {error}"))?,
         start:  field(19)?
            .parse()
            .map_err(|error| format!("{line}: {error}"))?,
      })
   }
}

/// The test each traced task belongs to, its own or the nearest ancestor's,
/// remembered per task and start time since ids are reused.
#[derive(Default)]
struct Lineage {
   known: HashMap<(u32, u64), Option<String>>,
}

impl Lineage {
   fn own(tests: &dyn Tests, task: u32) -> Option<String> {
      let exe = read_link(format!("/proc/{task}/exe")).ok()?;
      let cwd = read_link(format!("/proc/{task}/cwd")).ok()?;
      let line = read(format!("/proc/{task}/cmdline")).ok()?;
      let args = line
         .split(|byte| *byte == 0)
         .filter(|arg| !arg.is_empty())
         .map(|arg| OsStr::from_bytes(arg).to_owned())
         .collect::<Vec<_>>();
      tests.unit(&exe, &args, &cwd)
   }

   fn unit(&mut self, tests: &dyn Tests, task: u32) -> Option<String> {
      let Stat { parent, start } = read_to_string(format!("/proc/{task}/stat"))
         .ok()?
         .parse()
         .ok()?;
      if let Some(found) = self.known.get(&(task, start)) {
         return found.clone();
      }
      let found = Self::own(tests, task)
         .or_else(|| (parent > 1).then(|| self.unit(tests, parent)).flatten());
      self.known.insert((task, start), found.clone());
      found
   }
}

/// Records what a call did as `unit` doing it, keeping a file the build made
/// as it was before the call takes it away, when the run read it already.
fn record(accesses: &mut Accesses, event: Event, unit: Option<&str>, traced: bool, mark: Mark) {
   match event {
      Event::Looked(path) if unit.is_none() && traced => {
         accesses.looked_up.insert(path);
      },
      Event::Read(path) | Event::Exec(path) | Event::Looked(path) => {
         accesses.paths.entry(path).or_default().add(unit);
      },
      Event::Clobbered(path) => {
         let made =
            symlink_metadata(&path).is_ok_and(|meta| meta.is_file() && !mark.written(&meta));
         if made
            && accesses.paths.contains_key(&path)
            && !accesses.prior.contains_key(&path)
            && let Ok(bytes) = read(&path)
         {
            accesses.prior.insert(path.clone(), bytes);
         }
         record(accesses, Event::Looked(path), unit, traced, mark);
      },
      Event::Listed(path) => accesses.listed.entry(path).or_default().add(unit),
   }
}

/// Notes each call while the run is recorded, then keeps letting calls
/// through for whatever it left running, until no traced task remains.
fn serve(
   socket: &OwnedFd,
   recorded: &Mutex<Option<Accesses>>,
   tests: Option<&dyn Tests>,
   mark: Mark,
) {
   let Some(listener) = receive(socket) else {
      return;
   };
   let mut reader = Reader::default();
   let mut lineage = Lineage::default();
   loop {
      let mut polled = [PollFd::new(&listener, PollFlags::IN)];
      match poll(&mut polled, None) {
         Ok(_) => {},
         Err(Errno::INTR) => continue,
         Err(_) => return,
      }
      let events = polled[0].revents();
      if !events.contains(PollFlags::IN) {
         if events.intersects(PollFlags::HUP | PollFlags::ERR) {
            return;
         }
         continue;
      }
      let mut empty = MaybeUninit::<seccomp_notif>::zeroed();
      // SAFETY: RECV writes a `seccomp_notif` into the zeroed one it is given
      let received = unsafe {
         libc::ioctl(
            listener.as_raw_fd(),
            SECCOMP_IOCTL_NOTIF_RECV,
            empty.as_mut_ptr(),
         )
      };
      if received < 0_i32 {
         // a task killed while waiting leaves nothing to receive
         continue;
      }
      // SAFETY: RECV succeeded, so it filled the whole struct
      let request = unsafe { empty.assume_init() };
      if let Ok(mut guard) = recorded.lock()
         && let Some(accesses) = guard.as_mut()
      {
         let done = note(&mut reader, &request, accesses);
         let unit = match tests {
            Some(harness) if !done.is_empty() => lineage.unit(harness, request.pid),
            _ => None,
         };
         for event in done {
            let exec = match event {
               Event::Exec(ref path) => {
                  tests.and_then(|harness| harness.unit(path, &[], Path::new("/")))
               },
               Event::Read(_) | Event::Looked(_) | Event::Clobbered(_) | Event::Listed(_) => None,
            };
            let ran = exec.or_else(|| unit.clone());
            record(accesses, event, ran.as_deref(), tests.is_some(), mark);
         }
      }
      let mut response = seccomp_notif_resp {
         id:    request.id,
         val:   0,
         error: 0,
         flags: u32::try_from(SECCOMP_USER_NOTIF_FLAG_CONTINUE).unwrap_or_default(),
      };
      // SAFETY: SEND reads the `seccomp_notif_resp` it is given; a task that
      // died meanwhile fails it with ENOENT, which leaves nothing to do
      unsafe {
         libc::ioctl(
            listener.as_raw_fd(),
            SECCOMP_IOCTL_NOTIF_SEND,
            &raw mut response,
         );
      }
   }
}

/// Runs a command from `make` traced, or untraced when the filter cannot be
/// installed. A failed spawn is retried untraced, so an exec failure comes
/// back as the untraced spawn's error.
pub fn run(
   make: &dyn Fn() -> io::Result<Command>,
   tests: Option<&'static dyn Tests>,
   mark: Mark,
) -> io::Result<Outcome> {
   let traced = program().and_then(|filter| {
      let (ours, theirs) = socketpair(
         AddressFamily::UNIX,
         SocketType::STREAM,
         SocketFlags::CLOEXEC,
         None,
      )?;
      Ok((filter, ours, theirs))
   });
   let (filter, ours, theirs) = match traced {
      Ok(parts) => parts,
      Err(error) => return Ok(Outcome::Untraced(make()?.status()?, error)),
   };
   let recorded = Arc::new(Mutex::new(Some(Accesses::default())));
   let server = Arc::clone(&recorded);
   spawn(move || serve(&ours, &server, tests, mark));
   let raw = theirs.as_raw_fd();
   let mut command = make()?;
   // SAFETY: `install` makes only raw system calls and allocates nothing
   let spawned = unsafe { command.pre_exec(move || install(&filter, raw)) }.spawn();
   drop(theirs);
   match spawned {
      Ok(mut child) => {
         let status = child.wait()?;
         let accesses = recorded
            .lock()
            .map_err(|_poisoned| io::Error::other("the trace server panicked"))?
            .take()
            .unwrap_or_default();
         Ok(Outcome::Traced(status, accesses))
      },
      Err(error) => Ok(Outcome::Untraced(make()?.status()?, error)),
   }
}
