//! Cached GET, one JSON file per URL under `$XDG_CACHE_HOME/plumb/` with
//! etag, last-modified and body, so a rerun is conditional requests answered
//! by 304 (which GitHub does not rate-limit).

use std::{
   env::var,
   fs::{
      create_dir_all,
      read_to_string,
      write,
   },
   io,
   path::PathBuf,
   process::Command,
   time::{
      Duration,
      SystemTime,
      UNIX_EPOCH,
   },
};

use serde::{
   Deserialize,
   Serialize,
};
use serde_json::Value;
use ureq::{
   Agent,
   Body,
   http::Response as HttpResponse,
};

pub struct Response {
   pub status: u16,
   pub body:   String,
}

impl Response {
   /// The body as JSON, or the error the caller's `what` names.
   pub fn json(&self, what: &str) -> io::Result<Value> {
      serde_json::from_str(&self.body)
         .map_err(|error| io::Error::other(format!("{what}: not JSON, {error}")))
   }
}

#[derive(Deserialize, Serialize)]
struct Cached {
   etag:          Option<String>,
   last_modified: Option<String>,
   fetched:       u64,
   body:          String,
}

pub fn now() -> u64 {
   SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |since| since.as_secs())
}

fn cache_file(url: &str) -> io::Result<PathBuf> {
   let base = var("XDG_CACHE_HOME").map_or_else(
      |_unset| PathBuf::from(var("HOME").unwrap_or_default()).join(".cache"),
      PathBuf::from,
   );
   let dir = base.join("plumb");
   create_dir_all(&dir)?;
   Ok(dir.join(blake3::hash(url.as_bytes()).to_hex().as_str()))
}

/// `$GITHUB_TOKEN`, else whatever `gh` is logged in with. 60 requests an hour
/// unauthenticated is about 30 packages.
fn github_token() -> Option<String> {
   if let Ok(token) = var("GITHUB_TOKEN") {
      return Some(token);
   }
   let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
   output
      .status
      .success()
      .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

struct Header {
   name:  &'static str,
   value: String,
}

fn get(agent: &Agent, url: &str, headers: &[Header]) -> io::Result<HttpResponse<Body>> {
   let mut request = agent.get(url);
   for header in headers {
      request = request.header(header.name, header.value.as_str());
   }
   request.call().map_err(io::Error::other)
}

fn host(url: &str) -> &str {
   let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
   rest.split(['/', '?', '#']).next().unwrap_or_default()
}

/// `--max-age` serves from the cache without revalidating.
pub fn cached(url: &str, max_age: Duration) -> io::Result<Response> {
   let file = cache_file(url)?;
   let old: Option<Cached> = read_to_string(&file)
      .ok()
      .and_then(|text| serde_json::from_str(&text).ok());
   if let Some(ref hit) = old
      && now().saturating_sub(hit.fetched) < max_age.as_secs()
   {
      return Ok(Response {
         status: 200,
         body:   hit.body.clone(),
      });
   }
   let mut headers = Vec::new();
   if host(url) == "api.github.com"
      && let Some(token) = github_token()
   {
      headers.push(Header {
         name:  "Authorization",
         value: format!("Bearer {token}"),
      });
   }
   if let Some(ref hit) = old {
      if let Some(ref etag) = hit.etag {
         headers.push(Header {
            name:  "If-None-Match",
            value: etag.clone(),
         });
      }
      if let Some(ref stamp) = hit.last_modified {
         headers.push(Header {
            name:  "If-Modified-Since",
            value: stamp.clone(),
         });
      }
   }
   headers.push(Header {
      name:  "User-Agent",
      value: "plumb".to_owned(),
   });
   // hackage serves html unless asked for json. github.com (not the API)
   // answers 406 to json-only
   headers.push(Header {
      name:  "Accept",
      value: "application/json, */*;q=0.5".to_owned(),
   });
   let agent = Agent::config_builder()
      .http_status_as_error(false)
      .timeout_global(Some(Duration::from_secs(120)))
      .build()
      .new_agent();
   let mut response = get(&agent, url, &headers)?;
   let mut status = response.status().as_u16();
   let mut body = response
      .body_mut()
      .with_config()
      .limit(u64::MAX)
      .read_to_string()
      .map_err(io::Error::other)?;
   // an org with SAML enforcement rejects a token not authorized for it, public
   // data is still anonymous
   if status == 403 && body.contains("SAML") {
      let anonymous: Vec<Header> = headers
         .into_iter()
         .filter(|header| header.name != "Authorization")
         .collect();
      response = get(&agent, url, &anonymous)?;
      status = response.status().as_u16();
      body = response
         .body_mut()
         .with_config()
         .limit(u64::MAX)
         .read_to_string()
         .map_err(io::Error::other)?;
   }
   if status == 304
      && let Some(mut hit) = old
   {
      hit.fetched = now();
      write(&file, serde_json::to_vec(&hit)?)?;
      return Ok(Response {
         status: 200,
         body:   hit.body,
      });
   }
   if status >= 400 {
      return Ok(Response { status, body });
   }
   let header = |name: &str| {
      response
         .headers()
         .get(name)
         .and_then(|value| value.to_str().ok())
         .map(str::to_owned)
   };
   let fresh = Cached {
      etag: header("etag"),
      last_modified: header("last-modified"),
      fetched: now(),
      body,
   };
   write(&file, serde_json::to_vec(&fresh)?)?;
   Ok(Response {
      status: 200,
      body:   fresh.body,
   })
}
