// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use super::{
  AgentCall, Content, Obj, Present, Text, Val, inherit_roots, jsonl, jsonl_files, parse_ts,
};
use crate::model::{Event, Provider, Session, Speed, Usage};

/// A token count's six numbers: a fork replays its parent's counts, found by these.
type Signature = [u64; 6];

// MARK: Lines

#[derive(Deserialize, Default)]
#[serde(default)]
struct MetaLine {
  #[serde(rename = "type")]
  kind: Val,
  payload: Obj<Meta>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Meta {
  id: Val,
  parent_thread_id: Val,
  source: Option<Value>,
  thread_source: Option<Value>,
  history_base: Val,
  subagent_history_start_ordinal: Val,
  forked_from_id: Val,
  cwd: Val,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Line {
  #[serde(rename = "type")]
  kind: Val,
  timestamp: Val,
  ordinal: Val,
  payload: Obj<Payload>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Payload {
  #[serde(rename = "type")]
  kind: Val,
  model: Val,
  cwd: Val,
  service_tier: Val,
  thread_settings: Obj<Settings>,
  message: Text,
  info: Obj<Info>,
  call_id: Val,
  id: Val,
  name: Val,
  output: Present,
  role: Val,
  content: Content<Block>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Settings {
  service_tier: Val,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Info {
  total_token_usage: Obj<RawUsage>,
  last_token_usage: Obj<RawUsage>,
  model: Val,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Block {
  #[serde(rename = "type")]
  kind: Val,
  text: Text,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUsage {
  input_tokens: Val,
  cached_input_tokens: Val,
  cache_write_input_tokens: Val,
  output_tokens: Val,
  reasoning_output_tokens: Val,
  total_tokens: Val,
}

impl RawUsage {
  /// input, cached input, cache write, output, reasoning, total.
  fn values(&self) -> Signature {
    [
      self.input_tokens.int(),
      self.cached_input_tokens.int(),
      self.cache_write_input_tokens.int(),
      self.output_tokens.int(),
      self.reasoning_output_tokens.int(),
      self.total_tokens.int(),
    ]
  }
}

/// Codex counts cached input inside input and reasoning inside output.
fn normalize([input, cached, write, output, reasoning, _]: Signature) -> Usage {
  let cache_read = cached.min(input);
  let reasoning = reasoning.min(output);
  Usage {
    input: input - cache_read,
    cache_write_5m: write,
    cache_write_1h: 0,
    cache_read,
    output: output - reasoning,
    reasoning,
  }
}

fn delta(cur: Signature, prev: Option<Signature>) -> Signature {
  let prev = prev.unwrap_or_default();
  std::array::from_fn(|i| cur[i].saturating_sub(prev[i]))
}

// MARK: Rollouts

struct Rollout {
  path: PathBuf,
  id: String,
  parent_id: Option<String>,
  forked_from_id: Option<String>,
  has_history_base: bool,
  workdir: String,
  /// Lines with a smaller `ordinal` are the parent's history, copied into an agent's rollout.
  own_start: Option<i64>,
}

#[derive(Default)]
struct Record {
  event: Option<Event>,
  signature: Option<Signature>,
}

struct Parsed {
  session: Session,
  calls: Vec<AgentCall>,
  records: Vec<Record>,
  signatures: Vec<Signature>,
}

/// Sessions under `<root>/sessions` and `<root>/archived_sessions` (or `<root>`); spawned agents
/// carry their main session's root and working directory.
pub fn parse(root: &Path) -> Vec<Session> {
  let metas: Vec<Option<Rollout>> = history_files(root).par_iter().map(|x| read_meta(x)).collect();
  let mut rollouts: Vec<Rollout> = vec![];
  let mut indexes: HashMap<String, usize> = HashMap::new();
  for x in metas.into_iter().flatten() {
    match indexes.get(&x.id) {
      Some(i) => rollouts[*i] = x,
      None => {
        indexes.insert(x.id.clone(), rollouts.len());
        rollouts.push(x);
      }
    }
  }

  let mut parsed: Vec<Parsed> = rollouts.par_iter().map(parse_file).collect();
  let signatures: HashMap<&str, Vec<Signature>> =
    rollouts.iter().zip(&parsed).map(|(r, p)| (r.id.as_str(), p.signatures.clone())).collect();
  for (rollout, item) in rollouts.iter().zip(&mut parsed) {
    let mut replay = None;
    if !rollout.has_history_base && rollout.own_start.is_none() {
      let fork = rollout.forked_from_id.as_deref().filter(|x| !x.is_empty());
      replay = fork.and_then(|x| signatures.get(x));
    }
    let records = std::mem::take(&mut item.records);
    item.session.events = match replay {
      Some(x) => strip_replay(records, x),
      None => records.into_iter().filter_map(|x| x.event).collect(),
    };
  }

  let mut parents: HashMap<usize, usize> = HashMap::new();
  for rollout in &rollouts {
    let Some(parent_id) = &rollout.parent_id else { continue };
    let (Some(&child), Some(&parent)) = (indexes.get(&rollout.id), indexes.get(parent_id)) else {
      continue;
    };
    let calls = &mut parsed[parent].calls;
    let call = calls
      .iter()
      .position(|x| !x.resolved && x.agent_id.as_deref() == Some(&rollout.id))
      .or_else(|| calls.iter().position(|x| !x.resolved));
    if let Some(i) = call {
      calls[i].resolved = true;
      parents.insert(child, parent);
    }
  }

  let mut sessions: Vec<Session> = parsed.into_iter().map(|x| x.session).collect();
  inherit_roots(&mut sessions, &parents);
  sessions.retain(|x| !x.events.is_empty());
  sessions
}

fn history_files(root: &Path) -> Vec<PathBuf> {
  let roots = [root.join("archived_sessions"), root.join("sessions")];
  match roots.iter().any(|x| x.is_dir()) {
    true => jsonl_files(&roots),
    false => jsonl_files(&[root.to_path_buf()]),
  }
}

/// The rollout's `session_meta` line, the first one only.
fn read_meta(path: &Path) -> Option<Rollout> {
  let mut meta = None;
  jsonl(path, |line: MetaLine, _| {
    if line.kind.str() != Some("session_meta") {
      return true;
    }
    meta = Some(line.payload);
    false
  });
  let meta = meta?.0?;
  let id = meta.id.str()?;
  let parent_id = meta
    .parent_thread_id
    .str()
    .map(String::from)
    .or_else(|| meta.source.as_ref().and_then(nested_parent_id))
    .or_else(|| meta.thread_source.as_ref().and_then(nested_parent_id));
  Some(Rollout {
    path: path.to_path_buf(),
    id: id.to_string(),
    parent_id,
    forked_from_id: meta.forked_from_id.str().map(String::from),
    has_history_base: matches!(meta.history_base, Val::Map(_)),
    workdir: meta.cwd.str().unwrap_or("unknown").to_string(),
    own_start: meta.subagent_history_start_ordinal.ordinal(),
  })
}

fn nested_parent_id(value: &Value) -> Option<String> {
  match value {
    Value::Object(x) => match x.get("parent_thread_id") {
      Some(Value::String(id)) => Some(id.clone()),
      _ => x.values().find_map(nested_parent_id),
    },
    Value::Array(x) => x.iter().find_map(nested_parent_id),
    _ => None,
  }
}

fn parse_file(rollout: &Rollout) -> Parsed {
  let mut session =
    Session::new(&rollout.id, rollout.parent_id.is_none(), &rollout.workdir, Provider::Codex);
  let mut model: Option<String> = None;
  let mut speed: Option<Speed> = None;
  let mut prev_total: Option<Signature> = None;
  // The activity record of the current assistant turn, dropped when its token count follows.
  let mut assistant: Option<usize> = None;
  let mut pending: HashMap<String, usize> = HashMap::new();
  let mut calls: Vec<AgentCall> = vec![];
  let mut records: Vec<Record> = vec![];
  let mut signatures: Vec<Signature> = vec![];

  jsonl(&rollout.path, |line: Line, raw| {
    let inherited =
      matches!((rollout.own_start, line.ordinal.ordinal()), (Some(s), Some(o)) if o < s);
    let Some(p) = &line.payload.0 else { return true };
    let event_msg = line.kind.str() == Some("event_msg");

    match (line.kind.str(), p.kind.str()) {
      (Some("turn_context"), _) => {
        if let Some(x) = p.model.str() {
          model = Some(x.to_string());
        }
        if let Some(x) = p.cwd.str() {
          session.workdir = x.to_string();
        }
        if let Some(x) = p.service_tier.str() {
          speed = Speed::parse(x);
        }
      }
      (_, Some("thread_settings_applied")) if event_msg => {
        if let Some(x) = p.thread_settings.0.as_ref().and_then(|x| x.service_tier.str()) {
          speed = Speed::parse(x);
        }
      }
      (_, Some("user_message")) if event_msg && !inherited => {
        if let Some(ts) = parse_ts(&line.timestamp).filter(|_| p.message.has_text()) {
          records.push(Record { event: Some(Event::activity(ts)), signature: None });
        }
        assistant = None;
      }
      (_, Some("task_complete")) if event_msg && !inherited => assistant = None,
      (_, Some("token_count")) if event_msg => {
        let Some(info) = &p.info.0 else { return true };
        let total = info.total_token_usage.0.as_ref().map(RawUsage::values);
        if total.is_some() && total == prev_total {
          return true;
        }
        let last = info.last_token_usage.0.as_ref().map(RawUsage::values);
        if let Some(x) = last {
          signatures.push(x);
        }
        let usage = last.or_else(|| total.map(|x| delta(x, prev_total)));
        if total.is_some() {
          prev_total = total;
        }
        if inherited {
          return true;
        }

        let event = usage.map(normalize).filter(|x| x.total() > 0).map(|usage| Event {
          ts: parse_ts(&line.timestamp),
          usage: Some(usage),
          model: info.model.str().map(String::from).or_else(|| model.clone()),
          speed,
          ..Default::default()
        });
        if let Some(i) = assistant.filter(|_| event.as_ref().is_some_and(|x| x.ts.is_some())) {
          records[i].event = None;
        }
        assistant = None;
        records.push(Record { event, signature: last });
      }
      (Some("response_item"), _) if !inherited => {
        record_spawn(&mut calls, &mut pending, p, raw);
        if let Some(ts) = parse_ts(&line.timestamp)
          && assistant.is_none()
          && has_activity(p)
        {
          records.push(Record { event: Some(Event::activity(ts)), signature: None });
          assistant = Some(records.len() - 1);
        }
      }
      _ => {}
    }
    true
  });

  Parsed { session, calls, records, signatures }
}

/// Drops the leading events a fork replays from its parent: records whose signatures match the
/// parent's in order, up to the first that doesn't.
fn strip_replay(records: Vec<Record>, parent: &[Signature]) -> Vec<Event> {
  let mut events = vec![];
  let mut pending = vec![];
  let mut pos = 0;
  let mut replaying = true;
  for record in records {
    if !replaying {
      events.extend(record.event);
      continue;
    }
    pending.extend(record.event);
    let Some(signature) = record.signature else { continue };
    while pos < parent.len() && parent[pos] != signature {
      pos += 1;
    }
    if pos < parent.len() {
      pos += 1;
      pending.clear();
    } else {
      replaying = false;
      events.append(&mut pending);
    }
  }
  events.append(&mut pending);
  events
}

fn record_spawn(
  calls: &mut Vec<AgentCall>,
  pending: &mut HashMap<String, usize>,
  p: &Payload,
  raw: &[u8],
) {
  let id = match p.call_id {
    Val::Null => &p.id,
    _ => &p.call_id,
  };
  let Some(id) = id.str().filter(|x| !x.is_empty()) else { return };
  match p.kind.str() {
    Some("function_call" | "custom_tool_call" | "tool_search_call") => {
      let spawns = p.name.str().is_some_and(|x| x.ends_with("spawn_agent"));
      if spawns && !calls.iter().any(|x| x.tool_id == id) {
        pending.insert(id.to_string(), calls.len());
        calls.push(AgentCall::new(id));
      }
    }
    Some("function_call_output" | "custom_tool_call_output" | "tool_search_output") => {
      if p.output.0
        && let Some(i) = pending.remove(id)
      {
        calls[i].agent_id = output(raw).as_ref().and_then(find_agent_id);
      }
    }
    _ => {}
  }
}

/// The payload's `output`, parsed only for the rare outputs of agent spawns.
fn output(raw: &[u8]) -> Option<Value> {
  #[derive(Deserialize)]
  struct Line {
    payload: Payload,
  }
  #[derive(Deserialize)]
  struct Payload {
    output: Value,
  }
  serde_json::from_slice::<Line>(raw).ok().map(|x| x.payload.output)
}

/// The first `agent_id` in a value, looking into strings that hold JSON.
fn find_agent_id(value: &Value) -> Option<String> {
  match value {
    Value::String(x) => serde_json::from_str::<Value>(x).ok().as_ref().and_then(find_agent_id),
    Value::Object(x) => match x.get("agent_id") {
      Some(Value::String(id)) => Some(id.clone()),
      _ => x.values().find_map(find_agent_id),
    },
    Value::Array(x) => x.iter().find_map(find_agent_id),
    _ => None,
  }
}

fn has_activity(p: &Payload) -> bool {
  match p.kind.str() {
    Some("message") if p.role.str() == Some("assistant") => p
      .content
      .blocks()
      .any(|x| matches!(x.kind.str(), Some("output_text" | "text")) && x.text.has_text()),
    Some(
      "function_call"
      | "custom_tool_call"
      | "tool_search_call"
      | "web_search_call"
      | "image_generation_call",
    ) => true,
    _ => false,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn total(sessions: &[&Session]) -> u64 {
    sessions.iter().flat_map(|x| &x.events).filter_map(|x| x.usage).map(|x| x.total()).sum()
  }

  #[test]
  fn links_only_spawned_agent_and_removes_fork_replay() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex");
    let sessions = parse(&root);
    let find = |id: &str| sessions.iter().find(|x| x.id == id).unwrap();
    let (main, agent, fork) = (find("codex-main-1"), find("codex-agent-1"), find("codex-fork-1"));

    assert_eq!(agent.root_id, main.id);
    assert_eq!((agent.workdir.as_str(), main.workdir.as_str()), ("/repo", "/repo"));
    assert_eq!(fork.root_id, fork.id);
    assert_eq!(total(&[fork]), 25);
    assert_eq!(total(&sessions.iter().collect::<Vec<_>>()), 455);
  }

  #[test]
  fn strips_replayed_records() {
    let record = |input: u64, sig: Option<u64>| Record {
      event: Some(Event {
        usage: Some(Usage { input, ..Default::default() }),
        ..Default::default()
      }),
      signature: sig.map(|x| [x, 0, 0, 0, 0, 0]),
    };
    let sig = |x: u64| [x, 0, 0, 0, 0, 0];
    let records = vec![
      record(1, None),
      record(2, Some(1)),
      record(3, Some(2)),
      record(4, Some(9)),
      record(5, Some(2)),
    ];
    let events = strip_replay(records, &[sig(1), sig(2), sig(3)]);
    let inputs: Vec<u64> = events.iter().map(|x| x.usage.unwrap().input).collect();
    assert_eq!(inputs, vec![4, 5]);
  }

  #[test]
  fn finds_nested_agent_ids() {
    let value = serde_json::json!({"a": [{"b": "{\"agent_id\": \"x\"}"}], "c": {"agent_id": "y"}});
    assert_eq!(find_agent_id(&value).as_deref(), Some("x"));
    let value = serde_json::json!({"z": {"agent_id": "y"}, "agent_id": "w"});
    assert_eq!(find_agent_id(&value).as_deref(), Some("w"));
    assert_eq!(find_agent_id(&serde_json::json!("not json")), None);
  }

  #[test]
  fn normalizes_usage() {
    let x = normalize([100, 30, 5, 50, 20, 175]);
    assert_eq!(
      x,
      Usage {
        input: 70,
        cache_write_5m: 5,
        cache_read: 30,
        output: 30,
        reasoning: 20,
        ..Default::default()
      }
    );
    let x = normalize([10, 30, 0, 5, 20, 0]);
    assert_eq!(
      x,
      Usage { input: 0, cache_read: 10, output: 0, reasoning: 5, ..Default::default() }
    );
    assert_eq!(delta([5, 5, 5, 5, 5, 5], Some([1, 6, 0, 0, 0, 0])), [4, 0, 5, 5, 5, 5]);
  }
}
