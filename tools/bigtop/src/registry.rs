//! A read-only npm registry over a vendored package-lock.json, for test
//! suites that install packages: every locked version, its packument built
//! from the lock entry and its tarball read from the store.

use std::{
   cmp::Ordering,
   collections::BTreeMap,
   fs::{
      File,
      read_to_string,
   },
   io::{
      self,
      BufRead as _,
      BufReader,
      ErrorKind,
      Read as _,
      Write as _,
   },
   net::{
      Ipv4Addr,
      TcpListener,
      TcpStream,
   },
   path::{
      Path,
      PathBuf,
   },
   process::{
      Child,
      Command,
      Stdio,
   },
   str::from_utf8,
   string::FromUtf8Error,
   sync::Arc,
   thread,
};

use mummer::tree::note;
use rustix::process::{
   Signal,
   set_parent_process_death_signal,
};
use serde::Deserialize;
use serde_json::{
   Map,
   Value,
};

use crate::{
   attrs::TestVar,
   schema::PackagePath,
};

/// The variables npm and bun take a registry from.
pub const VARIABLES: [&str; 2] = ["npm_config_registry", "BUN_CONFIG_REGISTRY"];

/// A registry child for `[test] registry`, kept until `stop`.
pub struct Serving(Child);

/// The lock fields a version's packument entry carries over as they are.
const CARRIED: [&str; 9] = [
   "dependencies",
   "optionalDependencies",
   "peerDependencies",
   "peerDependenciesMeta",
   "bin",
   "os",
   "cpu",
   "engines",
   "hasInstallScript",
];

#[derive(Deserialize)]
struct Lock {
   packages: BTreeMap<String, Map<String, Value>>,
}

struct Registry {
   packuments: BTreeMap<String, Vec<u8>>,
   tarballs:   BTreeMap<String, PathBuf>,
}

/// Serves `deps`, an npmDeps output, to the test phases through
/// `VARIABLES` in `test_env`. The child dies with this process.
pub fn start(
   deps: Option<&PackagePath>,
   test_env: &mut BTreeMap<String, TestVar>,
) -> io::Result<Option<Serving>> {
   let Some(served) = deps else {
      return Ok(None);
   };
   let (child, url) = spawn(served.as_ref())?;
   for name in VARIABLES {
      test_env.insert(name.to_owned(), TestVar::Set(url.clone()));
   }
   Ok(Some(Serving(child)))
}

impl Serving {
   pub fn stop(mut self) -> io::Result<()> {
      self.0.kill()?;
      self.0.wait().map(drop)
   }
}

fn spawn(deps: &Path) -> io::Result<(Child, String)> {
   let mut child = Command::new("/proc/self/exe")
      .arg("registry")
      .arg(deps.join("package-lock.json"))
      .stdout(Stdio::piped())
      .spawn()?;
   let stdout = child
      .stdout
      .take()
      .ok_or_else(|| io::Error::other("registry: no stdout"))?;
   let mut url = String::new();
   BufReader::new(stdout).read_line(&mut url)?;
   if url.is_empty() {
      return Err(io::Error::other("registry: exited before listening"));
   }
   Ok((child, url.trim_end().to_owned()))
}

/// Serves `lock` until the parent dies, after printing the URL.
pub fn serve(lock: &Path) -> io::Result<()> {
   set_parent_process_death_signal(Some(Signal::KILL))?;
   let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
   let base = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
   let registry = Arc::new(Registry::load(lock, &base)?);
   let mut stdout = io::stdout().lock();
   writeln!(stdout, "{base}/")?;
   stdout.flush()?;
   drop(stdout);
   for incoming in listener.incoming() {
      let stream = incoming?;
      let shared = Arc::clone(&registry);
      thread::spawn(move || {
         match shared.answer(stream) {
            Err(error)
               if !matches!(
                  error.kind(),
                  ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
               ) =>
            {
               note("registry", &error.to_string());
            },
            Ok(()) | Err(_) => {},
         }
      });
   }
   Ok(())
}

