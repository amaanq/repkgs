//! The subset of PEP 508 environment markers uv writes into uv.lock,
//! `python_version` and friends compared with == != < <= > >= ~= in "not in",
//! combined with and / or / not and parentheses.

use std::{
   cmp::Ordering,
   collections::BTreeMap,
   io,
   iter::repeat,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
   Arbitrary,
   Equal,
   NotEqual,
   LessEqual,
   GreaterEqual,
   Compatible,
   Less,
   Greater,
   In,
   NotIn,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token<'marker> {
   Literal(&'marker str),
   Open,
   Close,
   Not,
   And,
   Or,
   Op(Op),
   Name(&'marker str),
}

/// Longest first, so `<=` is not read as `<`.
const OPERATORS: [(&str, Op); 8] = [
   ("===", Op::Arbitrary),
   ("==", Op::Equal),
   ("!=", Op::NotEqual),
   ("<=", Op::LessEqual),
   (">=", Op::GreaterEqual),
   ("~=", Op::Compatible),
   ("<", Op::Less),
   (">", Op::Greater),
];

fn tokenize(text: &str) -> io::Result<Vec<Token<'_>>> {
   let mut tokens = Vec::new();
   let mut rest = text;
   while let Some(first) = rest.chars().next() {
      if first.is_whitespace() {
         rest = &rest[first.len_utf8()..];
         continue;
      }
      if first == '\'' || first == '"' {
         let body = &rest[1..];
         let end = body
            .find(first)
            .ok_or_else(|| io::Error::other(format!("pep508: unterminated string in {text}")))?;
         tokens.push(Token::Literal(&body[..end]));
         rest = &body[end + 1..];
         continue;
      }
      if first == '(' {
         tokens.push(Token::Open);
         rest = &rest[1..];
         continue;
      }
      if first == ')' {
         tokens.push(Token::Close);
         rest = &rest[1..];
         continue;
      }
      if let Some(&(spelled, op)) = OPERATORS
         .iter()
         .find(|&&(spelled, _)| rest.starts_with(spelled))
      {
         tokens.push(Token::Op(op));
         rest = &rest[spelled.len()..];
         continue;
      }
      let end = rest
         .find(|character: char| !(character.is_alphanumeric() || character == '_'))
         .unwrap_or(rest.len());
      if end == 0 {
         return Err(io::Error::other(format!(
            "pep508: unexpected {first} in {text}"
         )));
      }
      let word = &rest[..end];
      rest = &rest[end..];
      tokens.push(match word {
         "not" => {
            let after = rest.trim_start();
            if let Some(tail) = after.strip_prefix("in")
               && !tail
                  .starts_with(|character: char| character.is_alphanumeric() || character == '_')
            {
               rest = tail;
               Token::Op(Op::NotIn)
            } else {
               Token::Not
            }
         },
         "in" => Token::Op(Op::In),
         "and" => Token::And,
         "or" => Token::Or,
         _ => Token::Name(word),
      });
   }
   Ok(tokens)
}

/// "3.14.7" -> [3, 14, 7], a non-numeric tail in a component counts as 0.
fn release(version: &str) -> Vec<u64> {
   version
      .split('.')
      .map(|part| {
         let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
         digits.parse().unwrap_or(0)
      })
      .collect()
}

fn version_order(lhs: &str, rhs: &str) -> Ordering {
   let left = release(lhs);
   let right = release(rhs);
   let width = left.len().max(right.len());
   let padded = |parts: Vec<u64>| parts.into_iter().chain(repeat(0)).take(width);
   padded(left).cmp(padded(right))
}

fn looks_like_version(text: &str) -> bool {
   text.starts_with(|character: char| character.is_ascii_digit())
}

/// string operators always, ordering only between version-looking operands.
fn compare(lhs: &str, op: Op, rhs: &str) -> bool {
   let versions = looks_like_version(lhs) && looks_like_version(rhs);
   match op {
      Op::In => rhs.contains(lhs),
      Op::NotIn => !rhs.contains(lhs),
      Op::Equal | Op::Arbitrary if !versions => lhs == rhs,
      Op::NotEqual if !versions => lhs != rhs,
      Op::Compatible | Op::Less | Op::LessEqual | Op::Greater | Op::GreaterEqual if !versions => {
         false
      },
      Op::Compatible => {
         let wanted = release(rhs);
         let prefix = wanted.len().saturating_sub(1);
         version_order(lhs, rhs) != Ordering::Less
            && release(lhs)
               .iter()
               .take(prefix)
               .eq(wanted.iter().take(prefix))
      },
      Op::Equal | Op::NotEqual if rhs.ends_with(".*") => {
         let prefix = release(rhs.strip_suffix(".*").unwrap_or(rhs));
         release(lhs).iter().take(prefix.len()).eq(prefix.iter()) == (op == Op::Equal)
      },
      Op::Equal | Op::Arbitrary => version_order(lhs, rhs) == Ordering::Equal,
      Op::NotEqual => version_order(lhs, rhs) != Ordering::Equal,
      Op::Less => version_order(lhs, rhs) == Ordering::Less,
      Op::LessEqual => version_order(lhs, rhs) != Ordering::Greater,
      Op::Greater => version_order(lhs, rhs) == Ordering::Greater,
      Op::GreaterEqual => version_order(lhs, rhs) != Ordering::Less,
   }
}

struct Parser<'marker> {
   tokens: Vec<Token<'marker>>,
   vars:   &'marker BTreeMap<String, String>,
   at:     usize,
}

