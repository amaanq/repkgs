use std::{
   collections::BTreeSet,
   fs::read_to_string,
   io,
};

use mummer::{
   hash::{
      Algorithm,
      OutputHash,
   },
   store_path::{
      DrvPath,
      StorePath,
   },
};
use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Assembly,
         Row,
         attr,
         collect_with,
         fetchurls,
         keys,
         say,
         store_attr,
      },
      registry::basename,
   },
   nix_store::Store,
};

const MSIS: [&str; 9] = [
   "Windows SDK Desktop Headers x86",
   "Windows SDK Desktop Headers <arch>",
   "Windows SDK Desktop Libs <arch>",
   "Windows SDK OnecoreUap Headers x86",
   "Windows SDK OnecoreUap Headers <arch>",
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
   #[serde(default, deserialize_with = "keys")]
   dependencies: BTreeSet<String>,
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

/// What `winsdk-assemble` finds beside the layout.
#[derive(Serialize)]
struct Assembled {
   sevenzip: StorePath,
   payloads: Vec<Selected>,
}

/// What `winsdk-assemble` does with a payload.
#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PayloadKind {
   Crt,
   Msi,
   Cab,
}

#[derive(Serialize)]
struct Selected {
   file: String,
   kind: PayloadKind,
   out:  StorePath,
}

/// The CPUs Microsoft ships an SDK for, as the SDK and the CRT spell them.
#[derive(Clone, Copy)]
enum WinArch {
   X64,
   Arm64,
}

impl WinArch {
   const fn sdk(self) -> &'static str {
      match self {
         Self::X64 => "x64",
         Self::Arm64 => "arm64",
      }
   }

   const fn crt(self) -> &'static str {
      match self {
         Self::X64 => "x64",
         Self::Arm64 => "ARM64",
      }
   }
}

impl TryFrom<&str> for WinArch {
   type Error = io::Error;

   fn try_from(cpu: &str) -> io::Result<Self> {
      match cpu {
         "x86_64" => Ok(Self::X64),
         "aarch64" => Ok(Self::Arm64),
         _ => {
            Err(io::Error::other(format!(
               "windows-sdk: Microsoft ships no {cpu} SDK"
            )))
         },
      }
   }
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
         .iter()
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
      format!("Redist.{}", crt_arch.to_uppercase()),
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
            file_name: basename(&payload.file_name.replace('\\', "/")).to_owned(),
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
      let arch = WinArch::try_from(cpu.as_str())?;
      let sdk_arch = arch.sdk();
      let manifest: Manifest = serde_json::from_str(&read_to_string(attr("manifest")?)?)?;
      let packages: Vec<Package> = manifest
         .packages
         .into_iter()
         .filter(|candidate| candidate.language.is_none())
         .collect();
      let (crt, vsix) = select_crt(&packages, arch.crt())?;
      let mut payloads: Vec<(Payload, PayloadKind)> = vsix
         .into_iter()
         .map(|payload| (payload, PayloadKind::Crt))
         .collect();
      let (sdk_id, sdk) = select_sdk(&packages)?;
      for name in MSIS {
         let wanted = format!("{}-x86_en-us.msi", name.replace("<arch>", sdk_arch));
         let msi = sdk
            .iter()
            .find(|payload| payload.file_name == wanted)
            .ok_or_else(|| io::Error::other(format!("windows-sdk: {sdk_id} has no {wanted}")))?;
         payloads.push((msi.clone(), PayloadKind::Msi));
      }
      for cab in sdk
         .iter()
         .filter(|payload| payload.file_name.strip_suffix(".cab").is_some())
      {
         payloads.push((cab.clone(), PayloadKind::Cab));
      }
      let rows: Vec<Row> = payloads
         .iter()
         .map(|selected| {
            Ok(Row {
               file: selected.0.file_name.clone(),
               url:  selected.0.url.clone(),
               hash: OutputHash::hex(Algorithm::Sha256, &selected.0.sha256)?,
            })
         })
         .collect::<io::Result<Vec<_>>>()?;
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
      let mut inputs: Vec<DrvPath> = fetched.into_iter().map(|made| made.drv).collect();
      inputs.push(attr("sevenzip_drv")?.try_into()?);
      collect_with(
         store,
         &format!("windows-sdk-{cpu}"),
         &[],
         &inputs,
         Assembly {
            script: "winsdk-assemble",
            attrs:  Assembled {
               sevenzip: store_attr("sevenzip")?,
               payloads: selected,
            },
            srcs:   &[],
         },
      )
   }
}