impl Registry {
   fn load(lock: &Path, base: &str) -> io::Result<Self> {
      let read = serde_json::from_str::<Lock>(&read_to_string(lock)?)?;
      let mut versions = BTreeMap::<String, Map<String, Value>>::new();
      let mut tarballs = BTreeMap::new();
      for (key, entry) in read.packages {
         let Some(located) = key.rsplit_once("node_modules/").map(|split| split.1) else {
            continue;
         };
         if entry.get("link").and_then(Value::as_bool) == Some(true) {
            continue;
         }
         let name = entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(located)
            .to_owned();
         let text = |field: &str| {
            entry
               .get(field)
               .and_then(Value::as_str)
               .ok_or_else(|| io::Error::other(format!("registry: {key} has no {field}")))
         };
         let version = text("version")?.to_owned();
         let resolved = text("resolved")?;
         let integrity = text("integrity")?;
         let stored = resolved.strip_prefix("file:").ok_or_else(|| {
            io::Error::other(format!("registry: {key} is not vendored, {resolved}"))
         })?;
         let file = Path::new(stored)
            .file_name()
            .and_then(|file| file.to_str())
            .ok_or_else(|| io::Error::other(format!("registry: {stored} has no file name")))?;
         let route = format!("/{name}/-/{file}");
         let mut manifest = Map::new();
         manifest.insert("name".to_owned(), Value::String(name.clone()));
         manifest.insert("version".to_owned(), Value::String(version.clone()));
         for field in CARRIED {
            if let Some(value) = entry.get(field) {
               manifest.insert(field.to_owned(), value.clone());
            }
         }
         let mut dist = Map::new();
         dist.insert(
            "tarball".to_owned(),
            Value::String(format!("{base}{route}")),
         );
         dist.insert("integrity".to_owned(), Value::String(integrity.to_owned()));
         manifest.insert("dist".to_owned(), Value::Object(dist));
         tarballs.insert(route, PathBuf::from(stored));
         versions
            .entry(name)
            .or_default()
            .insert(version, Value::Object(manifest));
      }
      let packuments = versions
         .into_iter()
         .map(|(name, listed)| {
            let latest = listed
               .keys()
               .max_by(|left, right| precedence(left, right))
               .cloned()
               .unwrap_or_default();
            let mut tags = Map::new();
            tags.insert("latest".to_owned(), Value::String(latest));
            let mut packument = Map::new();
            packument.insert("name".to_owned(), Value::String(name.clone()));
            packument.insert("dist-tags".to_owned(), Value::Object(tags));
            packument.insert("versions".to_owned(), Value::Object(listed));
            Ok((name, serde_json::to_vec(&packument)?))
         })
         .collect::<io::Result<_>>()?;
      Ok(Self {
         packuments,
         tarballs,
      })
   }

   fn answer(&self, mut stream: TcpStream) -> io::Result<()> {
      let mut reader = BufReader::new(stream.try_clone()?);
      let mut request = String::new();
      reader.read_line(&mut request)?;
      loop {
         let mut header = String::new();
         if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
         }
      }
      let mut words = request.split_whitespace();
      let method = words.next().unwrap_or_default();
      let Ok(target) = decode(words.next().unwrap_or_default()) else {
         return respond(&mut stream, "400 Bad Request", "text/plain", &[], false);
      };
      let path = target.split('?').next().unwrap_or_default();
      let head = method == "HEAD";
      if method != "GET" && !head {
         return respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            &[],
            head,
         );
      }
      if let Some(packument) = self.packuments.get(path.trim_start_matches('/')) {
         return respond(&mut stream, "200 OK", "application/json", packument, head);
      }
      if let Some(stored) = self.tarballs.get(path) {
         let mut bytes = Vec::new();
         File::open(stored)?.read_to_end(&mut bytes)?;
         return respond(
            &mut stream,
            "200 OK",
            "application/octet-stream",
            &bytes,
            head,
         );
      }
      note("registry", &format!("no {path}"));
      respond(
         &mut stream,
         "404 Not Found",
         "application/json",
         b"{}",
         head,
      )
   }
}

fn respond(
   stream: &mut TcpStream,
   status: &str,
   kind: &str,
   body: &[u8],
   head: bool,
) -> io::Result<()> {
   write!(
      stream,
      "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: \
       close\r\n\r\n",
      body.len()
   )?;
   if !head {
      stream.write_all(body)?;
   }
   stream.flush()
}

/// npm and bun spell a scoped name's slash as %2f.
fn decode(target: &str) -> Result<String, FromUtf8Error> {
   let bytes = target.as_bytes();
   let mut decoded = Vec::with_capacity(bytes.len());
   let mut at = 0;
   while let Some(&byte) = bytes.get(at) {
      let escaped = (byte == b'%')
         .then(|| bytes.get(at + 1..at + 3))
         .flatten()
         .and_then(|hex| from_utf8(hex).ok())
         .and_then(|hex| u8::from_str_radix(hex, 16).ok());
      if let Some(value) = escaped {
         decoded.push(value);
         at += 3;
      } else {
         decoded.push(byte);
         at += 1;
      }
   }
   String::from_utf8(decoded)
}

/// semver precedence, enough to name `latest`: numeric fields, then a
/// release above its prereleases.
fn precedence(left: &str, right: &str) -> Ordering {
   let split = |version: &str| {
      let unbuilt = version.split_once('+').map_or(version, |parts| parts.0);
      let (core, pre) = unbuilt
         .split_once('-')
         .map_or((unbuilt, None), |(core, pre)| (core, Some(pre.to_owned())));
      let numbers = core
         .split('.')
         .map(|part| part.parse::<u64>().unwrap_or(0))
         .collect::<Vec<_>>();
      (numbers, pre)
   };
   let (left_numbers, left_pre) = split(left);
   let (right_numbers, right_pre) = split(right);
   left_numbers.cmp(&right_numbers).then_with(|| {
      match (left_pre, right_pre) {
         (None, None) => Ordering::Equal,
         (None, Some(_)) => Ordering::Greater,
         (Some(_), None) => Ordering::Less,
         (Some(left_tag), Some(right_tag)) => left_tag.cmp(&right_tag),
      }
   })
}
