// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use super::{
  AgentCall, Content, Obj, Present, Replies, Text, Val, inherit_roots, jsonl, jsonl_files,
  own_words, parse_ts,
};
use crate::model::{Event, Provider, Session, Speed, Usage};
use crate::workdir::normalize_remote;

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
  git: Obj<Git>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Git {
  repository_url: Val,
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
  effort: Val,
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
pub(super) fn normalize([input, cached, write, output, reasoning, _]: Signature) -> Usage {
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
  /// The git remote Codex logged for the working directory, normalized.
  remote: Option<String>,
  /// Lines with a smaller `ordinal` are the parent's history, copied into an agent's rollout.
  own_start: Option<i64>,
  /// Started by `codex exec`: a script's or another agent's prompt, not one typed.
  exec: bool,
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

/// Sessions under each root's `sessions` and `archived_sessions` (or the root itself); spawned
/// agents carry their main session's root and working directory. A rollout id seen twice counts
/// once: within a root the later file wins (`sessions` over `archived_sessions`), across roots (a
/// copied profile) the longest file, whole.
pub fn parse(roots: &[PathBuf], prompts: bool) -> Vec<Session> {
  let files: Vec<(usize, PathBuf)> = roots
    .iter()
    .enumerate()
    .flat_map(|(i, x)| history_files(x).into_iter().map(move |x| (i, x)))
    .collect();
  let metas: Vec<Option<(usize, Rollout)>> =
    files.par_iter().map(|(i, x)| read_meta(x).map(|x| (*i, x))).collect();
  // Each root on its own first (the later file wins), then the longest of the roots' winners.
  let mut by_root: Vec<Vec<Rollout>> = roots.iter().map(|_| vec![]).collect();
  let mut in_root: Vec<HashMap<String, usize>> = roots.iter().map(|_| HashMap::new()).collect();
  for (root, x) in metas.into_iter().flatten() {
    match in_root[root].get(&x.id) {
      Some(i) => by_root[root][*i] = x,
      None => {
        in_root[root].insert(x.id.clone(), by_root[root].len());
        by_root[root].push(x);
      }
    }
  }
  let size = |x: &Rollout| std::fs::metadata(&x.path).map_or(0, |x| x.len());
  let mut rollouts: Vec<Rollout> = vec![];
  let mut indexes: HashMap<String, usize> = HashMap::new();
  for x in by_root.into_iter().flatten() {
    match indexes.get(&x.id) {
      Some(i) if size(&x) > size(&rollouts[*i]) => rollouts[*i] = x,
      Some(_) => {}
      None => {
        indexes.insert(x.id.clone(), rollouts.len());
        rollouts.push(x);
      }
    }
  }

  let mut parsed: Vec<Parsed> = rollouts.par_iter().map(|x| parse_file(x, prompts)).collect();
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
  // The remote belongs to the directory the rollout started in: a session that moved (a later
  // `turn_context`, or an agent taking its root's directory) keeps no remote.
  for (x, rollout) in sessions.iter_mut().zip(&rollouts) {
    if x.workdir != rollout.workdir {
      x.remote = None;
    }
  }
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
    remote: meta.git.0.as_ref().and_then(|x| normalize_remote(x.repository_url.str()?)),
    own_start: meta.subagent_history_start_ordinal.ordinal(),
    exec: matches!(&meta.source, Some(Value::String(x)) if x == "exec"),
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

fn parse_file(rollout: &Rollout, prompts: bool) -> Parsed {
  let prompts = prompts && rollout.parent_id.is_none() && !rollout.exec;
  let mut session =
    Session::new(&rollout.id, rollout.parent_id.is_none(), &rollout.workdir, Provider::Codex);
  session.remote = rollout.remote.clone();
  let mut model: Option<String> = None;
  let mut speed: Option<Speed> = None;
  let mut effort: Option<String> = None;
  let mut prev_total: Option<Signature> = None;
  // The activity record of the current assistant turn, dropped when its token count follows.
  let mut assistant: Option<usize> = None;
  let mut pending: HashMap<String, usize> = HashMap::new();
  let mut calls: Vec<AgentCall> = vec![];
  let mut records: Vec<Record> = vec![];
  let mut signatures: Vec<Signature> = vec![];
  let mut replies = Replies::default();

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
        // Each turn's context says its effort; one without leaves the turn's unknown.
        effort = p.effort.str().map(String::from);
      }
      (_, Some("thread_settings_applied")) if event_msg => {
        if let Some(x) = p.thread_settings.0.as_ref().and_then(|x| x.service_tier.str()) {
          speed = Speed::parse(x);
        }
      }
      (_, Some("user_message")) if event_msg && !inherited => {
        if let Some(ts) = parse_ts(&line.timestamp).filter(|_| p.message.has_text()) {
          let mut event = Event::activity(ts);
          if prompts && let Some(words) = prompt(raw) {
            replies.prompt(&mut event, words);
          }
          records.push(Record { event: Some(event), signature: None });
        }
        assistant = None;
      }
      // Codex logs a prompt as a `UserMessage` item instead since 2026-08.
      (_, Some("item_completed")) if event_msg && prompts => {
        if let Some(ts) = parse_ts(&line.timestamp)
          && let Some(words) = prompt(raw)
        {
          let mut event = Event::activity(ts);
          replies.prompt(&mut event, words);
          records.push(Record { event: Some(event), signature: None });
        }
      }
      (_, Some("task_complete")) if event_msg && !inherited => assistant = None,
      (_, Some("token_count")) if event_msg => {
        let Some(info) = &p.info.0 else { return true };
        let total = info.total_token_usage.0.as_ref().map(RawUsage::values);
        if total.is_some() && total == prev_total {
          return true;
        }
        if prompts && let Some(x) = info.model.str().or(model.as_deref()) {
          replies.answer(x);
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
          effort: effort.clone(),
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

  replies.finish(records.iter_mut().filter_map(|x| x.event.as_mut()));
  Parsed { session, calls, records, signatures }
}

/// A prompt's own words: a `user_message`'s, or a `UserMessage` item's.
fn prompt(raw: &[u8]) -> Option<Box<str>> {
  let x: Value = serde_json::from_slice(raw).ok()?;
  let (p, item) = (&x["payload"], &x["payload"]["item"]);
  match item["type"].as_str() {
    Some("UserMessage") => own_words(&item["content"]),
    Some(_) => None,
    None => own_words(&p["message"]),
  }
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
  fn reads_typed_prompts_only() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/swears/codex");
    // Injected context, an agent's and `codex exec`'s prompts and the fork's replay are no prompts;
    // a `UserMessage` item is one, without the answer to Codex's question. The fork's own prompt
    // counts toward the last answer it replayed.
    let expected = [
      ("gpt-5.4", "wtf now"),
      ("gpt-5.4", "what the fuck"),
      ("gpt-5.4", "please rename it"),
      ("gpt-5.5", "shit, again"),
    ];
    let sessions = parse(std::slice::from_ref(&root), true);
    assert_eq!(super::super::tests::prompts(&sessions), expected);
    let sessions = parse(&[root], false);
    assert!(sessions.iter().flat_map(|x| &x.events).all(|x| x.prompt.is_none()));
  }

  #[test]
  fn links_only_spawned_agent_and_removes_fork_replay() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex");
    let sessions = parse(std::slice::from_ref(&root), false);
    let find = |id: &str| sessions.iter().find(|x| x.id == id).unwrap();
    let (main, agent, fork) = (find("codex-main-1"), find("codex-agent-1"), find("codex-fork-1"));

    assert_eq!(agent.root_id, main.id);
    assert_eq!((agent.workdir.as_str(), main.workdir.as_str()), ("/repo", "/repo"));
    assert_eq!(fork.root_id, fork.id);
    assert_eq!(total(&[fork]), 25);
    assert_eq!(total(&sessions.iter().collect::<Vec<_>>()), 455);

    // The same rollouts under a second root (a copied profile) count once.
    let twice = parse(&[root.clone(), root], false);
    assert_eq!(twice.len(), sessions.len());
    assert_eq!(total(&twice.iter().collect::<Vec<_>>()), 455);
  }

  #[test]
  fn keeps_the_longest_copy_across_roots() {
    let base = std::env::temp_dir().join(format!("cctrail-copies-{}", std::process::id()));
    let meta = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"s","cwd":"/r"}}"#;
    let count = |n: u64, t: &str| {
      format!(
        r#"{{"timestamp":"2026-01-01T00:0{t}:00Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{n}}},"last_token_usage":{{"input_tokens":100}}}}}}}}"#
      )
    };
    let short = [meta.to_string(), count(100, "1")].join("\n");
    let long = [short.clone(), count(200, "2"), count(300, "3")].join("\n");
    let file = |root: &str, sub: &str, text: &str| {
      std::fs::create_dir_all(base.join(root).join(sub)).unwrap();
      std::fs::write(base.join(root).join(sub).join("rollout.jsonl"), text).unwrap();
    };
    let all = |roots: &[&str]| {
      let roots: Vec<PathBuf> = roots.iter().map(|x| base.join(x)).collect();
      total(&parse(&roots, false).iter().collect::<Vec<_>>())
    };

    // Across roots the longest file wins whole, in either order.
    file("a", "sessions", &long);
    file("b", "sessions", &short);
    assert_eq!((all(&["a", "b"]), all(&["b", "a"])), (300, 300));
    // Within a root the later file wins, even when shorter (`sessions` over `archived_sessions`).
    file("c", "archived_sessions", &long);
    file("c", "sessions", &short);
    assert_eq!(all(&["c"]), 100);
    std::fs::remove_dir_all(&base).unwrap();
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

  #[test]
  fn carries_the_effort_of_a_turn_context() {
    let base = std::env::temp_dir().join(format!("cctrail-effort-{}", std::process::id()));
    let line = |t: u8, kind: &str, payload: &str| {
      format!(r#"{{"timestamp":"2026-01-01T00:0{t}:00Z","type":"{kind}","payload":{payload}}}"#)
    };
    let count = |t, n: u64| {
      let usage = format!(r#"{{"input_tokens":{n}}}"#);
      let info = format!(r#"{{"total_token_usage":{usage},"last_token_usage":{usage}}}"#);
      line(t, "event_msg", &format!(r#"{{"type":"token_count","info":{info}}}"#))
    };
    let lines = [
      line(0, "session_meta", r#"{"id":"s","cwd":"/r"}"#),
      count(1, 1),
      line(2, "turn_context", r#"{"model":"gpt-5","effort":"high"}"#),
      count(3, 2),
      count(4, 3),
      line(5, "turn_context", r#"{"effort":"low"}"#),
      count(6, 4),
      line(7, "turn_context", r#"{"effort":null}"#),
      count(8, 5),
    ];
    std::fs::create_dir_all(base.join("sessions")).unwrap();
    std::fs::write(base.join("sessions/rollout.jsonl"), lines.join("\n")).unwrap();
    let sessions = parse(std::slice::from_ref(&base), false);
    let effort: Vec<_> = sessions[0].events.iter().map(|x| x.effort.as_deref()).collect();
    assert_eq!(effort, [None, Some("high"), Some("high"), Some("low"), None]);
    std::fs::remove_dir_all(&base).unwrap();
  }
}
