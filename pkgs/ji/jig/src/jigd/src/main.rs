mod identity;
mod server;
mod slots;
mod store;

use std::{
   env::{
      VarError,
      args_os,
      var,
      var_os,
   },
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
   path::{
      Path,
      PathBuf,
   },
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
   },
   time::{
      SystemTime,
      UNIX_EPOCH,
   },
};

use eyre::{
   Result,
   WrapErr as _,
   bail,
   eyre,
};
use rustix::{
   event::{
      PollFd,
      PollFlags,
      Timespec,
      poll,
   },
   io::Errno,
};
use signal_hook::{
   consts::{
      SIGINT,
      SIGTERM,
   },
   flag::register,
};

use crate::{
   server::{
      Connection,
      Daemon,
   },
   store::{
      Store,
      Tier,
   },
};

const USAGE: &str = "jigd <socket>\n\nJIGD_SIZE sets the cache budget in GiB, default 50, zero \
                     means unlimited\nJIGD_COLD names a second, slower pack directory that takes \
                     what JIGD_SIZE pushes out\nJIGD_COLD_SIZE sets its budget in GiB, default \
                     unlimited\nJIGD_SLOTS sets concurrent compiler slots, default available \
                     CPUs\nXDG_CACHE_HOME holds jigd/packs, default $HOME/.cache\n";

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
   color_eyre::install()?;
   let arguments = args_os().skip(1).collect::<Vec<_>>();
   if arguments.len() == 1 && (arguments[0] == "--help" || arguments[0] == "-h") {
      io::stdout().lock().write_all(USAGE.as_bytes())?;
      return Ok(());
   }
   let [ref socket_path] = *arguments.as_slice() else {
      bail!("{USAGE}");
   };
   let cache = match var_os("XDG_CACHE_HOME") {
      Some(path) if !path.is_empty() => PathBuf::from(path),
      Some(_) | None => {
         PathBuf::from(var_os("HOME").ok_or_else(|| eyre!("HOME is not set"))?).join(".cache")
      },
   };
   let mut tiers = vec![Tier {
      directory: cache.join("jigd/packs"),
      budget:    gibibytes("JIGD_SIZE", 50)?,
   }];
   match var_os("JIGD_COLD") {
      Some(path) if !path.is_empty() => {
         tiers.push(Tier {
            directory: PathBuf::from(path),
            budget:    gibibytes("JIGD_COLD_SIZE", 0)?,
         });
      },
      Some(_) | None => {},
   }
   let limit = match var("JIGD_SLOTS") {
      Ok(value) => {
         value
            .parse::<usize>()
            .wrap_err("JIGD_SLOTS must be a positive integer")?
      },
      Err(VarError::NotPresent) => available_parallelism()?.get(),
      Err(error) => return Err(error.into()),
   };
   if limit == 0 {
      bail!("JIGD_SLOTS must be positive");
   }

   let store = Store::open(tiers).wrap_err("could not open cache")?;
   let socket =
      Socket::bind(Path::new(socket_path).to_owned()).wrap_err("could not bind socket")?;
   let stopped = Arc::new(AtomicBool::new(false));
   register(SIGINT, Arc::clone(&stopped))?;
   register(SIGTERM, Arc::clone(&stopped))?;
   note(format_args!(
      "listening on {}, {limit} slots, {}",
      socket.path.display(),
      store.stats()
   ));
   let daemon = Arc::new(Daemon::new(store, limit));
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
      worker.join().map_err(|_error| eyre!("cache worker panicked"))?;
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
   drop(writeln!(io::stderr().lock(), "{} warning: {message}", timestamp()));
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

fn gibibytes(name: &str, fallback: u64) -> Result<u64> {
   let size = match var(name) {
      Ok(value) => {
         value
            .parse::<u64>()
            .wrap_err_with(|| format!("{name} must be a nonnegative integer"))?
      },
      Err(VarError::NotPresent) => fallback,
      Err(error) => return Err(error.into()),
   };
   size
      .checked_mul(1 << 30)
      .ok_or_else(|| eyre!("{name} is too large"))
}

fn serve(
   listener: &UnixListener,
   daemon: &Arc<Daemon>,
   stopped: &AtomicBool,
   workers: &mut Vec<(JoinHandle<()>, UnixStream)>,
) -> io::Result<()> {
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
         Err(error) => return Err(error),
      };
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
            let path = temp_dir().join(format!("repkgs-jigd-{}-{number}", id()));
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
