use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   env::var,
   io::{
      self,
      BufReader,
      Read as _,
      Write as _,
   },
   os::unix::net::UnixStream,
};

use jig::{
   process::diagnostic,
   store::STORE_DIR,
};
use serde::{
   Deserialize,
   Serialize,
};
use sha2::{
   Digest as _,
   Sha256,
};

use crate::cli::StoreOperation;

#[derive(Default, Deserialize, Serialize)]
struct Output {
   #[serde(default)]
   path:      String,
   #[serde(default, rename = "hashAlgo")]
   algorithm: String,
   #[serde(default)]
   hash:      String,
}

#[derive(Default, Deserialize)]
struct Derivation {
   #[serde(default)]
   outputs: BTreeMap<String, Output>,
   #[serde(default, rename = "inputDrvs")]
   inputs:  BTreeMap<String, BTreeSet<String>>,
   #[serde(default, rename = "inputSrcs")]
   sources: BTreeSet<String>,
   system:  String,
   builder: String,
   #[serde(default)]
   args:    Vec<String>,
   #[serde(default)]
   env:     BTreeMap<String, String>,
}

impl Derivation {
   fn aterm(&self) -> String {
      let outputs = self
         .outputs
         .iter()
         .map(|(name, spec)| {
            format!(
               "({},{},{},{})",
               quote(name),
               quote(&spec.path),
               quote(&spec.algorithm),
               quote(&spec.hash)
            )
         })
         .collect::<Vec<_>>()
         .join(",");
      let inputs = self
         .inputs
         .iter()
         .map(|(path, names)| {
            format!(
               "({},[{}])",
               quote(path),
               names
                  .iter()
                  .map(|name| quote(name))
                  .collect::<Vec<_>>()
                  .join(",")
            )
         })
         .collect::<Vec<_>>()
         .join(",");
      let sources = self
         .sources
         .iter()
         .map(|source| quote(source))
         .collect::<Vec<_>>()
         .join(",");
      let arguments = self
         .args
         .iter()
         .map(|arg| quote(arg))
         .collect::<Vec<_>>()
         .join(",");
      let environment = self
         .env
         .iter()
         .map(|(key, value)| format!("({},{})", quote(key), quote(value)))
         .collect::<Vec<_>>()
         .join(",");
      format!(
         "Derive([{outputs}],[{inputs}],[{sources}],{},{},[{arguments}],[{environment}])",
         quote(&self.system),
         quote(&self.builder)
      )
   }
}

fn quote(text: &str) -> String {
   let mut result = String::from("\"");
   for character in text.chars() {
      match character {
         '"' => result.push_str("\\\""),
         '\\' => result.push_str("\\\\"),
         '\n' => result.push_str("\\n"),
         '\r' => result.push_str("\\r"),
         '\t' => result.push_str("\\t"),
         other => result.push(other),
      }
   }
   result.push('"');
   result
}

fn store_path(kind: &str, name: &str, contents: &[u8]) -> String {
   let inner = Sha256::digest(contents);
   let mut hex = String::with_capacity(64);
   for byte in inner {
      const HEX: &[u8; 16] = b"0123456789abcdef";
      hex.push(char::from(HEX[usize::from(byte >> 4_u8)]));
      hex.push(char::from(HEX[usize::from(byte & 0x0F)]));
   }
   let fingerprint = format!("{kind}:sha256:{hex}:{STORE_DIR}:{name}");
   let hash = Sha256::digest(fingerprint);
   let mut compressed = [0_u8; 20];
   for (index, byte) in hash.iter().enumerate() {
      compressed[index % 20] ^= byte;
   }
   let alphabet = b"0123456789abcdfghijklmnpqrsvwxyz";
   let mut encoded = String::new();
   for index in (0..32).rev() {
      let bit = index * 5;
      let low = u16::from(compressed[bit / 8]);
      let high = u16::from(compressed.get(bit / 8 + 1).copied().unwrap_or(0));
      let shift = bit % 8_usize;
      encoded.push(char::from(
         alphabet[usize::from(((low | (high << 8_u32)) >> shift) & 31_u16)],
      ));
   }
   format!("{STORE_DIR}/{encoded}-{name}")
}

#[derive(Deserialize)]
struct FetchFile {
   name: String,
   url:  String,
   algo: String,
   hex:  String,
   sri:  String,
}

