use std::{
   collections::BTreeMap,
   env::var_os,
   fmt::Write as FormatWrite,
   fs::create_dir_all,
   io::{
      self,
      BufRead,
      Read,
      Write,
   },
   path::{
      Path,
      PathBuf,
   },
   time::Instant,
};

use base64::{
   Engine,
   engine::general_purpose::STANDARD,
};
use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   cache::{
      CacheClient,
      MAX_OBJECT_SIZE,
   },
   files::write_file,
   process::{
      diagnostic,
      log,
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
      action: String,
   },
   #[serde(rename = "put")]
   Put {
      #[serde(rename = "ActionID")]
      action: String,
      #[serde(rename = "OutputID")]
      output: String,
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
   Closed(BTreeMap<String, String>),
}

fn identifier(encoded: &str) -> io::Result<(Vec<u8>, String)> {
   let bytes = STANDARD.decode(encoded).map_err(io::Error::other)?;
   if bytes.len() != 32 {
      return Err(io::Error::other(
         "Go cache identifier must contain 32 bytes",
      ));
   }
   let mut hex = String::with_capacity(64);
   for byte in &bytes {
      write!(hex, "{byte:02x}").expect("formatting a string cannot fail");
   }
   Ok((bytes, hex))
}

fn next_line(input: &mut impl BufRead, limit: usize) -> io::Result<Option<String>> {
   let mut line = String::new();
   let count = input.take(limit as u64 + 1).read_line(&mut line)?;
   if count == 0 {
      return Ok(None);
   }
   if count > limit || !line.ends_with('\n') {
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

pub fn run(socket: &Path) -> io::Result<()> {
   let mut cache = match CacheClient::connect(socket) {
      Ok(client) => Some(client),
      Err(error) => {
         diagnostic(format_args!("jig Go cache daemon unavailable, {error}"));
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
   let mut input = io::stdin().lock();
   let mut output = io::stdout().lock();
   reply(&mut output, &Response {
      id:   0,
      body: ResponseBody::Greeting {
         commands: ["get", "put", "close"],
      },
   })?;
   while let Some(line) = next_line(&mut input, 65536)? {
      if line.trim().is_empty() {
         continue;
      }
      let request: Request = serde_json::from_str(&line)?;
      let started = Instant::now();
      let closing = matches!(request.command, RequestCommand::Close);
      let body = match request.command {
         RequestCommand::Close => ResponseBody::Closed(BTreeMap::new()),
         RequestCommand::Get { action } => {
            let (_, action_hex) = identifier(&action)?;
            let found = cache
               .as_mut()
               .map(|client| -> io::Result<_> {
                  let Some(output_id) = client.get(&format!("go/a/{action_hex}"))? else {
                     return Ok(None);
                  };
                  let encoded = STANDARD.encode(output_id);
                  let (_, output_hex) = identifier(&encoded)?;
                  Ok(client
                     .get(&format!("go/o/{output_hex}"))?
                     .map(|bytes| (encoded, output_hex, bytes)))
               })
               .transpose();
            match found {
               Ok(Some(Some((encoded, output_hex, bytes)))) => {
                  let path = directory.join(output_hex);
                  write_file(&path, &bytes, None)?;
                  log("go", "cached", &action_hex, started);
                  ResponseBody::Hit {
                     output: encoded,
                     size: bytes.len(),
                     path,
                  }
               },
               Ok(_) => ResponseBody::Miss { miss: true },
               Err(error) => {
                  diagnostic(format_args!("jig Go cache lookup failed, {error}"));
                  cache = None;
                  ResponseBody::Miss { miss: true }
               },
            }
         },
         RequestCommand::Put {
            action,
            output: output_id,
            size,
         } => {
            let (_, action_hex) = identifier(&action)?;
            let (output_bytes, output_hex) = identifier(&output_id)?;
            if size > MAX_OBJECT_SIZE {
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
                  let encoded: String = serde_json::from_str(&body_line)?;
                  bytes = STANDARD.decode(encoded).map_err(io::Error::other)?;
                  break;
               }
            }
            if bytes.len() != size {
               return Err(io::Error::other("Go cache body length differs"));
            }
            let path = directory.join(&output_hex);
            write_file(&path, &bytes, None)?;
            if let Some(client) = cache.as_mut()
               && let Err(error) = client
                  .put(&format!("go/o/{output_hex}"), &bytes)
                  .and_then(|()| client.put(&format!("go/a/{action_hex}"), &output_bytes))
            {
               diagnostic(format_args!("jig Go cache upload failed, {error}"));
               cache = None;
            }
            log(
               "go",
               if cache.is_some() {
                  "compiled"
               } else {
                  "compiled-unstored"
               },
               &action_hex,
               started,
            );
            ResponseBody::Stored { path }
         },
      };
      reply(&mut output, &Response {
         id: request.id,
         body,
      })?;
      if closing {
         break;
      }
   }
   Ok(())
}
