//! purl type -> candidates `{version date prerelease tag? url? sha256? …}`.
//!
//! Each source fills what it knows, `versions` completes date and prerelease.
//! Every GET goes through the http cache.

use std::{
   io,
   time::Duration,
};

use percent_encoding::{
   NON_ALPHANUMERIC,
   utf8_percent_encode,
};
use regex_lite::Regex;
use serde_json::{
   Map,
   Value,
   json,
};

use crate::{
   http,
   purl::Purl,
   version,
};

pub type Candidate = Map<String, Value>;

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;

fn text<'value>(candidate: &'value Candidate, key: &str) -> Option<&'value str> {
   candidate.get(key).and_then(Value::as_str)
}

pub fn version_of(candidate: &Candidate) -> &str {
   text(candidate, "version").unwrap_or_default()
}

pub fn prerelease(candidate: &Candidate) -> bool {
   candidate
      .get("prerelease")
      .and_then(Value::as_bool)
      .unwrap_or(false)
}

pub fn date(candidate: &Candidate) -> Option<&str> {
   text(candidate, "date")
}

fn fetch(url: &str, what: &str, max_age: Duration) -> io::Result<http::Response> {
   let response = http::cached(url, max_age)?;
   if response.status != 200 {
      return Err(io::Error::other(format!(
         "{what}: GET {url} -> {}",
         response.status
      )));
   }
   Ok(response)
}

fn fetch_json(url: &str, what: &str, max_age: Duration) -> io::Result<Value> {
   fetch(url, what, max_age)?.json(what)
}

const fn ten_minutes() -> Duration {
   Duration::from_secs(10 * MINUTE)
}

/// All versions the purl's datasource lists, dated and flagged.
pub fn versions(purl: &Purl) -> io::Result<Vec<Candidate>> {
   let found = match purl.kind.as_str() {
      "github" => github(purl)?,
      "gitlab" => gitlab(purl)?,
      "pypi" => pypi(purl)?,
      "cargo" => crates(purl)?,
      "npm" => npm(purl)?,
      "hackage" => hackage(purl)?,
      "gnu" => {
         listing(
            &format!("https://ftp.gnu.org/gnu/{}/", purl.name),
            &purl.name,
            None,
         )?
      },
      "generic" => generic(purl)?,
      "visualstudio" => visualstudio(purl)?,
      "applesdk" => applesdk(purl)?,
      other => {
         return Err(io::Error::other(format!(
            "no datasource for purl type {other}"
         )));
      },
   };
   Ok(found
      .into_iter()
      .map(|mut candidate| {
         let flagged = prerelease(&candidate) || version::is_prerelease(version_of(&candidate));
         candidate.insert("prerelease".to_owned(), Value::Bool(flagged));
         if !candidate.contains_key("date") {
            candidate.insert("date".to_owned(), Value::Null);
         }
         candidate
      })
      .filter(|candidate| version_of(candidate).starts_with(|ch: char| ch.is_ascii_digit()))
      .collect())
}

fn candidate(value: Value) -> Candidate {
   if let Value::Object(map) = value {
      map
   } else {
      Map::new()
   }
}

fn array(value: &Value) -> &[Value] {
   value.as_array().map_or(&[], Vec::as_slice)
}

fn str_at<'value>(value: &'value Value, key: &str) -> Option<&'value str> {
   value.get(key).and_then(Value::as_str)
}

/// Releases if the project publishes any, else tags.
fn github(purl: &Purl) -> io::Result<Vec<Candidate>> {
   let repo = format!(
      "https://api.github.com/repos/{}/{}",
      purl.namespace, purl.name
   );
   let releases = fetch_json(
      &format!("{repo}/releases?per_page=20"),
      &repo,
      ten_minutes(),
   )?;
   let found: Vec<Candidate> = array(&releases)
      .iter()
      .filter(|release| {
         !release
            .get("draft")
            .and_then(Value::as_bool)
            .unwrap_or(false)
      })
      .map(|release| {
         let tag = str_at(release, "tag_name").unwrap_or_default();
         candidate(json!({
            "version": version::from_tag(tag),
            "date": release.get("published_at").cloned().unwrap_or(Value::Null),
            "prerelease": release.get("prerelease").and_then(Value::as_bool).unwrap_or(false),
            "tag": tag,
         }))
      })
      .collect();
   if !found.is_empty() {
      return Ok(found);
   }
   let tags = fetch_json(&format!("{repo}/tags?per_page=100"), &repo, ten_minutes())?;
   Ok(array(&tags)
      .iter()
      .map(|tag| {
         let name = str_at(tag, "name").unwrap_or_default();
         candidate(json!({ "version": version::from_tag(name), "tag": name }))
      })
      .collect())
}

