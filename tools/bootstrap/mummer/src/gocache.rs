use std::{
   collections::BTreeMap,
   env::var_os,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   fs::create_dir_all,
   io::{
      self,
      BufRead,
      BufReader,
      Read as _,
      Write,
   },
   path::{
      Path,
      PathBuf,
   },
   time::Instant,
};

use base64::{
   Engine as _,
   engine::general_purpose::STANDARD,
};
use mummer::{
   files::write_file,
   process::{
      diagnostic,
      log,
   },
};
use serde::{
   Deserialize,
   Serialize,
};
use tightrope::{
   MAX_OBJECT,
   client::Client,
   key::{
      Hex,
      Key,
      Namespace,
   },
};

#[derive(Deserialize)]
struct Request {
   #[serde(rename = "ID")]
   id:      u64,
   #[serde(flatten)]
   command: RequestCommand,
}

#[derive(Deserialize)]
#[serde(tag = "Command")]
enum RequestCommand {
   #[serde(rename = "get")]
   Get {
      #[serde(rename = "ActionID")]
      action: GoId,
   },
   #[serde(rename = "put")]
   Put {
      #[serde(rename = "ActionID")]
      action: GoId,
      #[serde(rename = "OutputID")]
      output: GoId,
      #[serde(default, rename = "BodySize")]
      size:   usize,
   },
   #[serde(rename = "close")]
   Close,
}

