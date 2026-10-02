use std::iter::once;

use crate::{
   attrs::{
      Cc,
      Tests,
   },
   exports::Stored,
   schema::{
      Field,
      OptionType as _,
      Type,
   },
};

fn strings() -> Type {
   Type::list(Type::String)
}

fn source() -> Type {
   Type::union(vec![Type::String, Type::Path, Type::Package])
}

fn step() -> Type {
   Type::union(vec![
      Type::String,
      Type::record(vec![("name", Type::String), ("run", Type::String)], vec![]),
   ])
}

fn steps() -> Type {
   Type::map(Type::union(vec![step(), Type::list(step())]))
}

fn feature() -> Type {
   let scalar = || vec![Type::Boolean, Type::String, Type::Integer];
   let mut default = scalar();
   default.push(strings());

   Type::record(vec![("default", Type::union(default))], vec![
      ("values", Type::list(Type::union(scalar()))),
      ("doc", Type::String),
   ])
}

fn platforms() -> Type {
   Type::record(vec![], vec![
      ("cpu", strings()),
      ("os", strings()),
      ("abi", strings()),
      ("libc", strings()),
      ("posix", Type::Boolean),
      ("cross", Type::Boolean),
   ])
}

fn phases() -> Type {
   Type::union(vec![
      Type::list(step()),
      Type::record(vec![], vec![
         ("before", steps()),
         ("after", steps()),
         ("replace", steps()),
         ("remove", strings()),
      ]),
   ])
}

fn completions() -> Type {
   let shells = ["bash", "zsh", "fish", "nu"].into_iter();
   Type::record(
      vec![],
      shells.map(|shell| (shell, Type::list(source()))).collect(),
   )
}

fn optional(systems: Vec<String>) -> [(&'static str, Type, &'static str); 22] {
   let prebuilt = Type::Enum {
      values: vec!["ldso".to_owned()],
   };

   [
      (
         "version",
         Type::String,
         "upstream version, by default from sources.toml",
      ),
      (
         "features",
         Type::map(feature()),
         "declared features, a default and the values it may take",
      ),
      (
         "platforms",
         platforms(),
         "where the package builds, every listed key must admit the platform",
      ),
      (
         "source",
         source(),
         "a sources.toml key, a path or a derivation, by default the platform's tarball",
      ),
      (
         "patches",
         Type::list(source()),
         "applied with -p1 before the first phase",
      ),
      (
         "uses",
         Type::list(Type::Enum { values: systems }),
         "build systems, each set up before the phases",
      ),
      (
         "phases",
         phases(),
         "the phase list, or before, after, replace and remove edits to the first build system's",
      ),
      (
         "buildDependencies",
         Type::list(Type::Package),
         "packages for the build machine, on PATH",
      ),
      (
         "dependencies",
         Type::list(Type::Package),
         "packages for the target, linked against",
      ),
      (
         "sys",
         Type::union(vec![strings(), Type::Null]),
         "libraries the lock files may link, by default [pin] sys from sources.toml",
      ),
      ("bin", strings(), "programs the version check runs"),
      (
         "tests",
         Tests::kind(),
         "whether and how the test phases run",
      ),
      (
         "exports",
         Type::union(vec![Type::Boolean, Stored::kind()]),
         "what dependents see, false for nothing",
      ),
      ("env", Type::map(source()), "environment for every phase"),
      (
         "root",
         Type::String,
         "directory below the source the project lives in",
      ),
      ("cc", Cc::kind(), "extra compiler flags and hardening"),
      (
         "bootstrapTools",
         Type::Boolean,
         "build with the seed's tools instead of the set's base tools",
      ),
      (
         "prebuilt",
         Type::union(vec![Type::Boolean, prebuilt]),
         "upstream binaries, implanted with our interpreter (true) or run via launch (ldso)",
      ),
      ("debug", Type::Boolean, "split DWARF into the debug output"),
      (
         "install",
         Type::map(Type::union(vec![source(), Type::list(source())])),
         "output path -> files copied there",
      ),
      (
         "links",
         Type::map(Type::String),
         "output path -> symlink target",
      ),
      (
         "completions",
         completions(),
         "shell completion files, installed where each shell looks",
      ),
   ]
}

pub fn recipe(systems: Vec<String>) -> Vec<Field> {
   let name = Field::common("name", Type::String, true, "attribute and output name");
   let rest = optional(systems).map(|(field, kind, doc)| Field::common(field, kind, false, doc));
   once(name).chain(rest).collect()
}
