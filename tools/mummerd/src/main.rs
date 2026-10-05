mod identity;
mod remote;
mod server;
mod slots;
mod store;

#[cfg(target_os = "linux")] use std::fs::read_to_string;
use std::{
   env::var_os,
   fmt::Arguments,
   fs::{
      Permissions,
      create_dir_all,
      remove_file,
      set_permissions,
      symlink_metadata,
   },
   io::{
      self,
      Write as _,
   },
   net::Shutdown,
   os::unix::{
      fs::{
         FileTypeExt as _,
         MetadataExt as _,
         PermissionsExt as _,
      },
      net::{
         UnixListener,
         UnixStream,
      },
   },
   path::PathBuf,
   sync::{
      Arc,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
   thread::{
      Builder,
      JoinHandle,
      available_parallelism,
      sleep,
   },
   time::{
      Duration,
      SystemTime,
      UNIX_EPOCH,
   },
};

use misstep::{
   Result,
   ResultExt as _,
   bail,
   report,
};
use pound::Parse;
use rustix::{
   event::{
      PollFd,
      PollFlags,
      Timespec,
      poll,
   },
   io::Errno,
   process::{
      Resource,
      getrlimit,
      setrlimit,
   },
};
#[cfg(target_os = "linux")]
use rustix::{
   net::sockopt::socket_peercred,
   process::getuid,
};
use signal_hook::{
   consts::{
      SIGINT,
      SIGTERM,
   },
   flag::register,
};

use crate::{
   remote::cache::Remote,
   server::{
      Connection,
      Daemon,
   },
   store::{
      Store,
      Tier,
   },
};

/// Compile cache and compiler slot daemon behind mummer. Packs live under
/// `XDG_CACHE_HOME/mummerd/packs`, default `$HOME/.cache`.
#[derive(Parse)]
#[pound(name = "mummerd")]
struct Options {
   /// The Unix socket to listen on.
   socket:          PathBuf,
   /// Cache budget in GiB, zero means unlimited.
   #[pound(long, env = "MUMMERD_SIZE", default = "100")]
   size:            u64,
   /// A second, slower pack directory that takes what the budget pushes out.
   #[pound(long, env = "MUMMERD_COLD")]
   cold:            Option<PathBuf>,
   /// Budget of the cold directory in GiB, zero means unlimited.
   #[pound(long, env = "MUMMERD_COLD_SIZE", default = "0")]
   cold_size:       u64,
   /// Concurrent compiler slots, default the number of CPUs.
   #[pound(long, env = "MUMMERD_SLOTS")]
   slots:           Option<usize>,
   /// A REAPI cache asked on a local miss, `grpc://host:port/instance` or
   /// `grpcs://` for TLS, such as `NativeLink`.
   #[pound(long, env = "MUMMERD_REMOTE")]
   remote:          Option<String>,
   /// The CA a `grpcs://` remote's certificate must chain to, else the web's.
   #[pound(long, env = "MUMMERD_REMOTE_CA")]
   remote_ca:       Option<PathBuf>,
   /// A PEM file with this machine's client certificate chain and key.
   #[pound(long, env = "MUMMERD_REMOTE_IDENTITY")]
   remote_identity: Option<PathBuf>,
   /// Also write to the remote, which only a trusted builder should.
   #[pound(long)]
   remote_writes:   bool,
}

struct Socket {
   listener: UnixListener,
   path:     PathBuf,
   device:   u64,
   inode:    u64,
}

impl Socket {
   fn bind(path: PathBuf) -> io::Result<Self> {
      if let Some(parent) = path
         .parent()
         .filter(|parent| !parent.as_os_str().is_empty())
      {
         create_dir_all(parent)?;
      }
      match symlink_metadata(&path) {
         Ok(metadata) => {
            if !metadata.file_type().is_socket() {
               return Err(io::Error::other("socket path is occupied by another file"));
            }
            match UnixStream::connect(&path) {
               Ok(_) => {
                  return Err(io::Error::new(
                     io::ErrorKind::AddrInUse,
                     "daemon already listening",
                  ));
               },
               Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                  remove_file(&path)?;
               },
               Err(error) => return Err(error),
            }
         },
         Err(error) if error.kind() == io::ErrorKind::NotFound => {},
         Err(error) => return Err(error),
      }
      let listener = UnixListener::bind(&path)?;
      let metadata = symlink_metadata(&path)?;
      let socket = Self {
         listener,
         path,
         device: metadata.dev(),
         inode: metadata.ino(),
      };
      set_permissions(&socket.path, Permissions::from_mode(0o666))?;
      socket.listener.set_nonblocking(true)?;
      Ok(socket)
   }
}

