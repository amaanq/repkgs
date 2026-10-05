use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   env::current_dir,
   fs::read_to_string,
   io::{
      self,
      Write as _,
   },
   mem::take,
   path::{
      Component,
      Path,
      PathBuf,
   },
};

use glob::glob;
use roxmltree::{
   Document,
   Node,
};
use serde::{
   Deserialize,
   Serialize,
};

#[derive(Serialize)]
pub struct Element {
   tag:        String,
   attributes: BTreeMap<String, String>,
   content:    Vec<Content>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Content {
   Element(Element),
   Text {
      tag:        (),
      attributes: (),
      content:    String,
   },
}

#[derive(Serialize)]
pub struct Item {
   #[serde(rename = "type")]
   pub kind:    String,
   pub include: String,
   pub path:    String,
   pub meta:    BTreeMap<String, String>,
}

#[derive(Serialize)]
pub struct Evaluation {
   pub props:    BTreeMap<String, String>,
   pub itemdefs: BTreeMap<String, BTreeMap<String, String>>,
   pub items:    Vec<Item>,
   pub targets:  BTreeMap<String, Vec<Element>>,
}

#[derive(Deserialize)]
struct Input {
   properties: BTreeMap<String, String>,
   #[serde(default)]
   itemdefs:   BTreeMap<String, BTreeMap<String, String>>,
}

impl Element {
   fn from_node(node: Node<'_, '_>) -> Self {
      Self {
         tag:        node.tag_name().name().to_owned(),
         attributes: node
            .attributes()
            .map(|attr| (attr.name().to_owned(), attr.value().to_owned()))
            .collect(),
         content:    node
            .children()
            .filter_map(|child| {
               if child.is_element() {
                  Some(Content::Element(Self::from_node(child)))
               } else {
                  child
                     .text()
                     .filter(|text| child.is_text() && !text.trim().is_empty())
                     .map(|text| {
                        Content::Text {
                           tag:        (),
                           attributes: (),
                           content:    text.trim().to_owned(),
                        }
                     })
               }
            })
            .collect(),
      }
   }

   fn elements(&self) -> impl Iterator<Item = &Self> {
      self.content.iter().filter_map(|content| {
         if let Content::Element(ref element) = *content {
            Some(element)
         } else {
            None
         }
      })
   }

   fn into_elements(self) -> Vec<Self> {
      self
         .content
         .into_iter()
         .filter_map(|content| {
            if let Content::Element(element) = content {
               Some(element)
            } else {
               None
            }
         })
         .collect()
   }

   fn text(&self) -> String {
      self
         .content
         .iter()
         .filter_map(|content| {
            match *content {
               Content::Text {
                  tag: (),
                  attributes: (),
                  content: ref text,
               } => Some(text.as_str()),
               Content::Element(_) => None,
            }
         })
         .collect::<String>()
         .trim()
         .to_owned()
   }

   fn attr(&self, name: &str) -> &str {
      self.attributes.get(name).map_or("", String::as_str)
   }

   fn passes(
      &self,
      props: &BTreeMap<String, String>,
      dir: &Path,
      meta: &BTreeMap<String, String>,
   ) -> bool {
      let source = self.attr("Condition");
      source.is_empty() || condition(source, props, dir, meta).unwrap_or(false)
   }

   fn holds(&self, props: &BTreeMap<String, String>, dir: &Path) -> bool {
      self.passes(props, dir, &BTreeMap::new())
   }
}

pub fn run(file: &Path) -> io::Result<()> {
   let input: Input = serde_json::from_reader(io::stdin().lock())?;
   let result = evaluate(file, &input.properties, input.itemdefs)?;
   let mut stdout = io::stdout().lock();
   serde_json::to_writer(&mut stdout, &result)?;
   writeln!(stdout)
}

pub fn evaluate(
   file: &Path,
   seeds: &BTreeMap<String, String>,
   itemdefs: BTreeMap<String, BTreeMap<String, String>>,
) -> io::Result<Evaluation> {
   let name = file
      .file_stem()
      .unwrap_or_default()
      .to_string_lossy()
      .into_owned();
   let mut props = BTreeMap::from([
      ("MSBuildProjectName".to_owned(), name.clone()),
      ("ProjectName".to_owned(), name),
   ]);
   props.extend(
      seeds
         .iter()
         .map(|(key, value)| (key.clone(), value.clone())),
   );
   let mut result = Evaluation {
      props,
      itemdefs,
      items: Vec::new(),
      targets: BTreeMap::new(),
   };
   result.eval_file(file, seeds)?;
   Ok(result)
}

pub fn win_path(value: &str, dir: &Path) -> io::Result<String> {
   let replaced = value.replace('\\', "/");
   if replaced.starts_with('/') {
      return Ok(replaced);
   }
   let joined = dir.join(replaced);
   let absolute = if joined.is_absolute() {
      joined
   } else {
      current_dir()?.join(joined)
   };
   let mut normalized = PathBuf::new();
   for component in absolute.components() {
      match component {
         Component::ParentDir => {
            normalized.pop();
         },
         Component::CurDir => {},
         Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
            normalized.push(component.as_os_str());
         },
      }
   }
   Ok(normalized.to_string_lossy().into_owned())
}

