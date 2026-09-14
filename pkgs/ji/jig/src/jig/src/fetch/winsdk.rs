use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
};

use serde::{
   Deserialize,
   Serialize,
};
use serde_json::{
   Map,
   Value,
   to_value,
};

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Hash,
         Row,
         attr,
         collect,
         fetchurls,
         say,
      },
   },
   nix_store::Store,
};

const MSIS: [&str; 7] = [
   "Windows SDK Desktop Headers x86",
   "Windows SDK Desktop Libs {arch}",
   "Windows SDK OnecoreUap Headers {arch}",
   "Windows SDK for Windows Store Apps Headers",
   "Windows SDK for Windows Store Apps Headers OnecoreUap",
   "Windows SDK for Windows Store Apps Libs",
   "Universal CRT Headers Libraries and Sources",
];

#[derive(Deserialize)]
struct Manifest {
   packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
   id:           String,
   language:     Option<String>,
   #[serde(default)]
   dependencies: BTreeMap<String, Value>,
   #[serde(default)]
   payloads:     Vec<Payload>,
}

#[derive(Clone, Deserialize)]
struct Payload {
   #[serde(rename = "fileName")]
   file_name: String,
   url:       String,
   sha256:    String,
}

#[derive(Serialize)]
pub struct Selected {
   pub file: String,
   pub kind: &'static str,
   pub out:  String,
}

fn newest<'version, Versions>(versions: Versions) -> Option<&'version str>
where
   Versions: Iterator<Item = &'version str>,
{
   versions.max_by_key(|version| {
      version
         .split('.')
         .map(|part| part.parse::<u64>().unwrap_or(0))
         .collect::<Vec<_>>()
   })
}

fn package<'manifest>(packages: &'manifest [Package], id: &str) -> io::Result<&'manifest Package> {
   packages
      .iter()
      .find(|candidate| candidate.id == id)
      .ok_or_else(|| io::Error::other(format!("windows-sdk: manifest has no package {id}")))
}

fn numeric(version: &str) -> bool {
   version
      .bytes()
      .all(|byte| byte.is_ascii_digit() || byte == b'.')
}

/// the newest VC CRT as .vsix, headers with the STL, <arch> Desktop and Store
/// libs.
fn select_crt(packages: &[Package], crt_arch: &str) -> io::Result<(String, Vec<Payload>)> {
   let crt = newest(
      package(packages, "Microsoft.VisualStudio.Product.BuildTools")?
         .dependencies
         .keys()
         .filter_map(|key| {
            key.strip_prefix("Microsoft.VisualStudio.Component.VC.")?
               .strip_suffix(".x86.x64")
         })
         .filter(|version| numeric(version)),
   )
   .ok_or_else(|| io::Error::other("windows-sdk: no VC component in the manifest"))?
   .to_owned();
   let mut payloads = Vec::new();
   for kind in [
      "Headers".to_owned(),
      format!("{crt_arch}.Desktop"),
      format!("{crt_arch}.Store"),
   ] {
      let vsix = package(packages, &format!("Microsoft.VC.{crt}.CRT.{kind}.base"))?
         .payloads
         .first()
         .ok_or_else(|| io::Error::other(format!("windows-sdk: CRT {kind} has no payload")))?;
      payloads.push(vsix.clone());
   }
   Ok((crt, payloads))
}

/// the newest Win11 SDK's payloads, file names cut to their basename.
fn select_sdk(packages: &[Package]) -> io::Result<(String, Vec<Payload>)> {
   let sdk_version = newest(packages.iter().filter_map(|candidate| {
      candidate
         .id
         .strip_prefix("Win11SDK_")
         .filter(|version| numeric(version))
   }))
   .ok_or_else(|| io::Error::other("windows-sdk: no Win11SDK in the manifest"))?
   .to_owned();
   let sdk_id = format!("Win11SDK_{sdk_version}");
   let sdk = package(packages, &sdk_id)?
      .payloads
      .iter()
      .map(|payload| {
         Payload {
            file_name: payload
               .file_name
               .replace('\\', "/")
               .rsplit('/')
               .next()
               .unwrap_or_default()
               .to_owned(),
            url:       payload.url.clone(),
            sha256:    payload.sha256.clone(),
         }
      })
      .collect();
   Ok((sdk_id, sdk))
}

/// VisualStudio.vsman -> {crt,sdk}/, selected like xwin, the newest VC CRT as
/// .vsix and from the newest Win11 SDK the header/lib/UCRT .msi plus all of
/// its cabinets (an .msi names the ones it needs only inside itself, unused
/// ones cost a download, not output). `winsdk-assemble` unpacks.
pub struct Winsdk;

impl Producer for Winsdk {
   fn name(&self) -> &'static str {
      "winsdk"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let cpu = attr("arch")?;
      let sdk_arch = match cpu.as_str() {
         "x86_64" => "x64",
         "aarch64" => "arm64",
         _ => {
            return Err(io::Error::other(format!(
               "windows-sdk: Microsoft ships no {cpu} SDK"
            )));
         },
      };
      let manifest: Manifest = serde_json::from_str(&read_to_string(attr("manifest")?)?)?;
      let packages: Vec<Package> = manifest
         .packages
         .into_iter()
         .filter(|candidate| candidate.language.is_none())
         .collect();
      let crt_arch = if sdk_arch == "arm64" {
         "ARM64"
      } else {
         sdk_arch
      };
      let (crt, vsix) = select_crt(&packages, crt_arch)?;
      let mut payloads: Vec<(Payload, &'static str)> =
         vsix.into_iter().map(|payload| (payload, "crt")).collect();
      let (sdk_id, sdk) = select_sdk(&packages)?;
      for name in MSIS {
         let wanted = format!("{}-x86_en-us.msi", name.replace("{arch}", sdk_arch));
         let msi = sdk
            .iter()
            .find(|payload| payload.file_name == wanted)
            .ok_or_else(|| io::Error::other(format!("windows-sdk: {sdk_id} has no {wanted}")))?;
         payloads.push((msi.clone(), "msi"));
      }
      for cab in sdk
         .iter()
         .filter(|payload| payload.file_name.strip_suffix(".cab").is_some())
      {
         payloads.push((cab.clone(), "cab"));
      }
      let rows: Vec<Row> = payloads
         .iter()
         .map(|selected| {
            Row {
               file: selected.0.file_name.clone(),
               url:  selected.0.url.clone(),
               hash: Hash::Sha256(selected.0.sha256.clone()),
            }
         })
         .collect();
      let fetched = fetchurls(store, &rows)?;
      say(&format!("windowsSdk: CRT {crt}, {sdk_id} for {sdk_arch}"));
      let selected: Vec<Selected> = payloads
         .iter()
         .zip(&fetched)
         .map(|(selected, made)| {
            Selected {
               file: selected.0.file_name.clone(),
               kind: selected.1,
               out:  made.out.clone(),
            }
         })
         .collect();
      let mut inputs: Vec<String> = fetched.into_iter().map(|made| made.drv).collect();
      inputs.push(attr("sevenzip_drv")?);
      let mut attrs = Map::new();
      attrs.insert("sevenzip".to_owned(), Value::String(attr("sevenzip")?));
      attrs.insert("payloads".to_owned(), to_value(&selected)?);
      collect(
         store,
         &format!("windows-sdk-{cpu}"),
         &[],
         &inputs,
         Some("winsdk-assemble"),
         attrs,
      )
   }
}
