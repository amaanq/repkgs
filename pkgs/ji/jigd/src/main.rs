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
   fs::{
      Permissions,
      create_dir_all,
      remove_file,
      set_permissions,
      symlink_metadata,
   },
   io::{
      self,
      IsTerminal,
      Write,
   },
   net::Shutdown,
   os::unix::{
      fs::{
         FileTypeExt,
         MetadataExt,
         PermissionsExt,
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
};

use eyre::{
   Result,
   WrapErr,
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
use tracing::{
   error,
   info,
   warn,
};

use crate::{
   server::{
      Connection,
      Daemon,
   },
   store::Store,
};

const USAGE: &str = "jigd <socket>\n\nJIGD_SIZE sets the cache budget in GiB, default 50, zero \
                     means unlimited\nJIGD_SLOTS sets concurrent compiler slots, default \
                     available CPUs\nXDG_CACHE_HOME holds jigd/packs, default $HOME/.cache\n";

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
         error!(
            "failed to remove socket {} with {error}",
            self.path.display()
         );
      }
   }
}

fn main() -> Result<()> {
   color_eyre::install()?;
   tracing_subscriber::fmt()
      .with_target(false)
      .with_ansi(io::stderr().is_terminal())
      .with_writer(io::stderr)
      .init();
   let arguments: Vec<_> = args_os().skip(1).collect();
   if arguments.len() == 1 && (arguments[0] == "--help" || arguments[0] == "-h") {
      io::stdout().lock().write_all(USAGE.as_bytes())?;
      return Ok(());
   }
   let [socket_path] = arguments.as_slice() else {
      bail!("{USAGE}");
   };
   let cache = match var_os("XDG_CACHE_HOME") {
      Some(path) if !path.is_empty() => PathBuf::from(path),
      Some(_) => bail!("XDG_CACHE_HOME must not be empty"),
      None => PathBuf::from(var_os("HOME").ok_or_else(|| eyre!("HOME is not set"))?).join(".cache"),
   };
   let size = match var("JIGD_SIZE") {
      Ok(value) => {
         value
            .parse::<u64>()
            .wrap_err("JIGD_SIZE must be a nonnegative integer")?
      },
      Err(VarError::NotPresent) => 50,
      Err(error) => return Err(error.into()),
   };
   let budget = size
      .checked_mul(1 << 30)
      .ok_or_else(|| eyre!("JIGD_SIZE is too large"))?;
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

   let store = Store::open(&cache.join("jigd/packs"), budget).wrap_err("could not open cache")?;
   let socket =
      Socket::bind(Path::new(socket_path).to_owned()).wrap_err("could not bind socket")?;
   let stopped = Arc::new(AtomicBool::new(false));
   register(SIGINT, Arc::clone(&stopped))?;
   register(SIGTERM, Arc::clone(&stopped))?;
   info!(
      "listening on {}, {limit} slots, {}",
      socket.path.display(),
      store.stats()
   );
   let daemon = Arc::new(Daemon::new(store, limit));
   let mut workers: Vec<(JoinHandle<()>, UnixStream)> = Vec::new();
   let result = serve(&socket.listener, &daemon, &stopped, &mut workers);
   daemon.stop();
   for (_, stream) in &workers {
      match stream.shutdown(Shutdown::Both) {
         Ok(()) => {},
         Err(error) if error.kind() == io::ErrorKind::NotConnected => {},
         Err(error) => warn!("could not stop client with {error}"),
      }
   }
   for (worker, _) in workers {
      worker.join().expect("cache worker panicked");
   }
   daemon.close()?;
   result.map_err(Into::into)
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
            worker.join().expect("cache worker panicked");
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
         if let Err(error) = connection.run() {
            match error.kind() {
               io::ErrorKind::BrokenPipe
               | io::ErrorKind::ConnectionReset
               | io::ErrorKind::ConnectionAborted
               | io::ErrorKind::UnexpectedEof => {},
               _ => warn!("client request failed with {error}"),
            }
         }
      })?;
      workers.push((worker, control));
   }
   Ok(())
}

#[cfg(test)]
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
