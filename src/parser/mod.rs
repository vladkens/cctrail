// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

pub mod claude;
pub mod codex;
pub mod fx;
pub mod pi;

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rayon::prelude::*;
use serde::de::{self, DeserializeOwned, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::model::{Event, Provider, Session, Ts};
use crate::swear::Dict;

/// The harnesses asked for, each with its history roots.
pub fn harnesses(provider: Option<Provider>) -> Vec<(Provider, Vec<PathBuf>)> {
  let all = [Provider::Claude, Provider::Codex, Provider::Pi, Provider::Fx];
  let roots = |x: Provider| match x {
    Provider::Claude => history_roots("CLAUDE_CONFIG_DIR", ".claude"),
    Provider::Codex => history_roots("CODEX_HOME", ".codex"),
    Provider::Pi => pi::roots(),
    Provider::Fx => vec![fx::root()],
  };
  all.into_iter().filter(|x| provider.is_none_or(|p| p == *x)).map(|x| (x, roots(x))).collect()
}

/// All sessions of every harness, or of one; with a dictionary, the swearing in their main
/// sessions' prompts too. Through the ledger unless `CCTRAIL_NO_LEDGER` is set.
pub fn parse_history(provider: Option<Provider>, swears: Option<&Dict>) -> Result<Vec<Session>> {
  let harnesses = harnesses(provider);
  if crate::ledger::enabled() {
    return crate::ledger::sessions(&harnesses, swears);
  }
  let sessions = harnesses.par_iter().map(|(p, roots)| {
    let sources = find(*p, roots).into_par_iter().map(|x| x.read(swears, 0)).collect();
    combine(*p, roots, sources)
  });
  Ok(sessions.collect::<Vec<_>>().concat())
}

// MARK: Sources

/// What reading one history file alone gives. The work across files (copies, agents, forks) runs
/// on these, so the ledger keeps them for the files read before and those gone since.
#[derive(Clone, Debug, Default)]
pub struct Unit {
  pub session: Option<Session>,
  pub links: Links,
}

/// What a harness's work across files needs of a file besides its session.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Links {
  #[default]
  None,
  Claude(claude::Links),
  Codex(codex::Links),
  Pi(pi::Links),
  Fx(fx::Links),
}

/// A history file (fx: a session directory) under one of its harness's roots, and what reading it
/// gave: a file of the ledger.
#[derive(Clone, Debug)]
pub struct Source {
  pub provider: Provider,
  pub root: String,
  pub path: PathBuf,
  pub size: u64,
  /// Nanoseconds since the Unix epoch.
  pub mtime: i64,
  /// The parser version and swear dictionary it was read with.
  pub version: (i64, i64),
  pub unit: Unit,
}

impl Source {
  /// A file found, not read yet; with the files beside it that its reading takes in, their sizes
  /// summed and the latest mtime.
  pub fn found(provider: Provider, root: &Path, path: PathBuf, beside: &[PathBuf]) -> Self {
    let (mut size, mut mtime) = (0u64, 0i64);
    for x in std::iter::once(&path).chain(beside).filter_map(|x| std::fs::metadata(x).ok()) {
      size = size.saturating_add(x.len());
      let at = x.modified().ok().and_then(|x| x.duration_since(std::time::UNIX_EPOCH).ok());
      mtime = mtime.max(at.map_or(0, |x| i64::try_from(x.as_nanos()).unwrap_or(i64::MAX)));
    }
    let root = root.to_string_lossy().to_string();
    Self { provider, root, path, size, mtime, version: (0, 0), unit: Unit::default() }
  }

  pub fn read(mut self, swears: Option<&Dict>, dict: i64) -> Self {
    self.unit = match self.provider {
      Provider::Claude => claude::read(&self.path, swears),
      Provider::Codex => codex::read(&self.path, swears),
      Provider::Pi => pi::read(&self.path, swears),
      Provider::Fx => fx::read(&self.path, swears),
    };
    self.version = (crate::ledger::PARSER_VERSION, dict);
    self
  }
}

