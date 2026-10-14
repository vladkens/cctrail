// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

pub mod claude;
pub mod codex;

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::de::{self, DeserializeOwned, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::model::{Provider, Session, Ts};

/// All sessions of both harnesses, or of one.
pub fn parse_history(provider: Option<Provider>) -> Vec<Session> {
  let claude = || match provider {
    Some(Provider::Codex) => vec![],
    _ => claude::parse(&history_roots("CLAUDE_CONFIG_DIR", ".claude")),
  };
  let codex = || match provider {
    Some(Provider::Claude) => vec![],
    _ => codex::parse(&history_roots("CODEX_HOME", ".codex")),
  };
  let (mut sessions, other) = rayon::join(claude, codex);
  sessions.extend(other);
  sessions
}

/// A harness's history directories: `$var` when set, else `~/<base>` and every `~/<base>-*`
/// profile (`~/.claude-work`).
pub fn history_roots(var: &str, base: &str) -> Vec<PathBuf> {
  match std::env::var_os(var) {
    Some(x) if x.is_empty() => vec![PathBuf::from(".")],
    Some(x) => vec![PathBuf::from(x)],
    None => profiles(&crate::config::home_dir().unwrap_or_else(|| ".".into()), base),
  }
}

/// `<home>/<base>` and its `<home>/<base>-*` sibling directories, sorted.
fn profiles(home: &Path, base: &str) -> Vec<PathBuf> {
  let prefix = format!("{base}-");
  let mut dirs: Vec<PathBuf> = std::fs::read_dir(home)
    .into_iter()
    .flatten()
    .flatten()
    .filter(|x| {
      x.file_name().to_str().is_some_and(|x| x.starts_with(&prefix) && x.len() > prefix.len())
    })
    .map(|x| x.path())
    // `is_dir` follows symlinks: a profile linked to another directory counts too.
    .filter(|x| x.is_dir())
    .collect();
  dirs.sort();
  dirs.insert(0, home.join(base));
  dirs
}

/// A tool call that may have spawned an agent.
#[derive(Clone, Debug)]
pub struct AgentCall {
  pub tool_id: String,
  pub agent_id: Option<String>,
  pub resolved: bool,
}

impl AgentCall {
  pub fn new(tool_id: &str) -> Self {
    Self { tool_id: tool_id.to_string(), agent_id: None, resolved: false }
  }
}

/// Gives every session the id and working directory of the main session at the top of its parents.
pub fn inherit_roots(sessions: &mut [Session], parents: &HashMap<usize, usize>) {
  for child in 0..sessions.len() {
    let mut root = child;
    for _ in 0..sessions.len() {
      match parents.get(&root) {
        Some(x) => root = *x,
        None => break,
      }
    }
    sessions[child].root_id = sessions[root].id.clone();
    sessions[child].workdir = sessions[root].workdir.clone();
  }
}

/// Every `*.jsonl` file under the roots, sorted by path components.
pub fn jsonl_files(roots: &[PathBuf]) -> Vec<PathBuf> {
  fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
      let path = entry.path();
      match entry.file_type() {
        Ok(x) if x.is_dir() => walk(&path, files),
        _ if path.to_string_lossy().ends_with(".jsonl") && path.is_file() => files.push(path),
        _ => {}
      }
    }
  }

  let mut files = vec![];
  for root in roots.iter().filter(|x| x.is_dir()) {
    walk(root, &mut files);
  }
  files.sort();
  files
}

/// Calls `f` with every line of a JSONL file that is a JSON object, and the line itself, until
/// `f` returns false. Unreadable files and lines that don't parse are skipped.
pub fn jsonl<T: DeserializeOwned>(path: &Path, mut f: impl FnMut(T, &[u8]) -> bool) {
  let Ok(file) = File::open(path) else { return };
  let mut reader = BufReader::with_capacity(1 << 16, file);
  let mut buf = vec![];
  loop {
    buf.clear();
    match reader.read_until(b'\n', &mut buf) {
      Ok(0) | Err(_) => return,
      Ok(_) => {}
    }
    if let Ok(Obj(Some(x))) = serde_json::from_slice::<Obj<T>>(&buf)
      && !f(x, &buf)
    {
      return;
    }
  }
}

