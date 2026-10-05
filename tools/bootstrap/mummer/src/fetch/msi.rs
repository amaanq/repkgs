//! Just enough of the Windows Installer table format, read from the streams
//! `7zz x foo.msi` leaves behind (`!_StringPool`, `!_StringData`, `!_Columns`,
//! `!<Table>`). Tables are column-major, integers offset by 0x8000 or
//! 0x80000000, strings are indexes into the pool.

use std::{
   collections::BTreeMap,
   fs::read,
   io,
   path::Path,
};

#[derive(Clone)]
pub enum Cell {
   Text(Option<String>),
   Int(Option<i64>),
}

impl Cell {
   pub fn text(&self) -> Option<&str> {
      match *self {
         Self::Text(ref value) => value.as_deref(),
         Self::Int(_) => None,
      }
   }

   const fn int(&self) -> Option<i64> {
      match *self {
         Self::Int(value) => value,
         Self::Text(_) => None,
      }
   }
}

#[derive(Clone, Copy)]
enum Kind {
   Text,
   Short,
   Long,
}

impl Kind {
   const fn width(self, pool: &Pool) -> usize {
      match self {
         Self::Text => pool.reference,
         Self::Short => 2,
         Self::Long => 4,
      }
   }
}

struct Pool {
   strings:   Vec<Option<String>>,
   reference: usize,
}

pub type Table = Vec<BTreeMap<String, Cell>>;

/// The `width` little-endian bytes at `at`, at most four.
fn int_at(bytes: &[u8], at: usize, width: usize) -> io::Result<u32> {
   let field = at
      .checked_add(width)
      .and_then(|end| bytes.get(at..end))
      .filter(|field| field.len() <= 4)
      .ok_or_else(|| io::Error::other("msi: a table ends inside a field"))?;
   let mut word = [0_u8; 4];
   word[..field.len()].copy_from_slice(field);
   Ok(u32::from_le_bytes(word))
}

fn string_pool(dir: &Path) -> io::Result<Pool> {
   let pool = read(dir.join("!_StringPool"))?;
   let data = read(dir.join("!_StringData"))?;
   let length_of = |value: u32| usize::try_from(value).map_err(io::Error::other);
   let mut strings = vec![None];
   let mut index = 4;
   let mut offset = 0;
   while index < pool.len() {
      let mut length = length_of(int_at(&pool, index, 2)?)?;
      let refs = int_at(&pool, index + 2, 2)?;
      index += 4;
      if length == 0 && refs != 0 {
         length = length_of(int_at(&pool, index, 4)?)?;
         index += 4;
      }
      let bytes = data
         .get(offset..offset + length)
         .ok_or_else(|| io::Error::other("msi: a string runs past the string data"))?;
      strings.push(Some(String::from_utf8_lossy(bytes).into_owned()));
      offset += length;
   }
   let reference = if int_at(&pool, 0, 4)? >= 0x8000_0000 {
      3
   } else {
      2
   };
   Ok(Pool { strings, reference })
}

fn rows(raw: &[u8], kinds: &[Kind], pool: &Pool) -> io::Result<Vec<Vec<Cell>>> {
   let widths = kinds
      .iter()
      .map(|kind| kind.width(pool))
      .collect::<Vec<usize>>();
   let row_width = widths.iter().sum::<usize>();
   if row_width == 0 {
      return Ok(Vec::new());
   }
   let count = raw.len() / row_width;
   let mut starts = Vec::with_capacity(widths.len());
   let mut start = 0;
   for width in &widths {
      starts.push(start);
      start += count * width;
   }
   (0..count)
      .map(|row| {
         kinds
            .iter()
            .zip(&widths)
            .zip(&starts)
            .map(|((kind, width), column_start)| {
               let value = int_at(raw, column_start + row * width, *width)?;
               Ok(match *kind {
                  Kind::Text => {
                     let at = usize::try_from(value).map_err(io::Error::other)?;
                     Cell::Text(
                        pool
                           .strings
                           .get(at)
                           .ok_or_else(|| io::Error::other("msi: a cell names no pooled string"))?
                           .clone(),
                     )
                  },
                  Kind::Short => Cell::Int((value != 0).then(|| i64::from(value) - 0x8000)),
                  Kind::Long => Cell::Int((value != 0).then(|| i64::from(value) - 0x8000_0000)),
               })
            })
            .collect()
      })
      .collect()
}

