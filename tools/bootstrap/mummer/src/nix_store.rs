use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   env::var,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
      Write as _,
   },
   io::{
      self,
      BufReader,
      Read as _,
      Write as _,
   },
   os::unix::net::UnixStream,
};

use mummer::{
   hash::{
      Algorithm,
      Digest,
      OutputHash,
      Spelling,
   },
   process::diagnostic,
   store::STORE_DIR,
   store_path::{
      DrvPath,
      StorePath,
   },
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
pub struct Output {
   #[serde(default)]
   pub path:      String,
   #[serde(default, rename = "hashAlgo")]
   pub algorithm: String,
   #[serde(default)]
   pub hash:      String,
}

#[derive(Default, Deserialize)]
pub struct Derivation {
   #[serde(default)]
   pub outputs: BTreeMap<String, Output>,
   #[serde(default, rename = "inputDrvs")]
   pub inputs:  BTreeMap<DrvPath, BTreeSet<String>>,
   #[serde(default, rename = "inputSrcs")]
   pub sources: BTreeSet<StorePath>,
   pub system:  String,
   pub builder: String,
   #[serde(default)]
   pub args:    Vec<String>,
   #[serde(default)]
   pub env:     BTreeMap<String, String>,
}

/// The output modes of the floating content-addressed derivations producers
/// write, as a derivation's `hashAlgo` spells them.
#[derive(Clone, Copy)]
pub enum Floating {
   Recursive,
   Text,
}

impl Display for Floating {
   #[expect(
      clippy::renamed_function_params,
      reason = "formatter describes the argument"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> FormatResult {
      formatter.write_str(match *self {
         Self::Recursive => "r:sha256",
         Self::Text => "text:sha256",
      })
   }
}

impl Output {
   pub fn floating(mode: Floating) -> Self {
      Self {
         algorithm: mode.to_string(),
         ..Self::default()
      }
   }
}

/// A string in `ATerm` quoting.
struct Quoted<'text>(&'text str);

impl Display for Quoted<'_> {
   #[expect(
      clippy::renamed_function_params,
      reason = "formatter describes the argument"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> FormatResult {
      formatter.write_char('"')?;
      for character in self.0.chars() {
         match character {
            '"' => formatter.write_str("\\\"")?,
            '\\' => formatter.write_str("\\\\")?,
            '\n' => formatter.write_str("\\n")?,
            '\r' => formatter.write_str("\\r")?,
            '\t' => formatter.write_str("\\t")?,
            other => formatter.write_char(other)?,
         }
      }
      formatter.write_char('"')
   }
}

