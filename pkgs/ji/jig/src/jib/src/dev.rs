use std::{
   collections::BTreeMap,
   env::{
      current_dir,
      var,
      vars,
   },
   fs::{
      copy,
      create_dir_all,
      read_to_string,
      rename,
      write,
   },
   io,
   path::Path,
};

use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      build_top,
   },
   build,
   cli::DevOperation,
   prepare::Source,
};

#[derive(Deserialize, Serialize)]
struct Environment {
   env:   BTreeMap<String, String>,
   cwd:   String,
   unset: Vec<String>,
}

fn save(top: &str, previous: &[String]) -> io::Result<()> {
   let path = Path::new(top).join("dev-env.json");
   let mut unset = if path.try_exists()? {
      serde_json::from_str::<Environment>(&read_to_string(&path)?)?.unset
   } else {
      Vec::new()
   };
   unset.extend_from_slice(previous);
   unset.retain(|name| var(name).is_err());
   unset.sort_unstable();
   unset.dedup();
   let state = Environment {
      env: vars()
         .filter(|entry| {
            !entry.0.starts_with("NU_")
               && !matches!(
                  entry.0.as_str(),
                  "PWD" | "OLDPWD" | "CURRENT_FILE" | "FILE_PWD" | "PROCESS_PATH"
               )
         })
         .collect(),
      cwd: current_dir()?.to_string_lossy().into_owned(),
      unset,
   };
   let pending = Path::new(top).join("dev-env.tmp.json");
   write(&pending, serde_json::to_vec(&state)?)?;
   rename(pending, path)
}

pub fn run(operation: DevOperation, socket: &Path) -> io::Result<()> {
   let attrs = Attrs::load()?;
   let top = build_top()?;
   match operation {
      DevOperation::Export => {
         create_dir_all(&attrs.outputs.out)?;
         let input = var("NIX_ATTRS_JSON_FILE").map_err(io::Error::other)?;
         copy(input, Path::new(&attrs.outputs.out).join("attrs.json"))?;
         Ok(())
      },
      DevOperation::Prepare => {
         if Path::new(&top).join("dev-env.json").try_exists()? {
            return Ok(());
         }
         let source = if Path::new(&top).join("source").try_exists()? {
            Source::Existing
         } else {
            Source::Unpack
         };
         build::setup(&attrs, source, socket)?;
         save(&top, &[])
      },
      DevOperation::Phase { name } => {
         let index = attrs
            .plan
            .phases
            .iter()
            .position(|step| step.name == name)
            .ok_or_else(|| io::Error::other(format!("no build phase {name}")))?;
         let ctx = Ctx::load(&top)?;
         let previous = vars().map(|entry| entry.0).collect::<Vec<_>>();
         build::phase(&attrs, &ctx, index, socket)?;
         save(&top, &previous)
      },
   }
}