pub fn find(provider: Provider, roots: &[PathBuf]) -> Vec<Source> {
  match provider {
    Provider::Claude => claude::find(roots),
    Provider::Codex => codex::find(roots),
    Provider::Pi => pi::find(roots),
    Provider::Fx => fx::find(&roots[0]),
  }
}

/// A harness's sessions read without the ledger.
#[cfg(test)]
pub fn parse(provider: Provider, roots: &[PathBuf], swears: Option<&Dict>) -> Vec<Session> {
  let sources = find(provider, roots).into_iter().map(|x| x.read(swears, 0));
  combine(provider, roots, sources.collect())
}

/// A harness's sessions from its sources, in the order its files are found: by root (those of
/// roots gone since last), then path.
pub fn combine(provider: Provider, roots: &[PathBuf], mut sources: Vec<Source>) -> Vec<Session> {
  let order = match provider {
    Provider::Claude => claude::dirs(roots),
    Provider::Codex => roots.to_vec(),
    // Found across the roots by path alone.
    Provider::Pi | Provider::Fx => vec![],
  };
  let order: Vec<String> = order.iter().map(|x| x.to_string_lossy().to_string()).collect();
  let rank = |x: &Source| order.iter().position(|r| *r == x.root).unwrap_or(order.len());
  sources.sort_by(|a, b| (rank(a), &a.root, &a.path).cmp(&(rank(b), &b.root, &b.path)));
  match provider {
    Provider::Claude => claude::combine(sources),
    Provider::Codex => codex::combine(sources),
    Provider::Pi => pi::combine(sources),
    Provider::Fx => fx::combine(sources),
  }
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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

// MARK: Prompts

/// Tags a harness wraps around what it adds to a prompt: Claude Code's slash commands, shell
/// commands, reminders and notifications, Codex's context and answers to its questions, pi's
/// skills. Their spans are not the user's words.
const INJECTED: &str = "system-reminder command-name command-message command-args \
  local-command-caveat local-command-stdout local-command-stderr bash-input bash-stdout \
  bash-stderr task-notification user-prompt-submit-hook ide_opened_file ide_selection \
  environment_context user_instructions user_action user_shell_command turn_aborted \
  subagent_notification send_user_message_question_reply realtime_delegation \
  codex_internal_context recommended_plugins image skill";

/// The user's own words in a message's content (a string or text blocks): without the spans of
/// injected tags (to the end when one isn't closed), trimmed; none when nothing is left.
pub fn own_words(content: &Value) -> Option<String> {
  let mut text = match content {
    Value::String(x) => x.clone(),
    Value::Array(x) => {
      let texts = x.iter().filter(|x| x["type"] == "text").filter_map(|x| x["text"].as_str());
      texts.collect::<Vec<_>>().join("\n")
    }
    _ => return None,
  };
  for tag in INJECTED.split_whitespace() {
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut from = 0;
    while let Some(i) = text[from..].find(&open).map(|x| x + from) {
      from = i + open.len();
      if !text[from..].starts_with(['>', ' ', '\n']) {
        continue;
      }
      let end = text[from..].find(&close).map_or(text.len(), |x| from + x + close.len());
      text.replace_range(i..end, "\n");
      from = i;
    }
  }
  let text = text.trim();
  (!text.is_empty()).then(|| text.to_string())
}

/// The model each prompt of a session counts toward: the last to answer before it, the first to
/// answer for those before any answer.
#[derive(Default)]
pub struct Replies {
  first: Option<String>,
  last: Option<String>,
}

impl Replies {
  pub fn answer(&mut self, model: &str) {
    self.first.get_or_insert_with(|| model.to_string());
    if self.last.as_deref() != Some(model) {
      self.last = Some(model.to_string());
    }
  }

  /// Counts the swearing in a prompt's words into its event; the words themselves are dropped.
  pub fn prompt(&self, event: &mut Event, dict: &Dict, ts: Ts, words: &str) {
    let mut x = dict.prompt(ts, words);
    x.model.clone_from(&self.last);
    event.prompt = Some(Box::new(x));
  }

  /// Gives the prompts before the first answer its model.
  pub fn finish<'a>(&self, events: impl IntoIterator<Item = &'a mut Event>) {
    let prompts = events.into_iter().filter_map(|x| x.prompt.as_mut());
    for x in prompts.filter(|x| x.model.is_none()) {
      x.model.clone_from(&self.first);
    }
  }
}

