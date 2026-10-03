use std::{
   fs::File,
   io::{
      self,
      copy,
   },
   path::Path,
   time::Duration,
};

use mummer::unpack::unpack;

use crate::fetch::drv::{
   attr,
   say,
};

/// url and its mirrors (nix/mirrors.nix) in order, the first that answers wins.
fn download(urls: &[&str], to: &Path) -> io::Result<()> {
   let agent = ureq::Agent::config_builder()
      .timeout_global(Some(Duration::from_secs(600)))
      .build()
      .new_agent();
   for url in urls {
      let fetched = agent.get(*url).call().and_then(|mut response| {
         let mut body = response.body_mut().with_config().limit(u64::MAX).reader();
         let mut file = File::create(to)?;
         copy(&mut body, &mut file)?;
         Ok(())
      });
      match fetched {
         Ok(()) => return Ok(()),
         Err(error) => say(&format!("{url}: {error}")),
      }
   }
   Err(io::Error::other(format!(
      "no url answered: {}",
      urls.join(" ")
   )))
}

/// nix/sources.nix's fixed-output builder, the first of `$urls` that answers
/// unpacked into `$out` with `$unpacker`'s bsdtar.
pub fn run_mode() -> io::Result<()> {
   let urls = attr("urls")?;
   let out = attr("out")?;
   let unpacker = attr("unpacker")?;
   let archive = Path::new("archive");
   download(&urls.split(' ').collect::<Vec<_>>(), archive)?;
   unpack(&format!("{unpacker}/bin/bsdtar"), archive, Path::new(&out))
}
