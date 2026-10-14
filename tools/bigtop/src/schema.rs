use std::{
   collections::BTreeMap,
   fmt::{
      self,
      Display,
      Formatter,
   },
   fs::read_to_string,
   io::{
      self,
      Write as _,
   },
   ops::Deref,
   path::{
      Path,
      PathBuf,
   },
};

use serde::{
   Deserialize,
   Serialize,
};
use serde_json::Value;

use crate::{
   attrs::Def,
   hardening::Hardening,
   recipe::recipe,
   system::dispatch::schemas,
   targets::{
      Facts,
      targets,
   },
};

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
#[non_exhaustive]
pub enum Type {
   Boolean,
   String,
   Integer,
   Path,
   Package,
   Null,
   List {
      element: Box<Self>,
   },
   Map {
      #[serde(default, skip_serializing_if = "Option::is_none")]
      key:   Option<Box<Self>>,
      value: Box<Self>,
   },
   Union {
      variants: Vec<Self>,
   },
   Enum {
      values: Vec<String>,
   },
   Record {
      fields:   BTreeMap<String, Self>,
      #[serde(default, skip_serializing_if = "Vec::is_empty")]
      optional: Vec<String>,
   },
}

impl Type {
   pub fn list(element: Self) -> Self {
      Self::List {
         element: Box::new(element),
      }
   }

   pub fn map(value: Self) -> Self {
      Self::Map {
         key:   None,
         value: Box::new(value),
      }
   }

   pub fn union(variants: Vec<Self>) -> Self {
      Self::Union {
         variants: variants
            .into_iter()
            .flat_map(|variant| {
               if let Self::Union { variants: nested } = variant {
                  nested
               } else {
                  vec![variant]
               }
            })
            .collect(),
      }
   }

   pub fn record(required: Vec<(&str, Self)>, optional: Vec<(&str, Self)>) -> Self {
      Self::Record {
         optional: optional.iter().map(|field| field.0.to_owned()).collect(),
         fields:   required
            .into_iter()
            .chain(optional)
            .map(|field| (field.0.to_owned(), field.1))
            .collect(),
      }
   }
}

#[derive(Deserialize, Serialize)]
#[serde(transparent)]
pub struct PackagePath(String);

impl Display for PackagePath {
   fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
      f.write_str(&self.0)
   }
}

impl AsRef<Path> for PackagePath {
   fn as_ref(&self) -> &Path {
      Path::new(&self.0)
   }
}

impl Deref for PackagePath {
   type Target = str;

   fn deref(&self) -> &Self::Target {
      &self.0
   }
}

pub trait OptionType {
   fn kind() -> Type;
}

impl OptionType for bool {
   fn kind() -> Type {
      Type::Boolean
   }
}

impl OptionType for String {
   fn kind() -> Type {
      Type::String
   }
}

impl OptionType for PackagePath {
   fn kind() -> Type {
      Type::union(vec![Type::Package, Type::String])
   }
}

impl OptionType for PathBuf {
   fn kind() -> Type {
      Type::union(vec![Type::String, Type::Path, Type::Package])
   }
}

impl<Item: OptionType> OptionType for Option<Item> {
   fn kind() -> Type {
      Type::union(vec![Item::kind(), Type::Null])
   }
}

impl<Item: OptionType> OptionType for Vec<Item> {
   fn kind() -> Type {
      Type::list(Item::kind())
   }
}

impl<Key: OptionType, Item: OptionType> OptionType for BTreeMap<Key, Item> {
   fn kind() -> Type {
      let key = Key::kind();

      Type::Map {
         key:   (key != Type::String).then(|| Box::new(key)),
         value: Box::new(Item::kind()),
      }
   }
}

impl OptionType for Def {
   fn kind() -> Type {
      Type::union(vec![
         Type::Boolean,
         Type::String,
         Type::Integer,
         Type::Path,
         Type::Package,
         Type::list(Type::union(vec![Type::String, Type::Path, Type::Package])),
      ])
   }
}

/// A required option has no default of its own, Nix supplies it from a lock or
/// a fetcher.
#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
   name:     String,
   #[serde(rename = "type")]
   kind:     Type,
   default:  Value,
   #[serde(default, skip_serializing_if = "is_false")]
   required: bool,
   doc:      String,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   system:   Option<String>,
}

