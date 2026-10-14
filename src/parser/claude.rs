// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Deserialize;

use super::{AgentCall, Content, Obj, Text, Val, inherit_roots, jsonl, jsonl_files, parse_ts};
use crate::model::{Event, Provider, Session, Speed, Ts, Usage};

// MARK: Lines

#[derive(Deserialize, Default)]
#[serde(default)]
struct Line {
  #[serde(rename = "type")]
  kind: Val,
  cwd: Val,
  timestamp: Val,
  #[serde(rename = "requestId")]
  request: Val,
  uuid: Val,
  #[serde(rename = "isMeta")]
  meta: Val,
  #[serde(rename = "isSidechain")]
  sidechain: Val,
  #[serde(rename = "isApiErrorMessage")]
  api_error: Val,
  message: Obj<Message>,
  #[serde(rename = "toolUseResult")]
  result: Obj<ToolResult>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Message {
  id: Val,
  model: Val,
  content: Content<Block>,
  usage: Obj<RawUsage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Block {
  #[serde(rename = "type")]
  kind: Val,
  id: Val,
  name: Val,
  tool_use_id: Val,
  text: Text,
  input_text: Text,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ToolResult {
  #[serde(rename = "agentId")]
  agent_id: Val,
  usage: Obj<RawUsage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUsage {
  #[serde(rename = "type")]
  kind: Val,
  input_tokens: Val,
  cache_creation_input_tokens: Val,
  cache_read_input_tokens: Val,
  output_tokens: Val,
  cache_creation: Obj<CacheCreation>,
  iterations: Content<RawUsage>,
  speed: Val,
  service_tier: Val,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CacheCreation {
  ephemeral_5m_input_tokens: Val,
  ephemeral_1h_input_tokens: Val,
}

impl RawUsage {
  fn usage(&self) -> Usage {
    let cache = self.cache_creation.0.as_ref();
    let cache_5m = cache.map_or(0, |x| x.ephemeral_5m_input_tokens.int());
    let cache_1h = cache.map_or(0, |x| x.ephemeral_1h_input_tokens.int());
    let mut usage = Usage {
      input: self.input_tokens.int(),
      cache_write_5m: match cache_5m + cache_1h {
        0 => self.cache_creation_input_tokens.int(),
        _ => cache_5m,
      },
      cache_write_1h: cache_1h,
      cache_read: self.cache_read_input_tokens.int(),
      output: self.output_tokens.int(),
      reasoning: 0,
    };
    // Advisor model calls report their tokens inside the main message's usage.
    for x in self.iterations.blocks().filter(|x| x.kind.str() == Some("advisor_message")) {
      usage += x.usage();
    }
    usage
  }

  /// `speed`, else `service_tier`, as Python's `speed or service_tier`.
  fn speed(&self) -> Option<Speed> {
    let value = if self.speed.truthy() { &self.speed } else { &self.service_tier };
    value.str().and_then(Speed::parse)
  }
}

// MARK: Files

/// A spawned agent's usage from its tool result, used when its own log is missing.
struct Fallback {
  id: String,
  ts: Option<Ts>,
  usage: Usage,
  speed: Option<Speed>,
}

struct Parsed {
  path: PathBuf,
  session: Session,
  calls: Vec<AgentCall>,
  fallbacks: Vec<Fallback>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum TurnKey {
  Message(String, Option<String>),
  Anonymous(String),
}

/// Sessions under `<root>/projects` (or `<root>`); agents carry their main session's root and
/// working directory.
pub fn parse(root: &Path) -> Vec<Session> {
  let projects = root.join("projects");
  let root = if projects.is_dir() { projects } else { root.to_path_buf() };
  if !root.is_dir() {
    return vec![];
  }

  let mut parsed: Vec<Parsed> = jsonl_files(&[root]).par_iter().map(|x| parse_file(x)).collect();
  let mut agents: HashMap<(PathBuf, String), usize> = HashMap::new();
  for (i, x) in parsed.iter().enumerate() {
    let Some(dir) = session_dir(&x.path).filter(|_| !x.session.is_main) else { continue };
    agents.insert((dir.clone(), x.session.id.clone()), i);
    if let Some(id) = tool_use_id(&x.path) {
      agents.insert((dir, id), i);
    }
  }

  let mut parents: HashMap<usize, usize> = HashMap::new();
  for i in 0..parsed.len() {
    let path = &parsed[i].path;
    let dir = match parsed[i].session.is_main {
      true => Some(path.with_extension("")),
      false => session_dir(path),
    };
    if let Some(dir) = dir {
      link_agents(i, &dir, &agents, &mut parsed, &mut parents);
    }
  }

  let mut seen: HashSet<(usize, String)> = HashSet::new();
  for parent in 0..parsed.len() {
    let fallbacks = std::mem::take(&mut parsed[parent].fallbacks);
    for x in fallbacks {
      let logged =
        parsed[parent].calls.iter().any(|c| c.resolved && c.agent_id.as_ref() == Some(&x.id));
      if logged || !seen.insert((parent, x.id.clone())) {
        continue;
      }
      parents.insert(parsed.len(), parent);
      let mut session = Session::new(&x.id, false, "unknown", Provider::Claude);
      let usage = Some(x.usage);
      session.events.push(Event { ts: x.ts, usage, speed: x.speed, ..Default::default() });
      parsed.push(Parsed { path: PathBuf::new(), session, calls: vec![], fallbacks: vec![] });
    }
  }

  let mut sessions: Vec<Session> = parsed.into_iter().map(|x| x.session).collect();
  inherit_roots(&mut sessions, &parents);
  sessions.retain(|x| !x.events.is_empty());
  sessions
}

fn parse_file(path: &Path) -> Parsed {
  let stem = path.file_stem().map(|x| x.to_string_lossy()).unwrap_or_default();
  let id = stem.strip_prefix("agent-").unwrap_or(&stem);
  let id = if id.is_empty() { "unknown" } else { id };
  let is_agent = path.components().any(|x| x.as_os_str() == "subagents");
  let mut session = Session::new(id, !is_agent, "unknown", Provider::Claude);
  let mut calls = vec![];
  let mut fallbacks = vec![];
  let mut turns: HashMap<TurnKey, usize> = HashMap::new();

  jsonl(path, |line: Line, _| {
    if session.workdir == "unknown"
      && let Some(cwd) = line.cwd.str()
    {
      session.workdir = cwd.to_string();
    }
    match line.kind.str() {
      Some("assistant") => record_assistant(&mut session, &mut calls, &mut turns, &line),
      Some("user") if !line.meta.truthy() => {
        record_user(&mut session, &mut calls, &mut fallbacks, &line)
      }
      _ => {}
    }
    true
  });
  Parsed { path: path.to_path_buf(), session, calls, fallbacks }
}

/// One API response spans several lines (one per content block); merge them into one event.
fn record_assistant(
  session: &mut Session,
  calls: &mut Vec<AgentCall>,
  turns: &mut HashMap<TurnKey, usize>,
  line: &Line,
) {
  let Some(message) = &line.message.0 else { return };
  record_spawns(calls, &message.content);
  let id = message.id.str();
  let request = line.request.str();
  let anonymous = request.filter(|x| !x.is_empty()).or(line.uuid.str());
  let key = match (id, anonymous) {
    (Some(id), _) => Some(TurnKey::Message(id.to_string(), request.map(String::from))),
    (None, Some(x)) => Some(TurnKey::Anonymous(x.to_string())),
    (None, None) => None,
  };

  let index = match key.as_ref().and_then(|x| turns.get(x)) {
    Some(x) => *x,
    None => {
      session.events.push(Event {
        ts: parse_ts(&line.timestamp),
        identity: id.map(|x| (x.to_string(), request.map(String::from))),
        sidechain: line.sidechain.truthy(),
        failed: line.api_error.truthy(),
        ..Default::default()
      });
      let index = session.events.len() - 1;
      if let Some(key) = key {
        turns.insert(key, index);
      }
      index
    }
  };

  let event = &mut session.events[index];
  event.failed |= line.api_error.truthy();
  event.sidechain |= line.sidechain.truthy();
  if event.model.is_none() {
    event.model = message.model.str().map(String::from);
  }
  if let Some(raw) = &message.usage.0 {
    let usage = raw.usage();
    if event.usage.is_none_or(|x| usage.total() > x.total()) {
      event.usage = Some(usage);
    }
    event.speed = raw.speed().or(event.speed);
  }
}

fn record_user(
  session: &mut Session,
  calls: &mut [AgentCall],
  fallbacks: &mut Vec<Fallback>,
  line: &Line,
) {
  let Some(message) = &line.message.0 else { return };
  let mut results = message.content.blocks().filter(|x| x.kind.str() == Some("tool_result"));
  if let Some(first) = results.next() {
    let Some(result) = &line.result.0 else { return };
    let Some(id) = result.agent_id.str() else { return };
    if let Some(tool_id) = first.tool_use_id.str().filter(|x| !x.is_empty())
      && let Some(call) = calls.iter_mut().find(|x| x.tool_id == tool_id)
    {
      call.agent_id = Some(id.to_string());
    }
    let raw = result.usage.0.as_ref();
    fallbacks.push(Fallback {
      id: id.to_string(),
      ts: parse_ts(&line.timestamp),
      usage: raw.map(|x| x.usage()).unwrap_or_default(),
      speed: raw.and_then(|x| x.speed()),
    });
    return;
  }
  if is_user_text(&message.content)
    && let Some(ts) = parse_ts(&line.timestamp)
  {
    session.events.push(Event::activity(ts));
  }
}

fn is_user_text(content: &Content<Block>) -> bool {
  match content {
    Content::Text(x) => x.has_text(),
    Content::Blocks(_) => content.blocks().any(|x| {
      let text = if x.text.truthy() { x.text } else { x.input_text };
      text.has_text()
    }),
    Content::Other(_) => false,
  }
}

fn record_spawns(calls: &mut Vec<AgentCall>, content: &Content<Block>) {
  for block in content.blocks().filter(|x| x.kind.str() == Some("tool_use")) {
    let Some(id) = block.id.str().filter(|x| !x.is_empty()) else { continue };
    let spawns = matches!(block.name.str(), Some("Agent" | "Task"));
    if spawns && !calls.iter().any(|x| x.tool_id == id) {
      calls.push(AgentCall::new(id));
    }
  }
}

/// The session directory an agent file belongs to: the parent of its `subagents` directory.
fn session_dir(path: &Path) -> Option<PathBuf> {
  let dir = path.ancestors().skip(1).find(|x| x.file_name().is_some_and(|x| x == "subagents"))?;
  dir.parent().map(Path::to_path_buf)
}

/// The spawning tool call's id from the agent's `<agent>.meta.json`.
fn tool_use_id(path: &Path) -> Option<String> {
  #[derive(Deserialize, Default)]
  #[serde(default)]
  struct Meta {
    #[serde(rename = "toolUseId")]
    tool_use_id: Val,
  }

  let data = std::fs::read(path.with_extension("meta.json")).ok()?;
  let meta = serde_json::from_slice::<Obj<Meta>>(&data).ok()?.0?;
  meta.tool_use_id.str().map(String::from)
}

fn link_agents(
  parent: usize,
  dir: &Path,
  agents: &HashMap<(PathBuf, String), usize>,
  parsed: &mut [Parsed],
  parents: &mut HashMap<usize, usize>,
) {
  let calls: Vec<(usize, String)> = parsed[parent]
    .calls
    .iter()
    .enumerate()
    .filter(|(_, x)| !x.resolved)
    .map(|(i, x)| (i, x.agent_id.clone().filter(|x| !x.is_empty()).unwrap_or(x.tool_id.clone())))
    .collect();
  for (i, key) in calls {
    let Some(&child) = agents.get(&(dir.to_path_buf(), key)) else { continue };
    parsed[parent].calls[i].resolved = true;
    if let std::collections::hash_map::Entry::Vacant(e) = parents.entry(child) {
      e.insert(parent);
      link_agents(child, dir, agents, parsed, parents);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn fixtures(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
  }

  fn total(sessions: &[Session]) -> u64 {
    sessions.iter().flat_map(|x| &x.events).filter_map(|x| x.usage).map(|x| x.total()).sum()
  }

  #[test]
  fn links_subagent_to_main_root() {
    let sessions = parse(&fixtures("claude/projects"));
    let main = sessions.iter().find(|x| x.is_main).unwrap();
    let agent = sessions.iter().find(|x| !x.is_main).unwrap();

    assert_eq!(agent.root_id, main.id);
    assert_eq!(agent.workdir, "/repo");
    assert_eq!(main.workdir, "/repo");
  }

  #[test]
  fn keeps_one_copy_of_sidechain_usage() {
    let sessions = parse(&fixtures("claude_dedupe/projects"));
    let events = crate::metrics::billable_events(&sessions);
    let total: u64 =
      events.iter().map(|(s, e)| sessions[*s].events[*e].usage.unwrap().total()).sum();
    assert_eq!(total, 80);
  }

  #[test]
  fn uses_tool_result_when_agent_log_is_missing() {
    let dir = std::env::temp_dir().join(format!("cctrail-claude-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lines = [
      r#"{"type": "assistant", "timestamp": "2026-01-01T00:00:00Z", "cwd": "/repo", "message": {"content": [{"type": "tool_use", "id": "tool", "name": "Agent"}], "usage": {}}}"#,
      r#"{"type": "user", "timestamp": "2026-01-01T00:00:01Z", "message": {"content": [{"type": "tool_result", "tool_use_id": "tool"}]}, "toolUseResult": {"agentId": "agent", "usage": {"input_tokens": 2, "output_tokens": 3}}}"#,
    ];
    std::fs::write(dir.join("main.jsonl"), lines.join("\n")).unwrap();

    let sessions = parse(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
    let main = sessions.iter().find(|x| x.is_main).unwrap();
    let agent = sessions.iter().find(|x| !x.is_main).unwrap();
    assert_eq!(agent.root_id, main.id);
    assert_eq!(agent.workdir, "/repo");
    assert_eq!(total(std::slice::from_ref(agent)), 5);
  }

  #[test]
  fn merges_assistant_lines_and_reads_usage() {
    let line = |s: &str| serde_json::from_str::<Obj<Line>>(s).unwrap().0.unwrap();
    let mut session = Session::new("s", true, "/repo", Provider::Claude);
    let (mut calls, mut turns) = (vec![], HashMap::new());
    let lines = [
      r#"{"type": "assistant", "requestId": "r", "timestamp": "2026-01-01T00:00:00Z", "message": {"id": "m", "model": "x", "usage": {"input_tokens": 1, "cache_creation_input_tokens": 9, "output_tokens": 1}}}"#,
      r#"{"type": "assistant", "requestId": "r", "isSidechain": true, "message": {"id": "m", "model": "y", "usage": {"input_tokens": 1, "output_tokens": 5, "speed": "fast", "cache_creation": {"ephemeral_1h_input_tokens": 4}, "iterations": [{"type": "advisor_message", "output_tokens": 10}, {"type": "other", "output_tokens": 100}]}}}"#,
      r#"{"type": "assistant", "uuid": "u", "message": {"usage": {"input_tokens": true, "output_tokens": 2.0}}}"#,
    ];
    for x in lines {
      record_assistant(&mut session, &mut calls, &mut turns, &line(x));
    }

    assert_eq!(session.events.len(), 2);
    let e = &session.events[0];
    assert_eq!((e.model.as_deref(), e.speed, e.sidechain), (Some("x"), Some(Speed::Fast), true));
    assert_eq!(e.identity, Some(("m".to_string(), Some("r".to_string()))));
    let usage = Usage { input: 1, cache_write_1h: 4, output: 15, ..Default::default() };
    assert_eq!(e.usage, Some(usage));
    assert_eq!(session.events[1].usage, Some(Usage::default()));
    assert_eq!(session.events[1].identity, None);
  }
}