impl Drop for Socket {
   fn drop(&mut self) {
      if let Ok(metadata) = symlink_metadata(&self.path)
         && metadata.dev() == self.device
         && metadata.ino() == self.inode
         && let Err(error) = remove_file(&self.path)
      {
         warn(format_args!(
            "failed to remove socket {} with {error}",
            self.path.display()
         ));
      }
   }
}

fn main() -> Result<()> {
   let options = Options::parse();
   let cache = match var_os("XDG_CACHE_HOME") {
      Some(path) if !path.is_empty() => PathBuf::from(path),
      Some(_) | None => {
         PathBuf::from(var_os("HOME").ok_or_else(|| report!("HOME is not set"))?).join(".cache")
      },
   };
   let mut tiers = vec![Tier {
      directory: cache.join("mummerd/packs"),
      budget:    gibibytes(options.size, "--size")?,
   }];
   if let Some(cold) = options
      .cold
      .filter(|directory| !directory.as_os_str().is_empty())
   {
      tiers.push(Tier {
         directory: cold,
         budget:    gibibytes(options.cold_size, "--cold-size")?,
      });
   }
   let limit = match options.slots {
      Some(slots) => slots,
      None => available_parallelism()?.get(),
   };
   if limit == 0 {
      bail!("--slots must be positive");
   }

   // every pack and hint stays open beside one descriptor per client, which
   // outgrows the usual soft limit of 1024 long before the hard one
   let mut files = getrlimit(Resource::Nofile);
   files.current = files.maximum;
   setrlimit(Resource::Nofile, files).context("could not raise the open file limit")?;

   let store = Store::open(tiers).context("could not open cache")?;
   let socket = Socket::bind(options.socket).context("could not bind socket")?;
   let stopped = Arc::new(AtomicBool::new(false));
   register(SIGINT, Arc::clone(&stopped))?;
   register(SIGTERM, Arc::clone(&stopped))?;
   note(format_args!(
      "listening on {}, {limit} slots, {}",
      socket.path.display(),
      store.stats()
   ));
   let store_dir = var_os("NIX_STORE_DIR")
      .filter(|path| !path.is_empty())
      .map_or_else(|| PathBuf::from("/nix/store"), PathBuf::from);
   let remote = options
      .remote
      .as_deref()
      .filter(|url| !url.is_empty())
      .map(|url| {
         Remote::new(
            url,
            options
               .remote_ca
               .as_deref()
               .filter(|path| !path.as_os_str().is_empty()),
            options
               .remote_identity
               .as_deref()
               .filter(|path| !path.as_os_str().is_empty()),
            options.remote_writes,
         )
      })
      .transpose()
      .context("could not set up the remote cache")?;
   if let Some(ref tier) = remote {
      note(format_args!(
         "remote tier {}, {}",
         tier.url(),
         if tier.writes() {
            "writing"
         } else {
            "read-only"
         }
      ));
   }
   let daemon = Arc::new(Daemon::new(store, limit, &store_dir, remote)?);
   let mut workers = Vec::<(JoinHandle<()>, UnixStream)>::new();
   let result = serve(&socket.listener, &daemon, &stopped, &mut workers);
   daemon.stop();
   for worker in &workers {
      match worker.1.shutdown(Shutdown::Both) {
         Ok(()) => {},
         Err(error) if error.kind() == io::ErrorKind::NotConnected => {},
         Err(error) => warn(format_args!("could not stop client with {error}")),
      }
   }
   for (worker, _) in workers {
      worker
         .join()
         .map_err(|_error| report!("cache worker panicked"))?;
   }
   daemon.close()?;
   result.map_err(Into::into)
}

#[inline]
pub fn note(message: Arguments<'_>) {
   drop(writeln!(io::stderr().lock(), "{} {message}", timestamp()));
}

#[inline]
pub fn warn(message: Arguments<'_>) {
   drop(writeln!(
      io::stderr().lock(),
      "{} warning: {message}",
      timestamp()
   ));
}

fn timestamp() -> String {
   let seconds = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |since| since.as_secs());
   let days = seconds / 86400;
   let clock = seconds % 86400;
   let shifted = days + 719_468;
   let era = shifted / 146_097;
   let day_of_era = shifted % 146_097;
   let year_of_era =
      (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
   let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
   let month_index = (5 * day_of_year + 2) / 153;
   let day = day_of_year - (153 * month_index + 2) / 5 + 1;
   let month = if month_index < 10 {
      month_index + 3
   } else {
      month_index - 9
   };
   let year = year_of_era + era * 400 + u64::from(month <= 2);
   format!(
      "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
      clock / 3600,
      clock % 3600 / 60,
      clock % 60
   )
}