#[derive(Serialize)]
struct Response {
   #[serde(rename = "ID")]
   id:   u64,
   #[serde(flatten)]
   body: ResponseBody,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ResponseBody {
   Greeting {
      #[serde(rename = "KnownCommands")]
      commands: [&'static str; 3],
   },
   Miss {
      #[serde(rename = "Miss")]
      miss: bool,
   },
   Hit {
      #[serde(rename = "OutputID")]
      output: String,
      #[serde(rename = "Size")]
      size:   usize,
      #[serde(rename = "DiskPath")]
      path:   PathBuf,
   },
   Stored {
      #[serde(rename = "DiskPath")]
      path: PathBuf,
   },
   Torn {
      #[serde(rename = "Err")]
      err: String,
   },
   Closed(BTreeMap<String, String>),
}

/// An action or output ID, 32 bytes Go sends in base64.
#[derive(Clone, Copy, Deserialize)]
#[serde(try_from = "String")]
struct GoId([u8; 32]);

impl GoId {
   fn encoded(self) -> String {
      STANDARD.encode(self.0)
   }
}

impl TryFrom<&[u8]> for GoId {
   type Error = io::Error;

   fn try_from(bytes: &[u8]) -> io::Result<Self> {
      bytes
         .try_into()
         .map(Self)
         .map_err(|_error| io::Error::other("Go cache identifier must contain 32 bytes"))
   }
}

impl TryFrom<String> for GoId {
   type Error = io::Error;

   fn try_from(encoded: String) -> io::Result<Self> {
      Self::try_from(
         STANDARD
            .decode(encoded)
            .map_err(io::Error::other)?
            .as_slice(),
      )
   }
}

impl Display for GoId {
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      Hex(&self.0).fmt(f)
   }
}

fn next_line(input: &mut impl BufRead, limit: usize) -> io::Result<Option<String>> {
   let mut line = String::new();
   let limit_u64 = u64::try_from(limit).map_err(io::Error::other)?;
   let count = input.take(limit_u64 + 1).read_line(&mut line)?;
   if count == 0 {
      return Ok(None);
   }
   if u64::try_from(count).map_err(io::Error::other)? > limit_u64 || !line.ends_with('\n') {
      return Err(io::Error::other(
         "unterminated or oversized Go cache message",
      ));
   }
   Ok(Some(line))
}

fn reply(output: &mut impl Write, response: &Response) -> io::Result<()> {
   serde_json::to_writer(&mut *output, response)?;
   output.write_all(b"\n")?;
   output.flush()
}

#[expect(
   clippy::too_many_lines,
   reason = "loop covers protocol handling and request dispatch in one place"
)]
pub fn run(socket: &Path) -> io::Result<()> {
   let mut cache = match Client::connect(socket) {
      Ok(client) => Some(client),
      Err(error) => {
         diagnostic(format_args!("mummer Go cache daemon unavailable, {error}"));
         None
      },
   };
   let directory = var_os("GOCACHEPROG_DIR").map_or_else(
      || {
         var_os("TMPDIR")
            .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
            .join("gocacheprog")
      },
      PathBuf::from,
   );
   create_dir_all(&directory)?;
   let mut input = BufReader::new(io::stdin());
   let stdout = io::stdout();
   let mut writer = stdout.lock();
   reply(&mut writer, &Response {
      id:   0,
      body: ResponseBody::Greeting {
         commands: ["get", "put", "close"],
      },
   })?;
   while let Some(line) = next_line(&mut input, 0x10000)? {
      if line.trim().is_empty() {
         continue;
      }
      let request = serde_json::from_str::<Request>(&line)?;
      let started = Instant::now();
      let closing = matches!(request.command, RequestCommand::Close);
      let body = match request.command {
         RequestCommand::Close => ResponseBody::Closed(BTreeMap::new()),
         RequestCommand::Get { action } => {
            let found = cache
               .as_mut()
               .map(|client| -> io::Result<_> {
                  let Some(output_bytes) = client.get(&Key::hex(Namespace::GoAction, &action.0))?
                  else {
                     return Ok(None);
                  };
                  let output = GoId::try_from(output_bytes.as_slice())?;
                  Ok(client
                     .get(&Key::hex(Namespace::GoOutput, &output.0))?
                     .map(|bytes| (output, bytes)))
               })
               .transpose();
            match found {
               Ok(Some(Some((output, bytes)))) => {
                  let path = directory.join(output.to_string());
                  write_file(&path, &bytes, None)?;
                  log("go", "cached", &action.to_string(), started);
                  ResponseBody::Hit {
                     output: output.encoded(),
                     size: bytes.len(),
                     path,
                  }
               },
               Ok(_) => ResponseBody::Miss { miss: true },
               Err(error) => {
                  diagnostic(format_args!("mummer Go cache lookup failed, {error}"));
                  cache = None;
                  ResponseBody::Miss { miss: true }
               },
            }
         },
         RequestCommand::Put {
            action,
            output,
            size,
         } => {
            if size > MAX_OBJECT {
               return Err(io::Error::other("Go cache body exceeds size limit"));
            }
            let mut bytes = Vec::new();
            if size != 0 {
               loop {
                  let body_line = next_line(&mut input, size.div_ceil(3) * 4 + 16)?
                     .ok_or_else(|| io::Error::other("Go cache body is missing"))?;
                  if body_line.trim().is_empty() {
                     continue;
                  }
                  let encoded = serde_json::from_str::<String>(&body_line)?;
                  bytes = STANDARD.decode(encoded).map_err(io::Error::other)?;
                  break;
               }
            }
            // a killed go build leaves a torn stream, storing it would poison
            // the action for every later build
            if bytes.len() == size {
               let path = directory.join(output.to_string());
               write_file(&path, &bytes, None)?;
               if let Some(client) = cache.as_mut()
                  && let Err(error) = client
                     .put(&Key::hex(Namespace::GoOutput, &output.0), &bytes)
                     .and_then(|()| {
                        client.put(&Key::hex(Namespace::GoAction, &action.0), &output.0)
                     })
               {
                  diagnostic(format_args!("mummer Go cache upload failed, {error}"));
                  cache = None;
               }
               log(
                  "go",
                  if cache.is_some() {
                     "compiled"
                  } else {
                     "compiled-unstored"
                  },
                  &action.to_string(),
                  started,
               );
               ResponseBody::Stored { path }
            } else {
               ResponseBody::Torn {
                  err: format!("body: got {} of {size} bytes", bytes.len()),
               }
            }
         },
      };
      reply(&mut writer, &Response {
         id: request.id,
         body,
      })?;
      if closing {
         break;
      }
   }
   Ok(())
}
