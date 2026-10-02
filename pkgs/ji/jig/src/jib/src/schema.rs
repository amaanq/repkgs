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

use crate::{
   attrs::Def,
   hardening::Hardening,
   system::dispatch::schemas,
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
   List { element: Box<Self> },
   Map { value: Box<Self> },
   Union { variants: Vec<Self> },
   Enum { values: Vec<String> },
   Record { fields: BTreeMap<String, Self> },
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum Literal {
   Boolean(bool),
   String(String),
   Integer(i64),
   List(Vec<Self>),
   Map(BTreeMap<String, Self>),
   Null,
}

impl From<bool> for Literal {
   fn from(value: bool) -> Self {
      Self::Boolean(value)
   }
}

impl From<String> for Literal {
   fn from(value: String) -> Self {
      Self::String(value)
   }
}

impl From<PathBuf> for Literal {
   fn from(value: PathBuf) -> Self {
      Self::String(value.to_string_lossy().into_owned())
   }
}

impl<Item: Into<Self>> From<Option<Item>> for Literal {
   fn from(value: Option<Item>) -> Self {
      value.map_or(Self::Null, Into::into)
   }
}

#[derive(Deserialize)]
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

impl From<PackagePath> for Literal {
   fn from(value: PackagePath) -> Self {
      Self::String(value.0)
   }
}

impl<Item: Into<Self>> From<Vec<Item>> for Literal {
   fn from(value: Vec<Item>) -> Self {
      Self::List(value.into_iter().map(Into::into).collect())
   }
}

impl<Key: Display, Item: Into<Self>> From<BTreeMap<Key, Item>> for Literal {
   fn from(value: BTreeMap<Key, Item>) -> Self {
      Self::Map(
         value
            .into_iter()
            .map(|(name, item)| (name.to_string(), item.into()))
            .collect(),
      )
   }
}

impl From<Def> for Literal {
   fn from(value: Def) -> Self {
      match value {
         Def::Flag(flag) => Self::Boolean(flag),
         Def::Text(text) => Self::String(text),
         Def::Number(number) => Self::Integer(number),
      }
   }
}

pub trait OptionType: Into<Literal> {
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
      Type::Union {
         variants: vec![Type::Package, Type::String],
      }
   }
}

impl OptionType for PathBuf {
   fn kind() -> Type {
      Type::Union {
         variants: vec![Type::String, Type::Path, Type::Package],
      }
   }
}

impl<Item: OptionType> OptionType for Option<Item> {
   fn kind() -> Type {
      Type::Union {
         variants: vec![Item::kind(), Type::Null],
      }
   }
}

impl<Item: OptionType> OptionType for Vec<Item> {
   fn kind() -> Type {
      Type::List {
         element: Box::new(Item::kind()),
      }
   }
}

impl<Key: Display, Item: OptionType> OptionType for BTreeMap<Key, Item> {
   fn kind() -> Type {
      Type::Map {
         value: Box::new(Item::kind()),
      }
   }
}

impl OptionType for Def {
   fn kind() -> Type {
      Type::Union {
         variants: vec![
            Type::Boolean,
            Type::String,
            Type::Integer,
            Type::Path,
            Type::Package,
         ],
      }
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
   default:  Literal,
   #[serde(default, skip_serializing_if = "is_false")]
   required: bool,
   doc:      String,
   system:   String,
}

#[expect(
   clippy::trivially_copy_pass_by_ref,
   reason = "serde passes the field by reference"
)]
const fn is_false(value: &bool) -> bool {
   !*value
}

impl Field {
   pub fn new<Value>(name: &str, default: Option<Value>, doc: &str, system: &str) -> Self
   where
      Value: OptionType,
   {
      Self {
         name:     name.to_owned(),
         kind:     Value::kind(),
         required: default.is_none(),
         default:  default.map_or(Literal::Null, Into::into),
         doc:      doc.to_owned(),
         system:   system.to_owned(),
      }
   }
}

macro_rules! record {
   ($name:ident {
      $($field:ident ($spelling:literal): $kind:ty),* $(,)?
   }) => {
      #[derive(Default, serde::Deserialize)]
      #[serde(deny_unknown_fields)]
      struct $name {
         $(#[serde(rename = $spelling)] $field: $kind),*
      }

      impl crate::schema::OptionType for $name {
         fn kind() -> crate::schema::Type {
            crate::schema::Type::Record {
               fields: [$((
                  $spelling.to_owned(),
                  <$kind as crate::schema::OptionType>::kind(),
               )),*]
               .into(),
            }
         }
      }

      impl From<$name> for crate::schema::Literal {
         fn from(value: $name) -> Self {
            Self::Map([$(($spelling.to_owned(), value.$field.into())),*].into())
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
         fn schema(system: &str) -> Vec<crate::schema::Field> {
            vec![$(crate::schema::Field::new::<$kind>(
               $spelling,
               options!(@default $($default)?),
               $doc,
               system,
            )),*]
         }
      }
   };
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Schema {
   hardening: Vec<Hardening>,
   systems:   BTreeMap<String, Vec<Field>>,
}

pub fn run(check: Option<&Path>) -> io::Result<()> {
   let current = Schema {
      hardening: Hardening::ALL.to_vec(),
      systems:   schemas(),
   };

   if let Some(path) = check {
      let recorded: Schema = serde_json::from_str(&read_to_string(path)?)?;

      if recorded != current {
         return Err(io::Error::other(format!(
            "{} is stale, regenerate it with jib schema",
            path.display()
         )));
      }

      return Ok(());
   }

   let mut stdout = io::stdout().lock();
   serde_json::to_writer_pretty(&mut stdout, &current)?;
   writeln!(stdout)
}
