use std::{
   collections::BTreeMap,
   io::{
      self,
      BufReader,
      BufWriter,
      Cursor,
      Write as _,
   },
   os::unix::net::UnixStream,
   path::Path,
   sync::{
      Arc,
      Mutex,
      atomic::{
         AtomicBool,
         AtomicU64,
         Ordering,
      },
      mpsc::{
         SyncSender,
         TrySendError,
         sync_channel,
      },
   },
   thread::{
      Builder,
      JoinHandle,
      scope,
   },
};

use tightrope::{
   read_line,
   request::Request,
};

use crate::{
   identity::Identities,
   remote::Remote,
   slots::Slots,
   store::{
      Store,
      Value,
   },
   warn,
};

const UPLOAD_QUEUE: usize = 4096;
const REMOTE_PARALLEL: usize = 16;

type Uploads = SyncSender<(String, Vec<u8>)>;

pub struct Daemon {
   store:       Store,
   slots:       Slots,
   identities:  Identities,
   gets:        AtomicU64,
   hits:        AtomicU64,
   puts:        AtomicU64,
   remote:      Option<Arc<Remote>>,
   remote_hits: AtomicU64,
   uploads:     Mutex<Option<Uploads>>,
   uploader:    Mutex<Option<JoinHandle<()>>>,
   dropped:     AtomicBool,
}

impl Daemon {
   pub fn stop(&self) {
      self.slots.stop();
   }

   pub fn close(&self) -> io::Result<()> {
      let uploads = self
         .uploads
         .lock()
         .map_err(|_error| io::Error::other("upload queue poisoned"))?
         .take();
      drop(uploads);
      let uploader = self
         .uploader
         .lock()
         .map_err(|_error| io::Error::other("uploader poisoned"))?
         .take();
      if let Some(handle) = uploader {
         handle
            .join()
            .map_err(|_error| io::Error::other("uploader panicked"))?;
      }
      self.store.close()
   }

   pub fn new(
      store: Store,
      limit: usize,
      store_dir: &Path,
      tier: Option<Remote>,
   ) -> io::Result<Self> {
      let remote = tier.map(Arc::new);
      let (uploads, uploader) = match remote {
         Some(ref shared) if shared.writes() => {
            let (sender, receiver) = sync_channel::<(String, Vec<u8>)>(UPLOAD_QUEUE);
            let writer = Arc::clone(shared);
            let uploader = Builder::new().name("upload".to_owned()).spawn(move || {
               for (key, value) in receiver {
                  writer.put(&key, &value);
               }
            })?;
            (Some(sender), Some(uploader))
         },
         Some(_) | None => (None, None),
      };
      Ok(Self {
         store,
         slots: Slots::new(limit),
         identities: Identities::new(store_dir)?,
         gets: AtomicU64::new(0),
         hits: AtomicU64::new(0),
         puts: AtomicU64::new(0),
         remote,
         remote_hits: AtomicU64::new(0),
         uploads: Mutex::new(uploads),
         uploader: Mutex::new(uploader),
         dropped: AtomicBool::new(false),
      })
   }

   /// The local values, else the remote's, asked for together and kept
   /// locally so the next lookup stays here.
   fn lookup(&self, keys: &[String]) -> io::Result<Vec<Option<Value>>> {
      let mut values = keys
         .iter()
         .map(|key| self.store.get(key))
         .collect::<Vec<_>>();
      let Some(ref remote) = self.remote else {
         return Ok(values);
      };
      let missing = (0..keys.len())
         .filter(|&position| values[position].is_none())
         .collect::<Vec<_>>();
      for group in missing.chunks(REMOTE_PARALLEL) {
         let fetched = scope(|threads| {
            group
               .iter()
               .map(|&position| threads.spawn(move || remote.get(&keys[position])))
               .collect::<Vec<_>>()
               .into_iter()
               .map(|thread| thread.join().ok().flatten())
               .collect::<Vec<_>>()
         });
         for (&position, found) in group.iter().zip(fetched) {
            let Some(bytes) = found else {
               continue;
            };
            self.remote_hits.fetch_add(1, Ordering::Relaxed);
            self
               .store
               .put(&keys[position], &mut Cursor::new(&bytes), bytes.len())?;
            values[position] = self.store.get(&keys[position]);
         }
      }
      Ok(values)
   }