impl Evaluation {
   pub fn sub_projects(&self, target: &str, dir: &Path) -> io::Result<Vec<String>> {
      let mut found = Vec::new();
      let mut seen = BTreeSet::new();
      for node in self.targets.get(target).into_iter().flatten() {
         if node.tag != "MSBuild" || !node.holds(&self.props, dir) {
            continue;
         }
         let referenced = item_refs(node.attr("Projects"), &self.items)?;
         let expanded = expand(&referenced, &self.props, &BTreeMap::new())?;
         for project in split_list(&expanded) {
            if seen.insert(project.to_owned()) {
               found.push(win_path(project, dir)?);
            }
         }
      }
      Ok(found)
   }

   fn eval_file(&mut self, file: &Path, seeds: &BTreeMap<String, String>) -> io::Result<()> {
      let dir = file.parent().unwrap_or_else(|| Path::new("."));
      let directory = format!("{}/", dir.display());
      let outer = self
         .props
         .insert("MSBuildThisFileDirectory".to_owned(), directory.clone())
         .unwrap_or(directory);
      let source = read_to_string(file)?;
      let document = Document::parse(&source).map_err(io::Error::other)?;
      let root = Element::from_node(document.root_element());
      for node in root.into_elements() {
         self.eval_element(node, dir, seeds)?;
      }
      self
         .props
         .insert("MSBuildThisFileDirectory".to_owned(), outer);
      Ok(())
   }

   fn eval_element(
      &mut self,
      node: Element,
      dir: &Path,
      seeds: &BTreeMap<String, String>,
   ) -> io::Result<()> {
      if !node.holds(&self.props, dir) {
         return Ok(());
      }
      match node.tag.as_str() {
         "PropertyGroup" => {
            for property in node.elements() {
               if seeds.contains_key(&property.tag) || !property.holds(&self.props, dir) {
                  continue;
               }
               if let Ok(value) = expand(&property.text(), &self.props, &BTreeMap::new()) {
                  self.props.insert(property.tag.clone(), value);
               }
            }
         },
         "ItemDefinitionGroup" => {
            for definition in node.elements() {
               if definition.holds(&self.props, dir) {
                  metadata(
                     definition,
                     self.itemdefs.entry(definition.tag.clone()).or_default(),
                     &self.props,
                     dir,
                  )?;
               }
            }
         },
         "ItemGroup" => {
            for item in node.elements() {
               self.eval_item(item, dir)?;
            }
         },
         "ImportGroup" => {
            for child in node.into_elements() {
               self.eval_element(child, dir, seeds)?;
            }
         },
         "Import" => {
            let expanded = expand(node.attr("Project"), &self.props, &BTreeMap::new())?;
            let path = win_path(&expanded, dir)?;
            let file = Path::new(&path);
            if !path.ends_with(".targets")
               && !file
                  .file_name()
                  .is_some_and(|name| name.to_string_lossy().starts_with("Microsoft.Cpp"))
               && file.exists()
            {
               self.eval_file(file, seeds)?;
            }
         },
         "Target" => {
            self
               .targets
               .insert(node.attr("Name").to_owned(), node.into_elements());
         },
         _ => {},
      }
      Ok(())
   }