pub fn table(dir: &Path, name: &str) -> io::Result<Table> {
   let pool = string_pool(dir)?;
   let columns = rows(
      &read(dir.join("!_Columns"))?,
      &[Kind::Text, Kind::Short, Kind::Text, Kind::Short],
      &pool,
   )?;
   let mut names = Vec::new();
   let mut kinds = Vec::new();
   for column in columns {
      if column.first().and_then(Cell::text) != Some(name) {
         continue;
      }
      let Some(column_name) = column.get(2).and_then(Cell::text) else {
         continue;
      };
      let flags = column.get(3).and_then(Cell::int).unwrap_or(0);
      names.push(column_name.to_owned());
      kinds.push(if flags & 0x800 != 0 {
         Kind::Text
      } else if flags & 0xFF == 2 {
         Kind::Short
      } else {
         Kind::Long
      });
   }
   let file = dir.join(format!("!{name}"));
   if names.is_empty() || !file.exists() {
      return Ok(Vec::new());
   }
   Ok(rows(&read(file)?, &kinds, &pool)?
      .into_iter()
      .map(|row| names.iter().cloned().zip(row).collect())
      .collect())
}

pub fn cabinets(dir: &Path) -> io::Result<Vec<String>> {
   Ok(table(dir, "Media")?
      .iter()
      .filter_map(|row| row.get("Cabinet").and_then(Cell::text))
      .map(str::to_owned)
      .collect())
}

/// the long name of a `short|long` `DefaultDir` or `FileName`, without a
/// `:source` part.
fn long(name: &str) -> &str {
   name
      .rsplit('|')
      .next()
      .unwrap_or(name)
      .split(':')
      .next()
      .unwrap_or(name)
}

fn dir_path(dirs: &Table, wanted: Option<&str>, parts: &mut Vec<String>) {
   let Some(id) = wanted else {
      return;
   };
   let Some(dir) = dirs
      .iter()
      .find(|row| row.get("Directory").and_then(Cell::text) == Some(id))
   else {
      return;
   };
   dir_path(
      dirs,
      dir.get("Directory_Parent").and_then(Cell::text),
      parts,
   );
   let name = long(
      dir.get("DefaultDir")
         .and_then(Cell::text)
         .unwrap_or_default(),
   );
   if name != "." && name != "SourceDir" {
      parts.push(name.to_owned());
   }
}

/// File id (the cabinet member name) -> install path.
pub fn install_paths(dir: &Path) -> io::Result<BTreeMap<String, String>> {
   let dirs = table(dir, "Directory")?;
   let component_dirs = table(dir, "Component")?
      .iter()
      .filter_map(|row| {
         Some((
            row.get("Component").and_then(Cell::text)?.to_owned(),
            row.get("Directory_").and_then(Cell::text)?.to_owned(),
         ))
      })
      .collect::<BTreeMap<String, String>>();
   let mut paths = BTreeMap::new();
   for row in table(dir, "File")? {
      let (Some(id), Some(component), Some(file_name)) = (
         row.get("File").and_then(Cell::text),
         row.get("Component_").and_then(Cell::text),
         row.get("FileName").and_then(Cell::text),
      ) else {
         continue;
      };
      let mut parts = Vec::new();
      dir_path(
         &dirs,
         component_dirs.get(component).map(String::as_str),
         &mut parts,
      );
      parts.push(long(file_name).to_owned());
      paths.insert(id.to_owned(), parts.join("/"));
   }
   Ok(paths)
}