// MARK: Models

/// Bedrock's model vendors: its ids are `[<region>.]<vendor>.<model>`.
const VENDORS: [&str; 10] = [
  "anthropic",
  "openai",
  "amazon",
  "meta",
  "mistral",
  "cohere",
  "deepseek",
  "qwen",
  "ai21",
  "writer",
];

/// A model id as pi and fx log it from any provider, as the price catalog names it: without a
/// router's vendor (`anthropic/claude-sonnet-4.5`) or fx's provider (`codex/gpt-6-astra`),
/// Bedrock's region, vendor and version (`us.anthropic.claude-opus-4-6-v1:0`,
/// `global.openai.gpt-6-astra`), with Vertex's date as theirs (`claude-sonnet-4@20250514` as
/// `claude-sonnet-4-20250514`), and with dashes in Claude's version (`claude-sonnet-4.5`). One
/// model reads the same by any route.
pub fn model_id(model: &str) -> String {
  let digits = |x: &str| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit());
  let id = model.rsplit('/').next().unwrap_or(model);
  // Vertex's date as Anthropic's and Bedrock's: some models have no undated id.
  let id = id.replacen('@', "-", 1);
  let id = id.as_str();
  let vendor = |v: &str| match id.strip_prefix(v).and_then(|x| x.strip_prefix('.')) {
    Some(x) => Some(id.len() - x.len()),
    None => id.find(&format!(".{v}.")).map(|i| i + v.len() + 2),
  };
  let id = VENDORS.iter().filter_map(|v| vendor(v)).min().map_or(id, |i| &id[i..]);
  let id = id.rsplit_once(':').filter(|x| digits(x.1)).map_or(id, |x| x.0);
  let id = id.rsplit_once("-v").filter(|x| digits(x.1)).map_or(id, |x| x.0);
  match id.starts_with("claude-") {
    true => id.replace('.', "-"),
    false => id.to_string(),
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

  pub fn float(&self) -> Option<f64> {
    match self {
      Self::Int(x) => Some(*x as f64),
      Self::Float(x) => Some(*x),
      _ => None,
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

  /// The prompts of main sessions as (model, words, swear words), in order.
  pub(super) fn prompts(sessions: &[Session]) -> Vec<(&str, u32, u32)> {
    let events = sessions.iter().filter(|x| x.is_main).flat_map(|x| &x.events);
    let prompts = events.filter_map(|x| x.prompt.as_deref());
    prompts.map(|x| (x.model.as_deref().unwrap_or("-"), x.words, x.swears)).collect()
  }

  pub(super) fn dict() -> Dict {
    Dict::load(None).unwrap()
  }

  #[test]
  fn keeps_only_the_users_own_words() {
    let words = |x: Value| own_words(&x);
    let text = |x: &str| words(Value::String(x.into()));
    assert_eq!(text("  fix it \n"), Some("fix it".into()));
    // Injected spans anywhere, with attributes, an unclosed one to the end.
    let x = "<command-name>/x</command-name>\n<command-args>a</command-args>";
    assert_eq!(text(x), None);
    assert_eq!(text("fix <system-reminder>be nice</system-reminder> it"), Some("fix \n it".into()));
    assert_eq!(text("<skill name=\"a\" location=\"/s\">body</skill>\n\ngo"), Some("go".into()));
    assert_eq!(text("go <environment_context>cwd"), Some("go".into()));
    // Other tags are the user's: pasted markup, a tag that only starts alike.
    assert_eq!(text("<div>x</div>"), Some("<div>x</div>".into()));
    assert_eq!(text("<images>x</images>"), Some("<images>x</images>".into()));
    // Text blocks joined by lines; other blocks and values skipped.
    let blocks = serde_json::json!([
      {"type": "text", "text": "a"},
      {"type": "image", "text": "b"},
      {"type": "text", "text": "<turn_aborted>x</turn_aborted>"},
      {"type": "text", "text": "c"},
    ]);
    assert_eq!(words(blocks), Some("a\n\n\nc".into()));
    assert_eq!(words(Value::Null), None);
    assert_eq!(words(serde_json::json!(5)), None);
  }

  #[test]
  fn counts_prompts_toward_the_last_answer() {
    let dict = dict();
    let mut events = vec![Event::default(); 4];
    let mut replies = Replies::default();
    replies.prompt(&mut events[0], &dict, 1, "fuck it, go");
    replies.answer("x");
    replies.prompt(&mut events[1], &dict, 2, "b");
    replies.answer("y");
    replies.answer("y");
    replies.prompt(&mut events[2], &dict, 3, "c");
    replies.finish(&mut events);
    let models: Vec<_> =
      events.iter().map(|x| x.prompt.as_ref().map(|x| x.model.as_deref())).collect();
    // The first prompt counts toward the first to answer; an event without a prompt has none.
    assert_eq!(models, [Some(Some("x")), Some(Some("x")), Some(Some("y")), None]);
    let first = events[0].prompt.as_deref().unwrap();
    assert_eq!((first.ts, first.words, first.swears), (1, 3, 1));
    // The events' own models stay as logged.
    assert!(events.iter().all(|x| x.model.is_none()));

    // Without any answer prompts count toward none.
    let mut events = vec![Event::default()];
    let replies = Replies::default();
    replies.prompt(&mut events[0], &dict, 1, "a");
    replies.finish(&mut events);
    assert_eq!(events[0].prompt.as_ref().unwrap().model, None);
  }

  #[test]
  fn names_a_model_alike_by_any_route() {
    let ids = [
      ("us.anthropic.claude-opus-4-6-v1", "claude-opus-4-6"),
      ("anthropic.claude-sonnet-4-5-20250929-v1:0", "claude-sonnet-4-5-20250929"),
      ("anthropic/claude-sonnet-4.5", "claude-sonnet-4-5"),
      ("claude-opus-4-1@20250805", "claude-opus-4-1-20250805"),
      ("claude-3-7-sonnet@20250219", "claude-3-7-sonnet-20250219"),
      ("claude-sonnet-4@20250514", "claude-sonnet-4-20250514"),
      ("anthropic.claude-sonnet-4-20250514-v1:0", "claude-sonnet-4-20250514"),
      ("openai/gpt-6-astra", "gpt-6-astra"),
      ("gpt-5.6-sol", "gpt-5.6-sol"),
      ("google/gemini-4-argon", "gemini-4-argon"),
      ("openai.gpt-5.4", "gpt-5.4"),
      ("global.openai.gpt-6-astra", "gpt-6-astra"),
      ("apac.anthropic.claude-sonnet-4-5-20250929-v1:0", "claude-sonnet-4-5-20250929"),
      ("eu.amazon.nova-pro-v1:0", "nova-pro"),
      ("glm-5", "glm-5"),
      ("codex/gpt-6-astra", "gpt-6-astra"),
      ("gateway/anthropic/claude-sonnet-4.5", "claude-sonnet-4-5"),
    ];
    for (logged, id) in ids {
      assert_eq!(model_id(logged), id);
    }
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
