use std::{
   collections::BTreeMap,
   fmt,
};

use serde::{
   Deserialize,
   Deserializer,
   Serialize,
   Serializer,
   de::{
      Error as _,
      MapAccess,
      Visitor,
      value::MapAccessDeserializer,
   },
};

use crate::schema::{
   OptionType,
   Type,
};

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum Hardening {
   Format,
   Fortify,
   Noplt,
   StackClashProtection,
   StackProtector,
   StrictFlexArrays,
   StrictOverflow,
   TrivialAutoVarInit,
   ZeroCallUsedRegs,
   Libcxx,
   Relro,
   BindNow,
   Relr,
   NoExecStack,
   AsNeeded,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BinaryFormat {
   Elf,
   Macho,
   Coff,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Line {
   Compile,
   Cxx,
   Link,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum Choice {
   All(bool),
   Flags(BTreeMap<Hardening, bool>),
}

impl OptionType for Choice {
   fn kind() -> Type {
      Type::union(vec![Type::Boolean, BTreeMap::<Hardening, bool>::kind()])
   }
}

impl<'de> Deserialize<'de> for Choice {
   fn deserialize<Decoder>(deserializer: Decoder) -> Result<Self, Decoder::Error>
   where
      Decoder: Deserializer<'de>,
   {
      struct ChoiceVisitor;

      impl<'de> Visitor<'de> for ChoiceVisitor {
         type Value = Choice;

         fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a boolean or a map of hardening names to booleans")
         }

         #[expect(
            clippy::renamed_function_params,
            reason = "serde names this parameter v"
         )]
         fn visit_bool<Error>(self, value: bool) -> Result<Choice, Error> {
            Ok(Choice::All(value))
         }

         fn visit_map<Map>(self, map: Map) -> Result<Choice, Map::Error>
         where
            Map: MapAccess<'de>,
         {
            BTreeMap::deserialize(MapAccessDeserializer::new(map)).map(Choice::Flags)
         }
      }

      deserializer.deserialize_any(ChoiceVisitor)
   }
}

impl Serialize for Hardening {
   fn serialize<Encoder>(&self, serializer: Encoder) -> Result<Encoder::Ok, Encoder::Error>
   where
      Encoder: Serializer,
   {
      serializer.serialize_str(self.name())
   }
}

impl<'de> Deserialize<'de> for Hardening {
   fn deserialize<Decoder>(deserializer: Decoder) -> Result<Self, Decoder::Error>
   where
      Decoder: Deserializer<'de>,
   {
      let name = String::deserialize(deserializer)?;
      Self::ALL
         .into_iter()
         .find(|flag| flag.name() == name)
         .ok_or_else(|| Decoder::Error::custom(format!("unknown hardening flag {name}")))
   }
}

impl OptionType for Hardening {
   fn kind() -> Type {
      Type::Enum {
         values: Self::ALL.map(|flag| flag.name().to_owned()).to_vec(),
      }
   }
}

impl Hardening {
   pub const ALL: [Self; 15] = [
      Self::Format,
      Self::Fortify,
      Self::Noplt,
      Self::StackClashProtection,
      Self::StackProtector,
      Self::StrictFlexArrays,
      Self::StrictOverflow,
      Self::TrivialAutoVarInit,
      Self::ZeroCallUsedRegs,
      Self::Libcxx,
      Self::Relro,
      Self::BindNow,
      Self::Relr,
      Self::NoExecStack,
      Self::AsNeeded,
   ];

   const fn name(self) -> &'static str {
      match self {
         Self::Format => "format",
         Self::Fortify => "fortify",
         Self::Noplt => "noplt",
         Self::StackClashProtection => "stackclashprotection",
         Self::StackProtector => "stackprotector",
         Self::StrictFlexArrays => "strictflexarrays",
         Self::StrictOverflow => "strictoverflow",
         Self::TrivialAutoVarInit => "trivialautovarinit",
         Self::ZeroCallUsedRegs => "zerocallusedregs",
         Self::Libcxx => "libcxxhardening",
         Self::Relro => "relro",
         Self::BindNow => "bindnow",
         Self::Relr => "relr",
         Self::NoExecStack => "noexecstack",
         Self::AsNeeded => "asneeded",
      }
   }

   const fn line(self) -> Line {
      match self {
         Self::Libcxx => Line::Cxx,
         Self::Relro | Self::BindNow | Self::Relr | Self::NoExecStack | Self::AsNeeded => {
            Line::Link
         },
         Self::Format
         | Self::Fortify
         | Self::Noplt
         | Self::StackClashProtection
         | Self::StackProtector
         | Self::StrictFlexArrays
         | Self::StrictOverflow
         | Self::TrivialAutoVarInit
         | Self::ZeroCallUsedRegs => Line::Compile,
      }
   }

   const fn elf_only(self) -> bool {
      matches!(self.line(), Line::Link) || matches!(self, Self::Noplt | Self::StackClashProtection)
   }

   const fn flags(self) -> &'static [&'static str] {
      match self {
         Self::Format => &["-Wformat", "-Wformat-security", "-Werror=format-security"],
         Self::Fortify => &["-D_FORTIFY_SOURCE=3"],
         Self::Noplt => &["-fno-plt"],
         Self::StackClashProtection => &["-fstack-clash-protection"],
         Self::StackProtector => &["-fstack-protector-strong"],
         Self::StrictFlexArrays => &["-fstrict-flex-arrays=1"],
         Self::StrictOverflow => &["-fwrapv"],
         Self::TrivialAutoVarInit => &["-ftrivial-auto-var-init=zero"],
         Self::ZeroCallUsedRegs => &["-fzero-call-used-regs=used-gpr"],
         Self::Libcxx => &["-D_LIBCPP_HARDENING_MODE=_LIBCPP_HARDENING_MODE_FAST"],
         Self::Relro => &["-Wl,-z,relro"],
         Self::BindNow => &["-Wl,-z,now"],
         Self::Relr => &["-Wl,-z,pack-relative-relocs"],
         Self::NoExecStack => &["-Wl,-z,noexecstack"],
         Self::AsNeeded => &["-Wl,--as-needed"],
      }
   }
}

pub fn flags(
   format: BinaryFormat,
   platform: &BTreeMap<Hardening, bool>,
   choice: Option<&Choice>,
   line: Line,
) -> Vec<String> {
   if matches!(choice, Some(Choice::All(false))) {
      return Vec::new();
   }

   let overrides = choice.and_then(|selection| {
      match *selection {
         Choice::Flags(ref selected) => Some(selected),
         Choice::All(_) => None,
      }
   });

   Hardening::ALL
      .into_iter()
      .filter(|hardening| hardening.line() == line)
      .filter(|hardening| {
         overrides
            .and_then(|selected| selected.get(hardening))
            .or_else(|| platform.get(hardening))
            .copied()
            .unwrap_or_else(|| format == BinaryFormat::Elf || !hardening.elf_only())
      })
      .flat_map(Hardening::flags)
      .map(|flag| (*flag).to_owned())
      .collect()
}
