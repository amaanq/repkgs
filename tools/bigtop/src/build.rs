use std::{
   collections::BTreeMap,
   env::{
      set_current_dir,
      vars,
   },
   fs::read_to_string,
   io,
   path::Path,
   process::Command,
   time::Instant,
};

use mummer::{
   files::write_file,
   process::Checked as _,
   tree::{
      note,
      set_phase,
   },
};
use serde_json::Value;

use crate::{
   attrs::{
      Attrs,
      Ctx,
      Kind,
      build_top,
   },
   finish,
   prepare::{
      Source,
      prepare,
   },
   system::{
      command::{
         Env,
         export,
      },
      dispatch,
   },
};

/// the tail of every nu phase, so what the phase exported reaches the next
/// one. nu keeps PATH as a list, and its own bookkeeping is not part of the
/// build environment.
const HANDOVER: &str = r#"
$env | transpose k v | each {|r|
  let kind = ($r.v | describe | str replace -r '<.*' '')
  {k: $r.k, v: (if $kind == "string" { $r.v } else if $kind == "list" { $r.v | str join ":" } else { null })}
} | where v != null and k !~ '^(NU_|PWD$|OLDPWD$|CURRENT_FILE$|FILE_PWD$|PROCESS_PATH$)' | transpose -rd | to json | save -f $"($env.NIX_BUILD_TOP)/phase-env.json"
"#;

/// nu's own bookkeeping, not part of the build environment a phase hands on.
pub fn nu_internal(name: &str) -> bool {
   name.starts_with("NU_")
      || matches!(
         name,
         "PWD" | "OLDPWD" | "CURRENT_FILE" | "FILE_PWD" | "PROCESS_PATH"
      )
}

fn apply(env: &BTreeMap<String, Value>) {
   for (name, value) in env {
      match *value {
         Value::String(ref text) => export(name, text),
         Value::Array(ref items) => {
            let joined = items
               .iter()
               .filter_map(Value::as_str)
               .collect::<Vec<&str>>();
            export(name, &joined.join(":"));
         },
         Value::Bool(flag) => export(name, &flag.to_string()),
         Value::Number(ref number) => export(name, &number.to_string()),
         Value::Null | Value::Object(_) => {},
      }
   }
}

/// an inline phase, run under nu with the builder tree on its include path
/// and its environment taken back afterwards.
fn nu_phase(attrs: &Attrs, index: usize, name: &str, body: &str, top: &str) -> io::Result<()> {
   let tree = attrs
      .probe_inputs
      .first()
      .ok_or_else(|| io::Error::other("the builder tree is not among probeInputs"))?;
   let script = format!("{top}/phase-{index}.nu");
   write_file(
      Path::new(&script),
      format!("{}\n{body}\n{HANDOVER}", attrs.plan.prelude).as_bytes(),
      None,
   )?;
   Command::new("nu")
      .args([
         "--no-config-file",
         &format!("--include-path={tree}"),
         "--experimental-options=[cell-path-types]",
         &script,
      ])
      .checked()
      .map_err(|error| io::Error::other(format!("phase {name}, {error}")))?;
   let handed = serde_json::from_str::<BTreeMap<String, String>>(&read_to_string(format!(
      "{top}/phase-env.json"
   ))?)?;
   let current = vars().collect::<BTreeMap<String, String>>();
   let mut removed = Env::default();
   for key in current.keys() {
      if !handed.contains_key(key) && !nu_internal(key) {
         removed.unset(key);
      }
   }
   removed.apply();
   for (key, value) in &handed {
      if current.get(key) != Some(value) {
         export(key, value);
      }
   }
   Ok(())
}

/// prepare unpacks and patches with tools from the build dependencies, before
/// it has computed the environment that names them.
pub fn run(tests: bool, from_tree: Option<&Path>, socket: &Path) -> io::Result<()> {
   let attrs = Attrs::load()?;
   let spec = attrs.spec()?;
   let started = Instant::now();
   let mut ctx = setup(
      &attrs,
      from_tree.map_or(Source::Unpack, Source::Archive),
      socket,
   )?;
   let mut timings = vec![format!("prepare {:.1}s", started.elapsed().as_secs_f64())];
   for (index, step) in attrs.plan.phases.iter().enumerate() {
      let wanted = if tests {
         step.test
      } else {
         !step.test || (spec.tests.run.unwrap_or(true) && !attrs.plan.separate && ctx.tests_run)
      };
      if wanted {
         let begun = Instant::now();
         phase(&attrs, &mut ctx, index, socket)?;
         timings.push(format!(
            "{} {:.1}s",
            step.name,
            begun.elapsed().as_secs_f64()
         ));
      }
   }
   set_phase("finish");
   let finishing = Instant::now();
   let finished = if tests {
      finish::tests()
   } else {
      finish::run(attrs.plan.separate)
   };
   timings.push(format!("finish {:.1}s", finishing.elapsed().as_secs_f64()));
   note("phases", &timings.join(", "));
   finished
}

pub fn setup(attrs: &Attrs, source: Source<'_>, socket: &Path) -> io::Result<Ctx> {
   set_phase("prepare");
   let bins = attrs
      .build_dependencies
      .iter()
      .map(|dep| format!("{dep}/bin"))
      .collect::<Vec<String>>();
   export("PATH", &bins.join(":"));
   let prepared = prepare(source)?;
   apply(&prepared.env);
   set_current_dir(&prepared.cwd)?;
   let ctx = prepared.ctx;
   for system in &attrs.plan.setups {
      set_phase(&format!("{system}.setup"));
      note("setup", system);
      dispatch::setup(system, &ctx, attrs, socket)?.apply();
   }
   set_current_dir(match attrs.plan.setups.first() {
      Some(system) => dispatch::workdir(system, &ctx)?,
      None => ctx.src.clone(),
   })?;
   Ok(ctx)
}

pub fn phase(attrs: &Attrs, ctx: &mut Ctx, index: usize, socket: &Path) -> io::Result<()> {
   let step = &attrs.plan.phases[index];
   set_phase(&step.name);
   let workdir = match attrs.plan.setups.first() {
      Some(system) => dispatch::workdir(system, ctx)?,
      None => ctx.src.clone(),
   };
   match step.kind {
      Kind::System {
         ref system,
         ref verb,
      } => dispatch::phase(system, verb, ctx, attrs, socket),
      Kind::Nu { ref body } => {
         set_current_dir(&workdir)?;
         let top = build_top()?;
         nu_phase(attrs, index, &step.name, body, &top)?;
         *ctx = Ctx::load(&top)?;
         Ok(())
      },
   }
}