/// `items` comma-separated, each written by `item`.
fn list<Item>(
   formatter: &mut Formatter<'_>,
   items: impl IntoIterator<Item = Item>,
   mut item: impl FnMut(&mut Formatter<'_>, Item) -> FormatResult,
) -> FormatResult {
   for (index, each) in items.into_iter().enumerate() {
      if index > 0 {
         formatter.write_char(',')?;
      }
      item(formatter, each)?;
   }
   Ok(())
}

/// The derivation in `ATerm`, what its store path hashes.
impl Display for Derivation {
   #[expect(
      clippy::renamed_function_params,
      reason = "formatter describes the argument"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> FormatResult {
      formatter.write_str("Derive([")?;
      list(formatter, &self.outputs, |out, (name, spec)| {
         write!(
            out,
            "({},{},{},{})",
            Quoted(name),
            Quoted(&spec.path),
            Quoted(&spec.algorithm),
            Quoted(&spec.hash)
         )
      })?;
      formatter.write_str("],[")?;
      list(formatter, &self.inputs, |out, (path, names)| {
         write!(out, "({},[", Quoted(path.as_str()))?;
         list(out, names, |inner, name| write!(inner, "{}", Quoted(name)))?;
         out.write_str("])")
      })?;
      formatter.write_str("],[")?;
      list(formatter, &self.sources, |out, source| {
         write!(out, "{}", Quoted(source.as_str()))
      })?;
      write!(
         formatter,
         "],{},{},[",
         Quoted(&self.system),
         Quoted(&self.builder)
      )?;
      list(formatter, &self.args, |out, arg| {
         write!(out, "{}", Quoted(arg))
      })?;
      formatter.write_str("],[")?;
      list(formatter, &self.env, |out, (key, value)| {
         write!(out, "({},{})", Quoted(key), Quoted(value))
      })?;
      formatter.write_str("])")
   }
}

fn store_path(kind: &str, name: &str, contents: &[u8]) -> io::Result<StorePath> {
   let fingerprint = format!(
      "{kind}:sha256:{}:{STORE_DIR}:{name}",
      Digest::sha256(contents).hex()
   );
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
   format!("{STORE_DIR}/{encoded}-{name}").try_into()
}

pub struct FetchFile {
   pub name: String,
   pub url:  String,
   pub hash: OutputHash,
}

/// One file of `nix-store fetchurls`, the hash in `sri` either SRI or hex in
/// `algo`.
#[derive(Deserialize)]
struct FetchRequest {
   name: String,
   url:  String,
   algo: String,
   hex:  String,
   sri:  String,
}

impl TryFrom<FetchRequest> for FetchFile {
   type Error = io::Error;

   fn try_from(request: FetchRequest) -> io::Result<Self> {
      let algorithm = request.algo.parse::<Algorithm>()?;
      let hash = if request.sri.starts_with(&format!("{algorithm}-")) {
         OutputHash::sri(request.sri.parse()?)
      } else {
         OutputHash::hex(algorithm, &request.sri)?
      };
      if hash.digest().hex() != request.hex.to_ascii_lowercase() {
         return Err(io::Error::other(format!(
            "{}: hex {} is not the digest {hash}",
            request.name, request.hex
         )));
      }
      Ok(Self {
         name: request.name,
         url: request.url,
         hash,
      })
   }
}

impl FetchFile {
   fn derivation(&self) -> io::Result<Derivation> {
      let digest = self.hash.digest();
      let algorithm = digest.algorithm().to_string();
      let hex = digest.hex();
      let path = store_path(
         "output:out",
         &self.name,
         format!("fixed:out:{algorithm}:{hex}:").as_bytes(),
      )?
      .to_string();
      let output_hash = self.hash.to_string();
      let environment = [
         ("name", self.name.as_str()),
         ("out", path.as_str()),
         ("outputHash", output_hash.as_str()),
         ("outputHashAlgo", match self.hash.spelling() {
            Spelling::Sri => "",
            Spelling::LowerHex | Spelling::UpperHex => algorithm.as_str(),
         }),
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
         algorithm,
         hash: hex,
      };
      Ok(Derivation {
         outputs: BTreeMap::from([("out".to_owned(), output)]),
         system: "builtin".to_owned(),
         builder: "builtin:fetchurl".to_owned(),
         env: environment,
         ..Derivation::default()
      })
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
      references: &BTreeSet<StorePath>,
      bytes: &[u8],
   ) -> io::Result<StorePath> {
      self.word(7);
      self.string(name.as_bytes())?;
      self.string(b"text:sha256")?;
      self.word(references.len().try_into().map_err(io::Error::other)?);
      for reference in references {
         self.string(reference.as_str().as_bytes())?;
      }
      self.word(0);
      self.word(bytes.len().try_into().map_err(io::Error::other)?);
      self.output.extend_from_slice(bytes);
      self.word(0);
      self.flush()?;
      self.drain_logs()?;
      let path = self.read_string()?.try_into()?;
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

   fn submit(&mut self, path: &DrvPath, output: &str) -> io::Result<()> {
      self.word(1000);
      self.word(0);
      self.string(path.as_str().as_bytes())?;
      self.string(output.as_bytes())?;
      self.flush()?;
      self.drain_logs()?;
      self.read_word()?;
      Ok(())
   }
}

#[derive(Serialize)]
pub struct Realisation {
   pub drv: DrvPath,
   pub out: StorePath,
}

/// the daemon behind `NIX_REMOTE`, or path arithmetic alone under
/// `MUMMER_NIX_STORE_OFFLINE`.
pub struct Store {
   connection: Option<Connection>,
}

impl Store {
   pub fn open() -> io::Result<Self> {
      let connection = if var("MUMMER_NIX_STORE_OFFLINE").is_ok_and(|value| !value.is_empty()) {
         None
      } else {
         Some(Connection::connect()?)
      };
      Ok(Self { connection })
   }

   pub fn add_text(
      &mut self,
      name: &str,
      references: &BTreeSet<StorePath>,
      bytes: &[u8],
   ) -> io::Result<StorePath> {
      self.connection.as_mut().map_or_else(
         || {
            let mut kind = "text".to_owned();
            for reference in references {
               kind.push(':');
               kind.push_str(reference.as_str());
            }
            store_path(&kind, name, bytes)
         },
         |daemon| daemon.add_text(name, references, bytes),
      )
   }

   pub fn add_derivation(
      &mut self,
      name: &str,
      references: &BTreeSet<StorePath>,
      derivation: &Derivation,
   ) -> io::Result<DrvPath> {
      self
         .add_text(name, references, derivation.to_string().as_bytes())?
         .try_into()
   }

   pub fn fetchurls(&mut self, files: &[FetchFile]) -> io::Result<Vec<Realisation>> {
      let mut results = Vec::with_capacity(files.len());
      for file in files {
         let derivation = file.derivation()?;
         let name = format!("{}.drv", file.name);
         let drv = self.add_derivation(&name, &BTreeSet::new(), &derivation)?;
         let out = derivation
            .outputs
            .into_values()
            .next()
            .ok_or_else(|| io::Error::other("fetchurl has no outputs"))?
            .path
            .try_into()?;
         results.push(Realisation { drv, out });
      }
      Ok(results)
   }

   pub fn submit(&mut self, path: &DrvPath, output: &str) -> io::Result<()> {
      self
         .connection
         .as_mut()
         .map_or(Ok(()), |daemon| daemon.submit(path, output))
   }
}

fn add_from_stdin(
   store: &mut Store,
   name: &str,
   references: &[String],
   derivation: bool,
) -> io::Result<()> {
   let mut bytes = Vec::new();
   io::stdin().read_to_end(&mut bytes)?;
   if derivation {
      bytes = serde_json::from_slice::<Derivation>(&bytes)?
         .to_string()
         .into_bytes();
   }
   let paths = references
      .iter()
      .map(|reference| reference.parse())
      .collect::<io::Result<BTreeSet<StorePath>>>()?;
   let path = store.add_text(name, &paths, &bytes)?;
   writeln!(io::stdout().lock(), "{path}")
}

pub fn run(operation: &StoreOperation) -> io::Result<()> {
   let mut store = Store::open()?;
   match *operation {
      StoreOperation::AddText {
         ref name,
         ref references,
      } => add_from_stdin(&mut store, name, references, false)?,
      StoreOperation::AddDrv {
         ref name,
         ref references,
      } => add_from_stdin(&mut store, name, references, true)?,
      StoreOperation::Fetchurls => {
         let files = serde_json::from_reader::<_, Vec<FetchRequest>>(io::stdin().lock())?
            .into_iter()
            .map(FetchFile::try_from)
            .collect::<io::Result<Vec<_>>>()?;
         let results = store.fetchurls(&files)?;
         serde_json::to_writer(io::stdout().lock(), &results)?;
         writeln!(io::stdout().lock())?;
      },
      StoreOperation::Submit {
         ref path,
         ref output,
      } => store.submit(&path.parse()?, output)?,
   }
   Ok(())
}