impl FetchFile {
   fn derivation(&self) -> Derivation {
      let path = store_path(
         "output:out",
         &self.name,
         format!("fixed:out:{}:{}:", self.algo, self.hex).as_bytes(),
      );
      let environment = [
         ("name", self.name.as_str()),
         ("out", path.as_str()),
         ("outputHash", self.sri.as_str()),
         (
            "outputHashAlgo",
            if self.sri.starts_with(&format!("{}-", self.algo)) {
               ""
            } else {
               self.algo.as_str()
            },
         ),
         ("outputHashMode", "flat"),
         ("url", self.url.as_str()),
         ("urls", self.url.as_str()),
         ("executable", ""),
         ("unpack", ""),
         (
            "impureEnvVars",
            "http_proxy https_proxy ftp_proxy all_proxy no_proxy",
         ),
         ("preferLocalBuild", "1"),
         ("system", "builtin"),
         ("builder", "builtin:fetchurl"),
      ]
      .into_iter()
      .map(|(key, value)| (key.to_owned(), value.to_owned()))
      .collect::<BTreeMap<String, String>>();
      let output = Output {
         path,
         algorithm: self.algo.clone(),
         hash: self.hex.clone(),
      };
      Derivation {
         outputs: BTreeMap::from([("out".to_owned(), output)]),
         system: "builtin".to_owned(),
         builder: "builtin:fetchurl".to_owned(),
         env: environment,
         ..Derivation::default()
      }
   }
}

struct Connection {
   reader: BufReader<UnixStream>,
   output: Vec<u8>,
}

impl Connection {
   fn connect() -> io::Result<Self> {
      let remote = var("NIX_REMOTE").map_err(io::Error::other)?;
      let socket = remote
         .strip_prefix("unix://")
         .ok_or_else(|| io::Error::other("NIX_REMOTE must name the builder RPC Unix socket"))?;
      let mut connection = Self {
         reader: BufReader::new(UnixStream::connect(socket)?),
         output: Vec::new(),
      };
      connection.word(0x6E69_7863);
      connection.word(0x126);
      connection.flush()?;
      if connection.read_word()? != 0x6478_696F {
         return Err(io::Error::other("invalid Nix daemon magic"));
      }
      if connection.read_word()? < 0x126 {
         return Err(io::Error::other(
            "Nix daemon lacks the builder RPC protocol",
         ));
      }
      let features = [
         "realisation-with-path-not-hash",
         "disable-set-options",
         "add-to-store-scanning",
         "submit-output",
      ];
      connection.word(features.len().try_into().map_err(io::Error::other)?);
      for feature in features {
         connection.string(feature.as_bytes())?;
      }
      connection.flush()?;
      connection.skip_strings()?;
      connection.word(0);
      connection.word(0);
      connection.flush()?;
      connection.read_string()?;
      connection.read_word()?;
      connection.drain_logs()?;
      Ok(connection)
   }

   fn word(&mut self, value: u64) {
      self.output.extend(value.to_le_bytes());
   }
   fn string(&mut self, value: &[u8]) -> io::Result<()> {
      self.word(value.len().try_into().map_err(io::Error::other)?);
      self.output.extend_from_slice(value);
      self
         .output
         .resize(self.output.len() + (8 - value.len() % 8) % 8, 0);
      Ok(())
   }
   fn flush(&mut self) -> io::Result<()> {
      self.reader.get_mut().write_all(&self.output)?;
      self.output.clear();
      Ok(())
   }
   fn read_word(&mut self) -> io::Result<u64> {
      let mut bytes = [0; 8];
      self.reader.read_exact(&mut bytes)?;
      Ok(u64::from_le_bytes(bytes))
   }
   fn read_string(&mut self) -> io::Result<String> {
      let length_words = self.read_word()?;
      if length_words > 64_u64 << 20_u32 {
         return Err(io::Error::other("Nix daemon string exceeds size limit"));
      }
      let length = usize::try_from(length_words).map_err(io::Error::other)?;
      let mut bytes = vec![0; length];
      self.reader.read_exact(&mut bytes)?;
      let mut padding = [0; 8];
      self
         .reader
         .read_exact(&mut padding[..(8 - length % 8) % 8])?;
      String::from_utf8(bytes).map_err(io::Error::other)
   }
   fn skip_strings(&mut self) -> io::Result<()> {
      let count = self.read_word()?;
      if count > (1_u64 << 20_u32) {
         return Err(io::Error::other("Nix daemon list exceeds size limit"));
      }
      for _ in 0..usize::try_from(count).map_err(io::Error::other)? {
         self.read_string()?;
      }
      Ok(())
   }
   fn skip_fields(&mut self) -> io::Result<()> {
      let count = self.read_word()?;
      if count > (1_u64 << 20_u32) {
         return Err(io::Error::other("Nix daemon field list exceeds size limit"));
      }
      for _ in 0..usize::try_from(count).map_err(io::Error::other)? {
         match self.read_word()? {
            0 => {
               self.read_word()?;
            },
            1 => {
               self.read_string()?;
            },
            _ => return Err(io::Error::other("unknown Nix daemon log field")),
         }
      }
      Ok(())
   }
   fn drain_logs(&mut self) -> io::Result<()> {
      loop {
         match self.read_word()? {
            0x616C_7473 => return Ok(()),
            0x6378_7470 => {
               self.read_string()?;
               self.read_word()?;
               self.read_string()?;
               return Err(io::Error::other(self.read_string()?));
            },
            0x6F6C_6D67 => diagnostic(format_args!("{}", self.read_string()?)),
            0x5354_5254 => {
               self.read_word()?;
               self.read_word()?;
               self.read_word()?;
               self.read_string()?;
               self.skip_fields()?;
               self.read_word()?;
            },
            0x5354_4F50 => {
               self.read_word()?;
            },
            0x5253_4C54 => {
               self.read_word()?;
               self.read_word()?;
               self.skip_fields()?;
            },
            _ => return Err(io::Error::other("unexpected Nix daemon message")),
         }
      }
   }