/// `pkg:gitlab/<ns>/<name>[?repository_url=https://gitlab.example.org]`.
fn gitlab(purl: &Purl) -> io::Result<Vec<Candidate>> {
   let host = purl
      .qualifiers
      .get("repository_url")
      .map_or("https://gitlab.com", String::as_str);
   let id = utf8_percent_encode(
      &format!("{}/{}", purl.namespace, purl.name),
      NON_ALPHANUMERIC,
   )
   .to_string();
   let tags = fetch_json(
      &format!("{host}/api/v4/projects/{id}/repository/tags?per_page=100"),
      &purl.name,
      ten_minutes(),
   )?;
   Ok(array(&tags)
      .iter()
      .map(|tag| {
         let name = str_at(tag, "name").unwrap_or_default();
         candidate(json!({
            "version": version::from_tag(name),
            "date": tag.get("commit").and_then(|commit| commit.get("created_at")).cloned().unwrap_or(Value::Null),
            "tag": name,
         }))
      })
      .collect())
}

fn pypi(purl: &Purl) -> io::Result<Vec<Candidate>> {
   let doc = fetch_json(
      &format!("https://pypi.org/pypi/{}/json", purl.name),
      &purl.name,
      ten_minutes(),
   )?;
   let Some(releases) = doc.get("releases").and_then(Value::as_object) else {
      return Ok(Vec::new());
   };
   Ok(releases
      .iter()
      .filter_map(|(release, files)| {
         let kept: Vec<&Value> = array(files)
            .iter()
            .filter(|file| !file.get("yanked").and_then(Value::as_bool).unwrap_or(false))
            .collect();
         let first = kept.first()?;
         let sdist = kept
            .iter()
            .find(|file| str_at(file, "packagetype") == Some("sdist"));
         Some(candidate(json!({
            "version": release,
            "date": first.get("upload_time_iso_8601").cloned().unwrap_or(Value::Null),
            "url": sdist.and_then(|file| file.get("url")).cloned().unwrap_or(Value::Null),
            "sha256": sdist
               .and_then(|file| file.get("digests"))
               .and_then(|digests| digests.get("sha256"))
               .cloned()
               .unwrap_or(Value::Null),
         })))
      })
      .collect())
}

fn crates(purl: &Purl) -> io::Result<Vec<Candidate>> {
   let doc = fetch_json(
      &format!("https://crates.io/api/v1/crates/{}/versions", purl.name),
      &purl.name,
      ten_minutes(),
   )?;
   Ok(array(doc.get("versions").unwrap_or(&Value::Null))
      .iter()
      .filter(|entry| {
         !entry
            .get("yanked")
            .and_then(Value::as_bool)
            .unwrap_or(false)
      })
      .map(|entry| {
         candidate(json!({
            "version": str_at(entry, "num").unwrap_or_default(),
            "date": entry.get("created_at").cloned().unwrap_or(Value::Null),
         }))
      })
      .collect())
}

fn npm(purl: &Purl) -> io::Result<Vec<Candidate>> {
   let name = [purl.namespace.as_str(), purl.name.as_str()]
      .iter()
      .filter(|part| !part.is_empty())
      .copied()
      .collect::<Vec<_>>()
      .join("/");
   let doc = fetch_json(
      &format!("https://registry.npmjs.org/{}", name.replace('/', "%2f")),
      &name,
      ten_minutes(),
   )?;
   let Some(versions) = doc.get("versions").and_then(Value::as_object) else {
      return Ok(Vec::new());
   };
   Ok(versions
      .keys()
      .map(|release| {
         candidate(json!({
            "version": release,
            "date": doc.get("time").and_then(|time| time.get(release)).cloned().unwrap_or(Value::Null),
         }))
      })
      .collect())
}

fn hackage(purl: &Purl) -> io::Result<Vec<Candidate>> {
   let doc = fetch_json(
      &format!(
         "https://hackage.haskell.org/package/{}/preferred",
         purl.name
      ),
      &purl.name,
      ten_minutes(),
   )?;
   Ok(array(doc.get("normal-version").unwrap_or(&Value::Null))
      .iter()
      .filter_map(Value::as_str)
      .map(|release| candidate(json!({ "version": release })))
      .collect())
}