fn gibibytes(size: u64, option: &str) -> Result<u64> {
   size
      .checked_mul(1 << 30)
      .ok_or_else(|| report!("{option} is too large"))
}

fn serve(
   listener: &UnixListener,
   daemon: &Arc<Daemon>,
   stopped: &AtomicBool,
   workers: &mut Vec<(JoinHandle<()>, UnixStream)>,
) -> io::Result<()> {
   #[cfg(target_os = "linux")]
   let nixbld = read_to_string("/etc/group")?
      .lines()
      .find_map(|line| {
         let entry = line.strip_prefix("nixbld:")?;
         entry.split(':').nth(1)
      })
      .map(str::parse::<u32>)
      .transpose()
      .map_err(io::Error::other)?;
   while !stopped.load(Ordering::Relaxed) {
      let mut position = 0;
      while position < workers.len() {
         if workers[position].0.is_finished() {
            let (worker, _) = workers.swap_remove(position);
            worker
               .join()
               .map_err(|_error| io::Error::other("cache worker panicked"))?;
         } else {
            position += 1;
         }
      }
      let mut descriptors = [PollFd::new(listener, PollFlags::IN)];
      let timeout = Timespec {
         tv_sec:  0,
         tv_nsec: 200_000_000,
      };
      match poll(&mut descriptors, Some(&timeout)) {
         Ok(0) | Err(Errno::INTR) => continue,
         Err(error) => return Err(error.into()),
         Ok(_) => {},
      }
      let stream = match listener.accept() {
         Ok((stream, _)) => stream,
         Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
         // out of descriptors or an aborted client is no reason to drop every build's cache
         Err(error)
            if matches!(
               Errno::from_io_error(&error),
               Some(
                  Errno::MFILE | Errno::NFILE | Errno::CONNABORTED | Errno::NOMEM | Errno::NOBUFS
               )
            ) =>
         {
            warn(format_args!("could not accept a client with {error}"));
            sleep(Duration::from_millis(100));
            continue;
         },
         Err(error) => return Err(error),
      };
      #[cfg(target_os = "linux")]
      if !allowed_peer(&stream, nixbld) {
         continue;
      }
      let control = stream.try_clone()?;
      let mut connection = Connection::new(Arc::clone(daemon), stream)?;
      let worker = Builder::new().spawn(move || {
         if let Err(error) = connection.run()
            && !matches!(
               error.kind(),
               io::ErrorKind::BrokenPipe
                  | io::ErrorKind::ConnectionReset
                  | io::ErrorKind::ConnectionAborted
                  | io::ErrorKind::UnexpectedEof
            )
         {
            warn(format_args!("client request failed with {error}"));
         }
      })?;
      workers.push((worker, control));
   }
   Ok(())
}

#[cfg(target_os = "linux")]
fn allowed_peer(stream: &UnixStream, nixbld: Option<u32>) -> bool {
   let Ok(peer) = socket_peercred(stream) else {
      return false;
   };
   let uid = peer.uid.as_raw();
   peer.uid == getuid()
      || uid == 0
      || nixbld == Some(peer.gid.as_raw())
      || (872_415_232..880_803_840).contains(&uid)
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests")]
mod test_directory {
   use std::{
      env::temp_dir,
      fs::{
         DirBuilder,
         remove_dir_all,
      },
      io::ErrorKind,
      ops::Deref,
      path::{
         Path,
         PathBuf,
      },
      process::id,
      sync::atomic::{
         AtomicU64,
         Ordering,
      },
   };

   static NEXT_ID: AtomicU64 = AtomicU64::new(0);

   pub struct TestDirectory(PathBuf);

   impl TestDirectory {
      pub fn new() -> Self {
         loop {
            let number = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = temp_dir().join(format!("motley-mummerd-{}-{number}", id()));
            let creation = DirBuilder::new().create(&path);
            if matches!(&creation, Err(error) if error.kind() == ErrorKind::AlreadyExists) {
               continue;
            }
            creation.expect("cannot create test directory");
            return Self(path);
         }
      }
   }

   impl Deref for TestDirectory {
      type Target = Path;

      fn deref(&self) -> &Path {
         &self.0
      }
   }

   impl Drop for TestDirectory {
      fn drop(&mut self) {
         remove_dir_all(&self.0).expect("cannot remove test directory");
      }
   }
}