#[expect(
   clippy::trivially_copy_pass_by_ref,
   reason = "serde passes the field by reference"
)]
const fn is_false(value: &bool) -> bool {
   !*value
}

impl Field {
   pub fn new<Kind>(name: &str, default: Option<Kind>, doc: &str, system: &str) -> io::Result<Self>
   where
      Kind: OptionType + Serialize,
   {
      Ok(Self {
         name:     name.to_owned(),
         kind:     Kind::kind(),
         required: default.is_none(),
         default:  serde_json::to_value(default)?,
         doc:      doc.to_owned(),
         system:   Some(system.to_owned()),
      })
   }

   /// A common recipe field, which no build system owns.
   pub fn common(name: &str, kind: Type, required: bool, doc: &str) -> Self {
      Self {
         name: name.to_owned(),
         kind,
         default: Value::Null,
         required,
         doc: doc.to_owned(),
         system: None,
      }
   }
}

/// A closed record nested in an option. Fields listed under `optional` may be
/// absent and are `Option`s in Rust.
macro_rules! record {
   ($(#[$meta:meta])* $vis:vis $name:ident {
      $(required { $($field:ident ($spelling:literal): $kind:ty),* $(,)? })?
      $(optional { $($ofield:ident ($ospelling:literal): $okind:ty),* $(,)? })?
   }) => {
      $(#[$meta])*
      #[derive(serde::Deserialize, serde::Serialize)]
      #[serde(deny_unknown_fields)]
      $vis struct $name {
         $($(#[serde(rename = $spelling)] pub $field: $kind,)*)?
         $($(
            #[serde(rename = $ospelling, default, skip_serializing_if = "Option::is_none")]
            pub $ofield: Option<$okind>,
         )*)?
      }

      impl crate::schema::OptionType for $name {
         fn kind() -> crate::schema::Type {
            crate::schema::Type::record(
               vec![$($(($spelling, <$kind as crate::schema::OptionType>::kind())),*)?],
               vec![$($(($ospelling, <$okind as crate::schema::OptionType>::kind())),*)?],
            )
         }
      }
   };
}

macro_rules! options {
   (@default) => {
      None
   };
   (@default $default:expr) => {
      Some($default)
   };
   ($name:ident {
      $($field:ident ($spelling:literal): $kind:ty $(= $default:expr)? => $doc:literal),* $(,)?
   }) => {
      #[derive(serde::Deserialize)]
      #[serde(deny_unknown_fields)]
      struct $name {
         $(#[serde(rename = $spelling)] $field: $kind),*
      }

      impl $name {
         fn schema(system: &str) -> std::io::Result<Vec<crate::schema::Field>> {
            Ok(vec![$(crate::schema::Field::new::<$kind>(
               $spelling,
               options!(@default $($default)?),
               $doc,
               system,
            )?),*])
         }
      }
   };
}

/// The platform a build system cannot serve.
#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Condition {
   Cross,
   Libc(String),
   LibcOtherThan(String),
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Unsupported {
   when:   Condition,
   reason: String,
}

impl Unsupported {
   pub fn new(when: Condition, reason: &str) -> Self {
      Self {
         when,
         reason: reason.to_owned(),
      }
   }
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
   pub phases:      Vec<String>,
   #[serde(default, skip_serializing_if = "is_false")]
   pub prebuilt:    bool,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub unsupported: Option<Unsupported>,
   pub options:     Vec<Field>,
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Schema {
   hardening: Vec<Hardening>,
   recipe:    Vec<Field>,
   systems:   BTreeMap<String, Descriptor>,
   targets:   Vec<Facts>,
}

pub fn run(check: Option<&Path>) -> io::Result<()> {
   let systems = schemas()?;
   let current = Schema {
      hardening: Hardening::ALL.to_vec(),
      recipe: recipe(systems.keys().cloned().collect()),
      systems,
      targets: targets(),
   };

   if let Some(path) = check {
      let recorded = serde_json::from_str::<Schema>(&read_to_string(path)?)?;

      if recorded != current {
         return Err(io::Error::other(format!(
            "{} is stale, regenerate it with bigtop schema",
            path.display()
         )));
      }

      return Ok(());
   }

   let mut stdout = io::stdout().lock();
   serde_json::to_writer_pretty(&mut stdout, &current)?;
   writeln!(stdout)
}