   fn eval_item(&mut self, node: &Element, dir: &Path) -> io::Result<()> {
      if !node.attributes.contains_key("Include") {
         for item in &mut self.items {
            if item.kind == node.tag && node.passes(&self.props, dir, &well_known(item)) {
               with_metadata(item, node, &self.props, dir)?;
            }
         }
         return Ok(());
      }
      if !node.holds(&self.props, dir) {
         return Ok(());
      }
      let excluded: BTreeSet<String> = self
         .item_paths(node.attr("Exclude"), dir)?
         .into_iter()
         .map(|entry| entry.1)
         .collect();
      let mut defaults = self.itemdefs.get(&node.tag).cloned().unwrap_or_default();
      defaults.extend(
         node
            .attributes
            .iter()
            .filter(|&(key, _)| {
               !matches!(key.as_str(), "Include" | "Exclude" | "Condition" | "Label")
            })
            .map(|(key, value)| (key.clone(), value.clone())),
      );
      for (include, path) in self.item_paths(node.attr("Include"), dir)? {
         if excluded.contains(&path) {
            continue;
         }
         let mut item = Item {
            kind: node.tag.clone(),
            include,
            path,
            meta: defaults.clone(),
         };
         if node.elements().next().is_some() {
            with_metadata(&mut item, node, &self.props, dir)?;
         }
         self.items.push(item);
      }
      Ok(())
   }

   fn item_paths(&self, spec: &str, dir: &Path) -> io::Result<Vec<(String, String)>> {
      let referenced = item_refs(spec, &self.items)?;
      let expanded = expand(&referenced, &self.props, &BTreeMap::new())?;
      let mut paths = Vec::new();
      for include in split_list(&expanded) {
         let path = win_path(include, dir)?;
         if include.contains(['*', '?']) {
            for matched in glob(&path).map_err(io::Error::other)? {
               let matched_path = matched.map_err(io::Error::other)?;

               if !matched_path.is_file() {
                  continue;
               }

               paths.push((
                  include.to_owned(),
                  matched_path.to_string_lossy().into_owned(),
               ));
            }
         } else {
            paths.push((include.to_owned(), path));
         }
      }
      Ok(paths)
   }
}

pub fn split_list(source: &str) -> impl Iterator<Item = &str> {
   source
      .split(';')
      .map(str::trim)
      .filter(|part| !part.is_empty())
}

fn well_known(item: &Item) -> BTreeMap<String, String> {
   let path = Path::new(&item.path);
   let mut meta = item.meta.clone();
   meta.extend([
      ("Identity".to_owned(), item.include.clone()),
      ("FullPath".to_owned(), item.path.clone()),
      (
         "Filename".to_owned(),
         path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
      ),
      (
         "Extension".to_owned(),
         format!(
            ".{}",
            path.extension().unwrap_or_default().to_string_lossy()
         ),
      ),
   ]);
   meta
}

fn with_metadata(
   item: &mut Item,
   node: &Element,
   props: &BTreeMap<String, String>,
   dir: &Path,
) -> io::Result<()> {
   let mut meta = well_known(item);
   metadata(node, &mut meta, props, dir)?;
   for key in ["Identity", "FullPath", "Filename", "Extension"] {
      meta.remove(key);
   }
   item.meta = meta;
   Ok(())
}

fn metadata(
   node: &Element,
   base: &mut BTreeMap<String, String>,
   props: &BTreeMap<String, String>,
   dir: &Path,
) -> io::Result<()> {
   for child in node.elements() {
      if child.passes(props, dir, base) {
         let value = expand(&child.text(), props, base)?;
         base.insert(child.tag.clone(), value.trim().to_owned());
      }
   }
   Ok(())
}

enum Segment {
   Text(String),
   Property(String),
   Metadata(String),
}

fn segments(source: &str) -> Vec<Segment> {
   let mut result = Vec::new();
   let mut buffer = String::new();
   let mut depth = 0_usize;
   let mut property = false;
   for character in source.chars() {
      if depth > 0 {
         match character {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {},
         }
         if depth == 0 {
            let content = take(&mut buffer);
            result.push(if property {
               Segment::Property(content)
            } else {
               Segment::Metadata(content)
            });
         } else {
            buffer.push(character);
         }
      } else if character == '(' && buffer.ends_with(['$', '%']) {
         property = buffer.pop() == Some('$');
         result.push(Segment::Text(take(&mut buffer)));
         depth = 1;
      } else {
         buffer.push(character);
      }
   }
   result.push(Segment::Text(buffer));
   result
}

