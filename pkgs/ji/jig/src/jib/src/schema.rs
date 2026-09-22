use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io::{
      self,
      Write as _,
   },
   path::Path,
};

use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   hardening::Hardening,
   system::dispatch::schemas,
};

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
#[non_exhaustive]
pub enum Type {
   Boolean,
   String,
   List { element: Box<Self> },
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum Literal {
   Boolean(bool),
   String(String),
   List(Vec<Self>),
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

impl<Item: Into<Self>> From<Vec<Item>> for Literal {
   fn from(value: Vec<Item>) -> Self {
      Self::List(value.into_iter().map(Into::into).collect())
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

impl<Item: OptionType> OptionType for Vec<Item> {
   fn kind() -> Type {
      Type::List {
         element: Box::new(Item::kind()),
      }
   }
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
   name:    String,
   #[serde(rename = "type")]
   kind:    Type,
   default: Literal,
   doc:     String,
   system:  String,
}

impl Field {
   pub fn new<Value>(name: &str, default: Value, doc: &str, system: &str) -> Self
   where
      Value: OptionType,
   {
      Self {
         name:    name.to_owned(),
         kind:    Value::kind(),
         default: default.into(),
         doc:     doc.to_owned(),
         system:  system.to_owned(),
      }
   }
}

macro_rules! options {
   ($name:ident ($system:literal) {
      $($field:ident ($spelling:literal): $kind:ty = $default:expr => $doc:literal),* $(,)?
   }) => {
      #[derive(serde::Deserialize)]
      #[serde(default, deny_unknown_fields)]
      struct $name {
         $(#[serde(rename = $spelling)] $field: $kind),*
      }

      impl Default for $name {
         fn default() -> Self {
            Self { $($field: $default),* }
         }
      }

      impl $name {
         fn schema() -> Vec<crate::schema::Field> {
            let defaults = Self::default();
            vec![$(crate::schema::Field::new($spelling, defaults.$field, $doc, $system)),*]
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