/// Python's `datetime.fromisoformat` after `Z` → `+00:00`; a time without offset is UTC.
pub fn parse_ts(value: &Val) -> Option<Ts> {
  let s = value.str()?.replace('Z', "+00:00");
  if let Ok(x) = s.parse::<jiff::Timestamp>() {
    return Some(x.as_microsecond());
  }
  let dt = s.parse::<jiff::civil::DateTime>().ok()?;
  dt.to_zoned(jiff::tz::TimeZone::UTC).ok().map(|x| x.timestamp().as_microsecond())
}

/// Python's `str.isspace` for one character.
fn is_space(c: char) -> bool {
  c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

// MARK: Lenient JSON
//
// History lines are checked the way the Python version checked dicts: a field of an unexpected
// type counts as missing instead of failing the line. Values not declared are skipped unparsed.

macro_rules! ignore_scalars {
  () => {
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
      Ok(Default::default())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
      Ok(Default::default())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
      Ok(Default::default())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
      Ok(Default::default())
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
      Ok(Default::default())
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
      Ok(Default::default())
    }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
      Ok(Default::default())
    }
  };
}

fn drain_seq<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<bool, A::Error> {
  let mut any = false;
  while seq.next_element::<IgnoredAny>()?.is_some() {
    any = true;
  }
  Ok(any)
}

fn drain_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<bool, A::Error> {
  let mut any = false;
  while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {
    any = true;
  }
  Ok(any)
}

/// A scalar field; containers keep only whether they are empty.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Val {
  #[default]
  Null,
  Bool(bool),
  Int(i128),
  Float(f64),
  Str(String),
  List(bool),
  Map(bool),
}

impl Val {
  pub fn str(&self) -> Option<&str> {
    match self {
      Self::Str(x) => Some(x),
      _ => None,
    }
  }

  /// A non-negative integer, else 0.
  pub fn int(&self) -> u64 {
    match self {
      Self::Int(x) if *x >= 0 => u64::try_from(*x).unwrap_or(u64::MAX),
      _ => 0,
    }
  }

  pub fn ordinal(&self) -> Option<i64> {
    match self {
      Self::Int(x) => i64::try_from(*x).ok(),
      _ => None,
    }
  }

  /// Python's `bool(value)`.
  pub fn truthy(&self) -> bool {
    match self {
      Self::Null => false,
      Self::Bool(x) | Self::List(x) | Self::Map(x) => *x,
      Self::Int(x) => *x != 0,
      Self::Float(x) => *x != 0.0,
      Self::Str(x) => !x.is_empty(),
    }
  }
}

impl<'de> Deserialize<'de> for Val {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
      type Value = Val;
      fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
      }
      fn visit_bool<E: de::Error>(self, x: bool) -> Result<Val, E> {
        Ok(Val::Bool(x))
      }
      fn visit_i64<E: de::Error>(self, x: i64) -> Result<Val, E> {
        Ok(Val::Int(x.into()))
      }
      fn visit_u64<E: de::Error>(self, x: u64) -> Result<Val, E> {
        Ok(Val::Int(x.into()))
      }
      fn visit_f64<E: de::Error>(self, x: f64) -> Result<Val, E> {
        Ok(Val::Float(x))
      }
      fn visit_str<E: de::Error>(self, x: &str) -> Result<Val, E> {
        Ok(Val::Str(x.to_string()))
      }
      fn visit_string<E: de::Error>(self, x: String) -> Result<Val, E> {
        Ok(Val::Str(x))
      }
      fn visit_unit<E: de::Error>(self) -> Result<Val, E> {
        Ok(Val::Null)
      }
      fn visit_none<E: de::Error>(self) -> Result<Val, E> {
        Ok(Val::Null)
      }
      fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Val, A::Error> {
        Ok(Val::List(drain_seq(seq)?))
      }
      fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Val, A::Error> {
        Ok(Val::Map(drain_map(map)?))
      }
    }
    d.deserialize_any(V)
  }
}