impl<'marker> Parser<'marker> {
   fn peek(&self) -> Option<Token<'marker>> {
      self.tokens.get(self.at).copied()
   }

   fn parse_or(&mut self) -> io::Result<bool> {
      let mut value = self.parse_and()?;
      while self.peek() == Some(Token::Or) {
         self.at += 1;
         let rhs = self.parse_and()?;
         value = value || rhs;
      }
      Ok(value)
   }

   fn parse_and(&mut self) -> io::Result<bool> {
      let mut value = self.parse_atom()?;
      while self.peek() == Some(Token::And) {
         self.at += 1;
         let rhs = self.parse_atom()?;
         value = value && rhs;
      }
      Ok(value)
   }

   fn operand(&mut self) -> io::Result<String> {
      let token = self
         .peek()
         .ok_or_else(|| io::Error::other("pep508: marker ends early"))?;
      self.at += 1;
      match token {
         Token::Literal(text) => Ok(text.to_owned()),
         Token::Name(name) => {
            self
               .vars
               .get(name)
               .cloned()
               .ok_or_else(|| io::Error::other(format!("pep508: unknown marker variable {name}")))
         },
         Token::Open | Token::Close | Token::Not | Token::And | Token::Or | Token::Op(_) => {
            Err(io::Error::other("pep508: operand expected"))
         },
      }
   }

   /// ( expr ) | not atom | lhs op rhs.
   fn parse_atom(&mut self) -> io::Result<bool> {
      match self.peek() {
         Some(Token::Open) => {
            self.at += 1;
            let inner = self.parse_or()?;
            if self.peek() != Some(Token::Close) {
               return Err(io::Error::other("pep508: missing )"));
            }
            self.at += 1;
            Ok(inner)
         },
         Some(Token::Not) => {
            self.at += 1;
            Ok(!self.parse_atom()?)
         },
         _ => {
            let lhs = self.operand()?;
            let Some(Token::Op(op)) = self.peek() else {
               return Err(io::Error::other("pep508: operator expected"));
            };
            self.at += 1;
            let rhs = self.operand()?;
            Ok(compare(&lhs, op, &rhs))
         },
      }
   }
}

/// true when `marker` holds for `vars`.
pub fn evaluate(marker: &str, vars: &BTreeMap<String, String>) -> io::Result<bool> {
   let mut parser = Parser {
      tokens: tokenize(marker)?,
      vars,
      at: 0,
   };
   let value = parser.parse_or()?;
   if parser.at != parser.tokens.len() {
      return Err(io::Error::other(format!(
         "pep508: trailing tokens in {marker}"
      )));
   }
   Ok(value)
}