fn expand(
   source: &str,
   props: &BTreeMap<String, String>,
   meta: &BTreeMap<String, String>,
) -> io::Result<String> {
   let mut result = String::new();
   for segment in segments(source) {
      match segment {
         Segment::Text(text) => result.push_str(&text),
         Segment::Metadata(name) => result.push_str(meta.get(&name).map_or("", String::as_str)),
         Segment::Property(expression) => {
            let name = expand(&expression, props, meta)?;
            if !name.is_empty()
               && name
                  .chars()
                  .all(|character| character.is_alphanumeric() || character == '_')
            {
               result.push_str(props.get(&name).map_or("", String::as_str));
            } else {
               result.push_str(&string_method(&name, props)?);
            }
         },
      }
   }
   Ok(result)
}

fn string_method(expression: &str, props: &BTreeMap<String, String>) -> io::Result<String> {
   let invalid = || io::Error::other(format!("cannot evaluate $({expression})"));
   let (property, rest) = expression.split_once('.').ok_or_else(invalid)?;
   let (method, arguments) = rest.split_once('(').ok_or_else(invalid)?;
   let args: Vec<&str> = arguments
      .strip_suffix(')')
      .ok_or_else(invalid)?
      .split(',')
      .map(|arg| {
         arg.trim()
            .trim_matches('`')
            .trim_matches('\'')
            .trim_matches('"')
      })
      .collect();
   let value = props.get(property).map_or("", String::as_str);
   let first = args.first().copied().ok_or_else(invalid)?;
   match method {
      "Replace" => Ok(value.replace(first, args.get(1).copied().ok_or_else(invalid)?)),
      "Trim" => {
         let chars = if first.is_empty() { " " } else { first };
         Ok(value
            .trim_matches(|character| chars.contains(character))
            .to_owned())
      },
      "TrimEnd" => {
         Ok(value
            .trim_end_matches(|character| first.contains(character))
            .to_owned())
      },
      "StartsWith" => Ok(value.starts_with(first).to_string()),
      "EndsWith" => Ok(value.ends_with(first).to_string()),
      "LastIndexOf" => {
         Ok(value
            .rfind(first)
            .map_or_else(|| "-1".to_owned(), |index| index.to_string()))
      },
      "Substring" => {
         let from = first.parse::<i64>().map_err(io::Error::other)?;
         let length = i64::try_from(value.len()).map_err(io::Error::other)?;
         let start = if from < 0 { length + from } else { from }.clamp(0, length);
         let limit = match args.get(1) {
            Some(count) => from.saturating_add(count.parse::<i64>().map_err(io::Error::other)?),
            None => length,
         };
         let end = if limit < 0 { length + limit } else { limit }.clamp(start, length);
         let lower = usize::try_from(start).map_err(io::Error::other)?;
         let upper = usize::try_from(end).map_err(io::Error::other)?;
         Ok(String::from_utf8_lossy(&value.as_bytes()[lower..upper]).into_owned())
      },
      _ => Err(invalid()),
   }
}

fn item_refs(spec: &str, items: &[Item]) -> io::Result<String> {
   let mut pieces = spec.split("@(");
   let mut result = pieces.next().unwrap_or_default().to_owned();
   for piece in pieces {
      let invalid = || io::Error::other(format!("invalid item reference @({piece}"));
      let (kind, pattern, rest) = if let Some((kind, transformed)) = piece.split_once("->'") {
         let (pattern, rest) = transformed.split_once("')").ok_or_else(invalid)?;
         (kind, pattern, rest)
      } else {
         let (kind, rest) = piece.split_once(')').ok_or_else(invalid)?;
         (kind, "%(Identity)", rest)
      };
      let matching: Vec<String> = items
         .iter()
         .filter(|item| item.kind == kind)
         .map(|item| pattern.replace("%(Identity)", &item.include))
         .collect();
      result.push_str(&matching.join(";"));
      result.push_str(rest);
   }
   Ok(result)
}

#[derive(PartialEq)]
enum Token {
   Value(String),
   Equal,
   NotEqual,
   Open,
   Close,
   Not,
   And,
   Or,
}

fn lex(source: &str) -> Vec<String> {
   let mut result = Vec::new();
   let mut chars = source.chars().peekable();
   while let Some(character) = chars.next() {
      if character.is_whitespace() {
         continue;
      }
      let mut token = String::from(character);
      if character == '\'' {
         for next in chars.by_ref() {
            token.push(next);
            if next == '\'' {
               break;
            }
         }
      } else if character == '!' || character == '=' {
         if chars.peek() == Some(&'=') {
            token.push('=');
            chars.next();
         } else if character == '=' {
            continue;
         }
      } else if !matches!(character, '(' | ')') {
         while let Some(&next) = chars.peek() {
            if next.is_whitespace() || matches!(next, '(' | ')' | '!' | '=' | '\'') {
               break;
            }
            token.push(next);
            chars.next();
         }
      }
      result.push(token);
   }
   result
}