   /// Queues a value just stored for the remote, dropping it rather than
   /// holding up the build when the queue is full.
   fn upload(&self, key: &str) -> io::Result<()> {
      let queue = self
         .uploads
         .lock()
         .map_err(|_error| io::Error::other("upload queue poisoned"))?
         .clone();
      let (Some(sender), Some(value)) = (queue, self.store.get(key)) else {
         return Ok(());
      };
      match sender.try_send((key.to_owned(), value.read()?)) {
         Ok(()) | Err(TrySendError::Disconnected(_)) => {},
         Err(TrySendError::Full(_)) => {
            if !self.dropped.swap(true, Ordering::Relaxed) {
               warn(format_args!("remote upload queue is full, dropping writes"));
            }
         },
      }
      Ok(())
   }
}

pub struct Connection {
   daemon: Arc<Daemon>,
   reader: BufReader<UnixStream>,
   writer: BufWriter<UnixStream>,
   held:   BTreeMap<String, usize>,
}

impl Connection {
   pub fn new(daemon: Arc<Daemon>, stream: UnixStream) -> io::Result<Self> {
      let writer = BufWriter::new(stream.try_clone()?);
      Ok(Self {
         daemon,
         reader: BufReader::new(stream),
         writer,
         held: BTreeMap::new(),
      })
   }

   pub fn run(&mut self) -> io::Result<()> {
      while let Some(line) = read_line(&mut self.reader)? {
         match line.parse()? {
            Request::Get(first) => {
               let mut keys = vec![first];
               while let Some(key) = self.buffered_get()? {
                  keys.push(key);
               }
               for value in self.daemon.lookup(&keys)? {
                  self.daemon.gets.fetch_add(1, Ordering::Relaxed);
                  if let Some(object) = value {
                     self.daemon.hits.fetch_add(1, Ordering::Relaxed);
                     writeln!(self.writer, "OK {}", object.size())?;
                     self.writer.flush()?;
                     object.write_to(self.writer.get_ref())?;
                  } else {
                     self.writer.write_all(b"MISS\n")?;
                  }
               }
            },
            Request::Has(key) => {
               let exists = self
                  .daemon
                  .lookup(&[key])?
                  .first()
                  .is_some_and(Option::is_some);
               self
                  .writer
                  .write_all(if exists { b"1\n" } else { b"0\n" })?;
            },
            Request::Put { key, length } => {
               self.daemon.store.put(&key, &mut self.reader, length)?;
               self.daemon.puts.fetch_add(1, Ordering::Relaxed);
               self.daemon.upload(&key)?;
               self.writer.write_all(b"OK\n")?;
            },
            Request::Identities(count) => {
               for _ in 0..count {
                  let path = read_line(&mut self.reader)?.ok_or_else(|| {
                     io::Error::new(io::ErrorKind::UnexpectedEof, "missing identity path")
                  })?;
                  match self.daemon.identities.of(&path) {
                     Ok(identity) => writeln!(self.writer, "{identity}")?,
                     Err(_) => self.writer.write_all(b"\n")?,
                  }
               }
            },
            Request::Acquire(build) => {
               self.daemon.slots.acquire(&build, self.reader.get_ref())?;
               *self.held.entry(build).or_default() += 1;
               self.writer.write_all(b"OK\n")?;
            },
            Request::Release(build) => {
               if let Some(count) = self.held.get_mut(&build) {
                  *count -= 1;
                  if *count == 0 {
                     self.held.remove(&build);
                  }
                  self.daemon.slots.release(&build, 1);
               }
               self.writer.write_all(b"OK\n")?;
            },
            Request::Stats => {
               let (occupied, waiting) = self.daemon.slots.stats();
               writeln!(
                  self.writer,
                  "gets={} hits={} remote={} puts={} ids={} slots={} waiting={} {}",
                  self.daemon.gets.load(Ordering::Relaxed),
                  self.daemon.hits.load(Ordering::Relaxed),
                  self.daemon.remote_hits.load(Ordering::Relaxed),
                  self.daemon.puts.load(Ordering::Relaxed),
                  self.daemon.identities.len(),
                  occupied,
                  waiting,
                  self.daemon.store.stats()
               )?;
            },
         }
         self.writer.flush()?;
      }
      Ok(())
   }