/// A directory index or download page: `<name>-<version>.tar.*` links, the
/// date from the same line if any.
fn listing(url: &str, name: &str, pattern: Option<&str>) -> io::Result<Vec<Candidate>> {
   let default = format!(
      r#"(?:^|[/">]){}-(\d[0-9a-z.]*?)\.(?:tar\.(?:xz|lz|bz2|gz|zst)|zip|tgz)"#,
      regex_lite::escape(name)
   );
   let matcher = Regex::new(pattern.unwrap_or(&default)).map_err(io::Error::other)?;
   let dated = Regex::new(r"(\d{4}-\d{2}-\d{2})").map_err(io::Error::other)?;
   let page = fetch(url, name, Duration::from_secs(6 * HOUR))?.body;
   let mut found: Vec<Candidate> = Vec::new();
   for line in page.lines() {
      let Some(hit) = matcher.captures(line) else {
         continue;
      };
      let Some(release) = hit.get(1).map(|group| group.as_str()) else {
         continue;
      };
      if found.iter().any(|seen| version_of(seen) == release) {
         continue;
      }
      let day = dated
         .captures(line)
         .and_then(|stamp| stamp.get(1))
         .map(|group| group.as_str());
      found.push(candidate(json!({ "version": release, "date": day })));
   }
   Ok(found)
}

/// `pkg:generic/<name>?url=…[&regex=…]`.
fn generic(purl: &Purl) -> io::Result<Vec<Candidate>> {
   let Some(url) = purl.qualifiers.get("url") else {
      return Err(io::Error::other(format!(
         "pkg:generic/{} needs ?url=",
         purl.name
      )));
   };
   listing(
      url,
      &purl.name,
      purl.qualifiers.get("regex").map(String::as_str),
   )
}

/// `pkg:visualstudio/<major>`: the release channel names the current
/// VisualStudio.vsman. Its URL is not derivable from the version, so the path
/// below download/pr/ rides in [pin] as `pr`.
fn visualstudio(purl: &Purl) -> io::Result<Vec<Candidate>> {
   let doc = fetch_json(
      &format!("https://aka.ms/vs/{}/release/channel", purl.name),
      "visualstudio channel",
      ten_minutes(),
   )?;
   let Some(manifest) = array(doc.get("channelItems").unwrap_or(&Value::Null))
      .iter()
      .find(|item| str_at(item, "type") == Some("Manifest"))
   else {
      return Err(io::Error::other("visualstudio channel: no Manifest item"));
   };
   let payload = manifest
      .get("payloads")
      .and_then(|payloads| payloads.get(0))
      .and_then(|payload| payload.get("url"))
      .and_then(Value::as_str)
      .unwrap_or_default();
   let pr = Regex::new("/download/pr/(.+)/[^/]+$")
      .map_err(io::Error::other)?
      .captures(payload)
      .and_then(|hit| hit.get(1))
      .map(|group| group.as_str().to_owned())
      .unwrap_or_default();
   Ok(vec![candidate(json!({
      "version": str_at(manifest, "version").unwrap_or_default(),
      "pr": pr,
   }))])
}

const SUCATALOG: &str = "https://swscan.apple.com/content/catalogs/others/index-26-15-14-13-12-10.16-10.15-10.14-10.13-10.12-10.11-10.10-10.9-mountainlion-lion-snowleopard-leopard.merged-1.sucatalog";

/// `pkg:applesdk/CLTools_macOSNMOS_SDK`: every product in the macOS software
/// update catalog that ships that package. The version is the Command Line
/// Tools release in its .pkm metadata, `path` (the part of the URL after
/// content/downloads/) rides in [pin] since it is not derivable.
fn applesdk(purl: &Purl) -> io::Result<Vec<Candidate>> {
   let catalog = fetch(SUCATALOG, "apple sucatalog", Duration::from_secs(12 * HOUR))?.body;
   let listed = Regex::new(&format!(
      r"<string>https://swcdn\.apple\.com/content/downloads/([^<]+)/{}\.pkg</string>",
      regex_lite::escape(&purl.name)
   ))
   .map_err(io::Error::other)?;
   let versioned =
      Regex::new(r#"pkg-info[^>]* version="(\d+\.\d+)[.\d]*""#).map_err(io::Error::other)?;
   let mut paths: Vec<&str> = Vec::new();
   for hit in listed.captures_iter(&catalog) {
      if let Some(path) = hit.get(1).map(|group| group.as_str())
         && !paths.contains(&path)
      {
         paths.push(path);
      }
   }
   let mut found = Vec::new();
   for path in paths {
      let pkm = fetch(
         &format!(
            "https://swdist.apple.com/content/downloads/{path}/{}.pkm",
            purl.name
         ),
         &purl.name,
         Duration::from_secs(12 * HOUR),
      )?
      .body;
      if let Some(release) = versioned
         .captures(&pkm)
         .and_then(|hit| hit.get(1))
         .map(|group| group.as_str())
      {
         found.push(candidate(json!({ "version": release, "path": path })));
      }
   }
   Ok(found)
}