fn tokens(source: &str) -> Vec<String> {
   let mut result: Vec<String> = Vec::new();
   for segment in segments(source) {
      let (piece, text) = match segment {
         Segment::Text(text) => (text, true),
         Segment::Property(name) => (format!("$({name})"), false),
         Segment::Metadata(name) => (format!("%({name})"), false),
      };
      let literal = result
         .last()
         .is_some_and(|last| last.starts_with('\'') && (last.len() == 1 || !last.ends_with('\'')));
      if literal {
         let close = text.then(|| piece.find('\'')).flatten();
         if let Some(last) = result.last_mut() {
            last.push_str(close.map_or(piece.as_str(), |index| &piece[..=index]));
         }
         if let Some(index) = close {
            result.extend(lex(&piece[index + 1..]));
         }
      } else if text {
         result.extend(lex(&piece));
      } else {
         result.push(piece);
      }
   }
   result
}

fn condition(
   source: &str,
   props: &BTreeMap<String, String>,
   dir: &Path,
   meta: &BTreeMap<String, String>,
) -> io::Result<bool> {
   let mut expanded = Vec::new();
   for token in tokens(source) {
      expanded.push(match token.as_str() {
         "==" => Token::Equal,
         "!=" => Token::NotEqual,
         "(" => Token::Open,
         ")" => Token::Close,
         "!" => Token::Not,
         text if text.eq_ignore_ascii_case("and") => Token::And,
         text if text.eq_ignore_ascii_case("or") => Token::Or,
         text => Token::Value(expand(text.trim_matches('\''), props, meta)?),
      });
   }
   Condition {
      tokens: &expanded,
      index: 0,
      dir,
   }
   .parse_or()
}

struct Condition<'input> {
   tokens: &'input [Token],
   index:  usize,
   dir:    &'input Path,
}

impl Condition<'_> {
   fn parse_or(&mut self) -> io::Result<bool> {
      let mut value = self.parse_and()?;
      while self.tokens.get(self.index) == Some(&Token::Or) {
         self.index += 1;
         value = self.parse_and()? || value;
      }
      Ok(value)
   }

   fn parse_and(&mut self) -> io::Result<bool> {
      let mut value = self.parse_unary()?;
      while self.tokens.get(self.index) == Some(&Token::And) {
         self.index += 1;
         value = self.parse_unary()? && value;
      }
      Ok(value)
   }

   fn operand(&self, index: usize) -> io::Result<&str> {
      let token = self
         .tokens
         .get(index)
         .ok_or_else(|| io::Error::other("missing MSBuild condition operand"))?;
      match *token {
         Token::Value(ref value) => Ok(value),
         Token::Equal
         | Token::NotEqual
         | Token::Open
         | Token::Close
         | Token::Not
         | Token::And
         | Token::Or => Err(io::Error::other("invalid MSBuild condition operand")),
      }
   }

   fn parse_unary(&mut self) -> io::Result<bool> {
      match self.tokens.get(self.index) {
         Some(&Token::Not) => {
            self.index += 1;
            return Ok(!self.parse_unary()?);
         },
         Some(&Token::Open) => {
            self.index += 1;
            let value = self.parse_or()?;
            self.index += 1;
            return Ok(value);
         },
         _ => {},
      }
      let word = self.operand(self.index)?.to_lowercase();
      if word == "exists" || word == "hastrailingslash" {
         let arg = self.operand(self.index + 2)?;
         let value = if word == "exists" {
            Path::new(&win_path(arg, self.dir)?).exists()
         } else {
            arg.ends_with(['\\', '/'])
         };
         self.index += 4;
         return Ok(value);
      }
      let value = match self.tokens.get(self.index + 1) {
         Some(&Token::Equal) => {
            let equal = word == self.operand(self.index + 2)?.to_lowercase();
            self.index += 3;
            equal
         },
         Some(&Token::NotEqual) => {
            let unequal = word != self.operand(self.index + 2)?.to_lowercase();
            self.index += 3;
            unequal
         },
         _ => {
            self.index += 1;
            word == "true"
         },
      };
      Ok(value)
   }
}