   /// The next request when it is a GET the client already pipelined, so a
   /// batch reaches the remote together.
   fn buffered_get(&mut self) -> io::Result<Option<String>> {
      let buffered = self.reader.buffer();
      if !buffered.starts_with(b"GET ") || !buffered.contains(&b'\n') {
         return Ok(None);
      }
      match read_line(&mut self.reader)?
         .map(|line| line.parse())
         .transpose()?
      {
         Some(Request::Get(key)) => Ok(Some(key)),
         _ => Err(io::Error::other("a buffered GET did not parse as one")),
      }
   }
}

impl Drop for Connection {
   fn drop(&mut self) {
      while let Some((build, count)) = self.held.pop_first() {
         self.daemon.slots.release(&build, count);
      }
   }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests")]
mod tests {
   use std::{
      fs::write,
      io::{
         BufRead as _,
         BufReader,
         Read as _,
         Write as _,
      },
      os::unix::net::UnixStream,
      sync::Arc,
      thread::spawn,
      time::Duration,
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
      test_directory::TestDirectory,
   };

   #[test]
   fn serve_ids_and_pipelined_gets() {
      let directory = TestDirectory::new();
      let header = directory.join("header.h");
      write(&header, b"abc").expect("write header");
      let daemon = Arc::new(
         Daemon::new(
            Store::open(vec![Tier {
               directory: directory.join("packs"),
               budget:    1 << 30,
            }])
            .expect("open store"),
            1,
            &directory,
            None,
         )
         .expect("create daemon"),
      );
      let (mut client, server) = UnixStream::pair().expect("socket pair");
      client
         .set_read_timeout(Some(Duration::from_secs(5)))
         .expect("set timeout");
      let mut connection = Connection::new(Arc::clone(&daemon), server).expect("connection");
      let worker = spawn(move || connection.run().expect("serve requests"));
      write!(
         client,
         "PUT k1 3\nabcIDS 2\n{}\n{}/missing\nGET nope\nGET k1\n",
         header.display(),
         directory.display()
      )
      .expect("pipeline requests");
      let mut reader = BufReader::new(&client);
      for expected in [
         "OK\n",
         "C:6437b3ac38465133ffb63b75273a8db5\n",
         "\n",
         "MISS\n",
         "OK 3\n",
      ] {
         let mut reply = String::new();
         reader.read_line(&mut reply).expect("read response");
         assert_eq!(reply, expected);
      }
      let mut value = [0; 3];
      reader.read_exact(&mut value).expect("read value");
      assert_eq!(&value, b"abc");
      drop(reader);
      client
         .write_all(b"SLOT b1\nDONE b1\nSLOT b1\n")
         .expect("slot requests");
      let mut replies = BufReader::new(&client);
      for _ in 0_usize..3_usize {
         let mut reply = String::new();
         replies.read_line(&mut reply).expect("slot reply");
         assert_eq!(reply, "OK\n");
      }
      assert_eq!(daemon.slots.stats().0, 1);
      drop(replies);
      drop(client);
      worker.join().expect("server worker panicked");
      let (mut next_client, next_server) = UnixStream::pair().expect("second socket pair");
      next_client
         .set_read_timeout(Some(Duration::from_secs(5)))
         .expect("set timeout");
      let mut next_connection =
         Connection::new(Arc::clone(&daemon), next_server).expect("second connection");
      let next_worker = spawn(move || next_connection.run().expect("serve second connection"));
      next_client
         .write_all(b"SLOT b2\n")
         .expect("acquire released slot");
      let mut answer = String::new();
      BufReader::new(&next_client)
         .read_line(&mut answer)
         .expect("slot after disconnect");
      assert_eq!(answer, "OK\n");
      drop(next_client);
      next_worker.join().expect("second worker panicked");
   }
}