/// A text field read without copying it: whether it is a string and empty or blank.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Text {
  #[default]
  Missing,
  Str {
    empty: bool,
    blank: bool,
  },
  Other(bool),
}

impl Text {
  /// A string with more than whitespace.
  pub fn has_text(self) -> bool {
    matches!(self, Self::Str { blank: false, .. })
  }

  pub fn truthy(self) -> bool {
    match self {
      Self::Missing => false,
      Self::Str { empty, .. } => !empty,
      Self::Other(x) => x,
    }
  }
}

impl<'de> Deserialize<'de> for Text {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    let x = match Content::<IgnoredAny>::deserialize(d)? {
      Content::Text(x) => x,
      Content::Blocks(x) => Text::Other(!x.is_empty()),
      Content::Other(x) => Text::Other(x),
    };
    Ok(x)
  }
}

/// A message's content: a string or a list of blocks.
#[derive(Clone, Debug, PartialEq)]
pub enum Content<T> {
  Text(Text),
  Blocks(Vec<Obj<T>>),
  Other(bool),
}

impl<T> Default for Content<T> {
  fn default() -> Self {
    Self::Text(Text::Missing)
  }
}

impl<T> Content<T> {
  pub fn blocks(&self) -> impl Iterator<Item = &T> {
    let blocks = match self {
      Self::Blocks(x) => x.as_slice(),
      _ => &[],
    };
    blocks.iter().filter_map(|x| x.0.as_ref())
  }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Content<T> {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V<T>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
      type Value = Content<T>;
      fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
      }
      fn visit_bool<E: de::Error>(self, x: bool) -> Result<Self::Value, E> {
        Ok(Content::Other(x))
      }
      fn visit_i64<E: de::Error>(self, x: i64) -> Result<Self::Value, E> {
        Ok(Content::Other(x != 0))
      }
      fn visit_u64<E: de::Error>(self, x: u64) -> Result<Self::Value, E> {
        Ok(Content::Other(x != 0))
      }
      fn visit_f64<E: de::Error>(self, x: f64) -> Result<Self::Value, E> {
        Ok(Content::Other(x != 0.0))
      }
      fn visit_str<E: de::Error>(self, x: &str) -> Result<Self::Value, E> {
        Ok(Content::Text(Text::Str { empty: x.is_empty(), blank: x.chars().all(is_space) }))
      }
      fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Content::Other(false))
      }
      fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Content::Other(false))
      }
      fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = vec![];
        while let Some(x) = seq.next_element::<Obj<T>>()? {
          items.push(x);
        }
        Ok(Content::Blocks(items))
      }
      fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        Ok(Content::Other(drain_map(map)?))
      }
    }
    d.deserialize_any(V(PhantomData))
  }
}

/// A JSON object read as `T`; any other value counts as missing.
#[derive(Clone, Debug, PartialEq)]
pub struct Obj<T>(pub Option<T>);

impl<T> Default for Obj<T> {
  fn default() -> Self {
    Self(None)
  }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Obj<T> {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V<T>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
      type Value = Obj<T>;
      fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
      }
      ignore_scalars!();
      fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
        drain_seq(seq)?;
        Ok(Obj(None))
      }
      fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        T::deserialize(de::value::MapAccessDeserializer::new(map)).map(|x| Obj(Some(x)))
      }
    }
    d.deserialize_any(V(PhantomData))
  }
}

/// Whether a field is present at all, `null` included; its value is skipped.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Present(pub bool);