   fn add_text(
      &mut self,
      name: &str,
      references: &BTreeSet<String>,
      bytes: &[u8],
   ) -> io::Result<String> {
      self.word(7);
      self.string(name.as_bytes())?;
      self.string(b"text:sha256")?;
      self.word(references.len().try_into().map_err(io::Error::other)?);
      for reference in references {
         self.string(reference.as_bytes())?;
      }
      self.word(0);
      self.word(bytes.len().try_into().map_err(io::Error::other)?);
      self.output.extend_from_slice(bytes);
      self.word(0);
      self.flush()?;
      self.drain_logs()?;
      let path = self.read_string()?;
      self.read_string()?;
      self.read_string()?;
      self.skip_strings()?;
      self.read_word()?;
      self.read_word()?;
      self.read_word()?;
      self.skip_strings()?;
      self.read_string()?;
      Ok(path)
   }

   fn submit(&mut self, path: &str, output: &str) -> io::Result<()> {
      self.word(1000);
      self.word(0);
      self.string(path.as_bytes())?;
      self.string(output.as_bytes())?;
      self.flush()?;
      self.drain_logs()?;
      self.read_word()?;
      Ok(())
   }
}

fn add_text(
   connection: &mut Option<Connection>,
   name: &str,
   references: &BTreeSet<String>,
   bytes: &[u8],
) -> io::Result<String> {
   connection.as_mut().map_or_else(
      || {
         let mut kind = "text".to_owned();
         for reference in references {
            kind.push(':');
            kind.push_str(reference);
         }
         Ok(store_path(&kind, name, bytes))
      },
      |daemon| daemon.add_text(name, references, bytes),
   )
}

#[derive(Serialize)]
struct Realisation {
   drv: String,
   out: String,
}

fn add_from_stdin(
   connection: &mut Option<Connection>,
   name: &str,
   references: &[String],
   derivation: bool,
) -> io::Result<()> {
   let mut bytes = Vec::new();
   io::stdin().read_to_end(&mut bytes)?;
   if derivation {
      bytes = serde_json::from_slice::<Derivation>(&bytes)?
         .aterm()
         .into_bytes();
   }
   let path = add_text(
      connection,
      name,
      &references.iter().cloned().collect(),
      &bytes,
   )?;
   writeln!(io::stdout().lock(), "{path}")
}

pub fn run(operation: &StoreOperation) -> io::Result<()> {
   let mut connection = if var("JIG_NIX_STORE_OFFLINE").is_ok_and(|value| !value.is_empty()) {
      None
   } else {
      Some(Connection::connect()?)
   };
   match *operation {
      StoreOperation::AddText {
         ref name,
         ref references,
      } => add_from_stdin(&mut connection, name, references, false)?,
      StoreOperation::AddDrv {
         ref name,
         ref references,
      } => add_from_stdin(&mut connection, name, references, true)?,
      StoreOperation::Fetchurls => {
         let files = serde_json::from_reader::<_, Vec<FetchFile>>(io::stdin().lock())?;
         let mut results = Vec::new();
         for file in files {
            let derivation = file.derivation();
            let name = format!("{}.drv", file.name);
            let drv = add_text(
               &mut connection,
               &name,
               &BTreeSet::new(),
               derivation.aterm().as_bytes(),
            )?;
            let out = derivation
               .outputs
               .into_values()
               .next()
               .ok_or_else(|| io::Error::other("fetchurl has no outputs"))?
               .path;
            results.push(Realisation { drv, out });
         }
         serde_json::to_writer(io::stdout().lock(), &results)?;
         writeln!(io::stdout().lock())?;
      },
      StoreOperation::Submit {
         ref path,
         ref output,
      } => {
         if let Some(daemon) = connection.as_mut() {
            daemon.submit(path, output)?;
         }
      },
   }
   Ok(())
}