impl<'de> Deserialize<'de> for Present {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    IgnoredAny::deserialize(d)?;
    Ok(Present(true))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Deserialize, Default, Debug)]
  #[serde(default)]
  struct Line {
    a: Val,
    b: Text,
    c: Obj<Inner>,
    d: Present,
    e: Content<Inner>,
  }

  #[derive(Deserialize, Default, Debug, PartialEq)]
  #[serde(default)]
  struct Inner {
    x: Val,
  }

  fn line(s: &str) -> Line {
    serde_json::from_str::<Obj<Line>>(s).unwrap().0.unwrap()
  }

  #[test]
  fn reads_fields_leniently() {
    let x =
      line(r#"{"a": 5, "b": "  ", "c": [1], "d": null, "e": [{"x": "y"}, 3], "z": {"q": 1}}"#);
    assert_eq!((x.a.int(), x.a.str()), (5, None));
    assert_eq!((x.b.truthy(), x.b.has_text()), (true, false));
    assert_eq!(x.c, Obj(None));
    assert_eq!(x.d, Present(true));
    assert_eq!(x.e.blocks().collect::<Vec<_>>(), vec![&Inner { x: Val::Str("y".into()) }]);

    let x = line(r#"{"a": true, "b": 0, "c": {"x": -1}, "e": "hi"}"#);
    assert_eq!((x.a.int(), x.a.truthy()), (0, true));
    assert_eq!((x.b.truthy(), x.b.has_text()), (false, false));
    assert_eq!(x.c.0.unwrap().x.ordinal(), Some(-1));
    assert_eq!(x.d, Present(false));
    assert_eq!(x.e, Content::Text(Text::Str { empty: false, blank: false }));

    let x = line(r#"{"a": 1.0, "b": "\u001f x"}"#);
    assert_eq!((x.a.int(), x.a.truthy(), x.b.has_text()), (0, true, true));
    assert!(!line(r#"{"b": "\u001f\n"}"#).b.has_text());
    assert!(serde_json::from_str::<Obj<Line>>("[1, 2]").unwrap().0.is_none());
  }

  #[test]
  fn finds_profile_directories() {
    let home = std::env::temp_dir().join(format!("cctrail-profiles-{}", std::process::id()));
    for x in [".claude", ".claude-work", ".claude-b", ".claudex", ".claude-", ".codex-alt"] {
      std::fs::create_dir_all(home.join(x)).unwrap();
    }
    std::fs::write(home.join(".claude-file"), "").unwrap();
    // A profile linked to another directory counts too.
    #[cfg(unix)]
    std::os::unix::fs::symlink(home.join(".claude-work"), home.join(".claude-link")).unwrap();
    let names = |base: &str| -> Vec<String> {
      let dirs = profiles(&home, base);
      dirs.iter().map(|x| x.file_name().unwrap().to_string_lossy().to_string()).collect()
    };
    let expected: &[&str] = match cfg!(unix) {
      true => &[".claude", ".claude-b", ".claude-link", ".claude-work"],
      false => &[".claude", ".claude-b", ".claude-work"],
    };
    assert_eq!(names(".claude"), expected);
    // The base itself needn't exist.
    assert_eq!(names(".codex"), [".codex", ".codex-alt"]);
    std::fs::remove_dir_all(&home).unwrap();
  }

  #[test]
  fn parses_timestamps_as_python() {
    let ts = |s: &str| parse_ts(&Val::Str(s.into()));
    let base = ts("2026-01-01T00:00:00Z").unwrap();
    assert_eq!(ts("2026-01-01T00:00:00+00:00"), Some(base));
    assert_eq!(ts("2026-01-01T00:00:00"), Some(base));
    assert_eq!(ts("2026-01-01T03:00:00+03:00"), Some(base));
    assert_eq!(ts("2026-01-01T00:00:00.1234567Z"), Some(base + 123_456));
    assert_eq!(ts("nope"), None);
    assert_eq!(parse_ts(&Val::Int(1)), None);
  }
}
