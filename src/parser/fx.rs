// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::{Content, Obj, Present, Val, codex, inherit_roots, jsonl, model_id};
use crate::config::home_dir;
use crate::model::{Event, Provider, Session, Speed, Ts, Usage};

/// A model's requests in a session are split into at most this many events: a crafted count must
/// not exhaust memory.
const MAX_REQUESTS: u64 = 1 << 16;

// MARK: Files

/// `session.json`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Meta {
  workspace_root: Val,
  subagent_child: Val,
  fast_mode: Val,
  effort: Val,
  created_at_ms: Val,
  updated_at_ms: Val,
}

/// `subagent/owner.json`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Owner {
  parent_id: Val,
}

/// `usage-v2.json`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct UsageFile {
  snapshot: Obj<Snapshot>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Snapshot {
  models: Content<ModelUsage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ModelUsage {
  model: Val,
  input_tokens: Val,
  cache_read_tokens: Val,
  cache_write_tokens: Val,
  output_tokens: Val,
  reasoning_tokens: Val,
  request_count: Val,
  total_cost: Val,
}

impl ModelUsage {
  /// fx counts as Codex does: cache reads inside input, reasoning inside output.
  fn usage(&self) -> Usage {
    let x = [&self.input_tokens, &self.cache_read_tokens, &self.cache_write_tokens];
    let y = [&self.output_tokens, &self.reasoning_tokens];
    codex::normalize([x[0].int(), x[1].int(), x[2].int(), y[0].int(), y[1].int(), 0])
  }
}

/// An `events.jsonl` line: `{timestamp_ms, event: {<kind>: {…}}}`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Line {
  timestamp_ms: Val,
  event: Obj<Kind>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Kind {
  user: Present,
  steering: Present,
  assistant: Present,
  tool_result: Obj<ToolResult>,
  turn_completed: Obj<Turn>,
  interrupted: Obj<Turn>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ToolResult {
  created_at_ms: Val,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Turn {
  turn_summary: Obj<Summary>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Summary {
  started_at_ms: Val,
  completed_at_ms: Val,
}

fn read<T: DeserializeOwned>(path: &Path) -> Option<T> {
  serde_json::from_slice::<Obj<T>>(&std::fs::read(path).ok()?).ok()?.0
}

fn millis(x: &Val) -> Option<Ts> {
  Some(jiff::Timestamp::from_millisecond(x.ordinal()?).ok()?.as_microsecond())
}

// MARK: Sessions

/// A moment of agent activity and the session it is in.
type Moment = (Ts, usize);

struct Parsed {
  parent: Option<String>,
  /// When the agent worked: responses, tool results, turn ends.
  agent: Vec<Ts>,
  /// When the session was created and last updated, for usage without any activity.
  meta: Vec<Ts>,
  models: Vec<ModelUsage>,
  speed: Option<Speed>,
  effort: Option<String>,
}

/// fx's home: `FX_HOME` when set (cctrail's own, fx has no override), else `~/.fx`.
pub fn root() -> PathBuf {
  match env::var_os("FX_HOME").filter(|x| !x.is_empty()) {
    Some(x) => x.into(),
    None => home_dir().unwrap_or_else(|| ".".into()).join(".fx"),
  }
}

/// Sessions in `<root>/sessions/<id>/`; subagents carry their main session's root and directory.
///
/// fx (0.0.13) logs a session's usage only as per-model totals in `usage-v2.json`, without times,
/// and an agent's usage in its main session's totals. The global `usage.jsonl` has the times of
/// the requests but no session, and parallel sessions make matching them by time unreliable. So
/// each model's totals are split into its requests, evenly over the agent activity of the main
/// session and its agents, each request at one of those moments: totals per session and project
/// are exact, their split by day approximate.
pub fn parse(root: &Path) -> Vec<Session> {
  let dirs = std::fs::read_dir(root.join("sessions")).into_iter().flatten().flatten();
  let mut dirs: Vec<PathBuf> = dirs.map(|x| x.path()).filter(|x| x.is_dir()).collect();
  dirs.sort();
  let parsed: Vec<(Session, Parsed)> = dirs.par_iter().filter_map(|x| parse_dir(x)).collect();
  let (mut sessions, parsed): (Vec<Session>, Vec<Parsed>) = parsed.into_iter().unzip();

  let index: HashMap<&str, usize> =
    sessions.iter().enumerate().map(|(i, x)| (x.id.as_str(), i)).collect();
  let parents: HashMap<usize, usize> = parsed
    .iter()
    .enumerate()
    .filter_map(|(i, x)| Some((i, *index.get(x.parent.as_deref()?)?)))
    .filter(|(i, j)| i != j)
    .collect();
  inherit_roots(&mut sessions, &parents);

  // The agent activity of each main session and its agents, in time order, and whose it is.
  let mut trees: HashMap<&str, Vec<Moment>> = HashMap::new();
  for (i, (s, x)) in sessions.iter().zip(&parsed).enumerate() {
    trees.entry(&s.root_id).or_default().extend(x.agent.iter().map(|t| (*t, i)));
  }
  trees.values_mut().for_each(|x| x.sort_unstable());

  let mut placed: Vec<(usize, Event)> = vec![];
  for (i, (s, x)) in sessions.iter().zip(&parsed).enumerate() {
    // Usage without any agent activity goes to the session's own times.
    let own: Vec<Moment> = match s.events.is_empty() {
      false => s.events.iter().filter_map(|e| e.ts).map(|t| (t, i)).collect(),
      true => x.meta.iter().map(|t| (*t, i)).collect(),
    };
    let at = trees.get(s.root_id.as_str()).filter(|x| !x.is_empty()).unwrap_or(&own);
    for m in &x.models {
      let usage = m.usage();
      let cost = m.total_cost.float().filter(|x| *x > 0.0);
      let requests = m.request_count.int();
      for (at, usage, cost) in spread(usage, requests, cost, at) {
        let event = Event {
          ts: at.map(|x| x.0),
          usage: Some(usage),
          model: m.model.str().map(model_id),
          speed: x.speed,
          effort: x.effort.clone(),
          cost,
          ..Default::default()
        };
        placed.push((at.map_or(i, |x| x.1), event));
      }
    }
  }
  for (i, event) in placed {
    sessions[i].events.push(event);
  }
  for x in &mut sessions {
    x.events.sort_by_key(|x| x.ts);
  }
  sessions.retain(|x| !x.events.is_empty());
  sessions
}

/// A model's usage over its requests, each an equal share (the remainder a token each to the first
/// ones) at the moment evenly placed among `at` for it; a share of the cost fx billed.
fn spread(
  total: Usage,
  requests: u64,
  cost: Option<f64>,
  at: &[Moment],
) -> Vec<(Option<Moment>, Usage, Option<f64>)> {
  if total.total() == 0 {
    return vec![];
  }
  let k = requests.clamp(1, MAX_REQUESTS);
  let part = |v: u64, j: u64| v / k + u64::from(j < v % k);
  let n = at.len() as u128;
  (0..k)
    .map(|j| {
      let usage = Usage {
        input: part(total.input, j),
        cache_write_5m: part(total.cache_write_5m, j),
        cache_write_1h: part(total.cache_write_1h, j),
        cache_read: part(total.cache_read, j),
        output: part(total.output, j),
        reasoning: part(total.reasoning, j),
      };
      let i = (2 * j as u128 + 1) * n / (2 * k as u128);
      (at.get(i as usize).copied(), usage, cost.map(|x| x / k as f64))
    })
    .collect()
}

fn parse_dir(dir: &Path) -> Option<(Session, Parsed)> {
  let id = dir.file_name()?.to_string_lossy().to_string();
  let meta: Meta = read(&dir.join("session.json"))?;
  let owner: Option<Owner> = read(&dir.join("subagent/owner.json"));
  let parent = owner.and_then(|x| x.parent_id.str().map(String::from));
  let is_main = !meta.subagent_child.truthy() && parent.is_none();
  let workdir = meta.workspace_root.str().unwrap_or("unknown");
  let mut session = Session::new(&id, is_main, workdir, Provider::Fx);

  // fx 0.0.13 writes lines in batches, all with the batch's `timestamp_ms`: a tool result's and a
  // turn's own times are exact.
  let mut agent = vec![];
  jsonl(&dir.join("events.jsonl"), |line: Line, _| {
    let Some(kind) = line.event.0 else { return true };
    let at = millis(&line.timestamp_ms);
    let turn = kind.turn_completed.0.or(kind.interrupted.0).and_then(|x| x.turn_summary.0);
    if kind.user.0 || kind.steering.0 {
      session.events.extend(at.map(Event::activity));
    } else if kind.assistant.0 {
      agent.extend(at);
    } else if let Some(x) = kind.tool_result.0 {
      agent.extend(millis(&x.created_at_ms).or(at));
    } else if let Some(x) = turn {
      session.events.extend(millis(&x.started_at_ms).map(Event::activity));
      agent.extend(millis(&x.completed_at_ms).or(at));
    }
    true
  });
  session.events.extend(agent.iter().map(|x| Event::activity(*x)));

  let usage: Option<UsageFile> = read(&dir.join("usage-v2.json"));
  let snapshot = usage.and_then(|x| x.snapshot.0);
  let models = match snapshot.map(|x| x.models) {
    Some(Content::Blocks(x)) => x.into_iter().filter_map(|x| x.0).collect(),
    _ => vec![],
  };
  let speed = match meta.fast_mode {
    Val::Bool(x) => Some(if x { Speed::Fast } else { Speed::Standard }),
    _ => None,
  };
  let parsed = Parsed {
    parent,
    agent,
    meta: [&meta.created_at_ms, &meta.updated_at_ms].into_iter().filter_map(millis).collect(),
    models,
    speed,
    effort: meta.effort.str().map(String::from),
  };
  Some((session, parsed))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::metrics::local_date;

  fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fx")
  }

  fn total<'a>(events: impl IntoIterator<Item = &'a Event>) -> u64 {
    events.into_iter().filter_map(|x| x.usage).map(|x| x.total()).sum()
  }

  #[test]
  fn links_agents_and_counts_their_usage_once() {
    let sessions = parse(&fixtures());
    let find = |id: &str| sessions.iter().find(|x| x.id == id).unwrap();
    let (main, agent, other) = (find("fx-main-1"), find("fx-agent-1"), find("fx-other-1"));
    // A session.json that doesn't parse skips its directory, as does a directory without one.
    assert_eq!(sessions.len(), 3);
    assert!(main.is_main && other.is_main && !agent.is_main);
    // An agent takes its main session's root and directory, keeps its own logged one.
    assert_eq!((agent.root_id.as_str(), agent.workdir.as_str()), ("fx-main-1", "/repo"));
    assert_eq!(agent.cwd, "/elsewhere");
    // Its main session's totals hold its usage, placed at its own activity.
    assert_eq!((total(&main.events), total(&agent.events)), (978, 432));
    assert_eq!(total(&other.events), 2065);
  }

  #[test]
  fn spreads_totals_over_the_agents_activity() {
    let sessions = parse(&fixtures());
    let tree: Vec<&Event> =
      sessions.iter().filter(|x| x.root_id == "fx-main-1").flat_map(|x| &x.events).collect();
    let day = |d: i8| {
      let on = |x: &&&Event| x.ts.is_some_and(|t| local_date(t).day() == d);
      total(tree.iter().filter(on).copied())
    };
    // Three requests of one model over seven moments, one on the first day.
    assert_eq!((day(5), day(7)), (435, 975));
    let billed: Vec<&Event> = tree.into_iter().filter(|x| x.usage.is_some()).collect();
    let mut models: Vec<&str> = billed.iter().filter_map(|x| x.model.as_deref()).collect();
    models.sort();
    assert_eq!(models, ["gpt-5.6-luna", "gpt-6-astra", "gpt-6-astra", "gpt-6-astra"]);
    // The main session's speed and effort, its agent's own unused.
    assert!(billed.iter().all(|x| x.speed == Some(Speed::Fast)));
    assert!(billed.iter().all(|x| x.effort.as_deref() == Some("xhigh")));

    let other = sessions.iter().find(|x| x.id == "fx-other-1").unwrap();
    let billed: Vec<_> = other.events.iter().filter(|x| x.usage.is_some()).collect();
    let costs: Vec<_> = billed.iter().map(|x| (x.model.as_deref().unwrap(), x.cost)).collect();
    let sonnet = ("claude-sonnet-4-5", None);
    assert_eq!(costs, [sonnet, sonnet, ("grok-4", Some(0.5))]);
    assert!(billed.iter().all(|x| x.speed == Some(Speed::Standard)));
  }

  #[test]
  fn splits_requests_exactly() {
    let total = Usage {
      input: 400,
      cache_write_5m: 2,
      cache_write_1h: 0,
      cache_read: 600,
      output: 200,
      reasoning: 100,
    };
    let at: Vec<Moment> = (0..7).map(|x| (x * 10, 0)).collect();
    let pieces = spread(total, 3, Some(0.75), &at);
    let sum = pieces.iter().fold(Usage::default(), |a, x| a + x.1);
    assert_eq!(sum, total);
    let inputs: Vec<u64> = pieces.iter().map(|x| x.1.input).collect();
    assert_eq!(inputs, [134, 133, 133]);
    // Evenly over the moments, each a share of the cost.
    let times: Vec<_> = pieces.iter().map(|x| x.0.map(|x| x.0)).collect();
    assert_eq!(times, [Some(10), Some(30), Some(50)]);
    assert!(pieces.iter().all(|x| x.2 == Some(0.25)));

    // More requests than moments; none at all; no requests counted; no tokens.
    let pieces = spread(total, 9, None, &at[..2]);
    assert_eq!(pieces.iter().fold(Usage::default(), |a, x| a + x.1), total);
    assert_eq!(pieces.iter().filter(|x| x.0 == Some((10, 0))).count(), 5);
    assert!(spread(total, 2, None, &[]).iter().all(|x| x.0.is_none()));
    assert_eq!(spread(total, 0, None, &at).len(), 1);
    assert!(spread(Usage::default(), 5, None, &at).is_empty());
    assert_eq!(spread(total, u64::MAX, None, &at).len() as u64, MAX_REQUESTS);
  }

  #[test]
  fn splits_cache_reads_and_reasoning() {
    let raw = |s: &str| serde_json::from_str::<ModelUsage>(s).unwrap().usage();
    let x = raw(
      r#"{"input_tokens": 100, "cache_read_tokens": 30, "cache_write_tokens": 5,
      "output_tokens": 50, "reasoning_tokens": 20}"#,
    );
    let expected =
      Usage { input: 70, cache_write_5m: 5, cache_read: 30, output: 30, reasoning: 20, ..x };
    assert_eq!(x, Usage { cache_write_1h: 0, ..expected });
    // Counts that don't add up are clamped, never wrapped.
    let x = raw(r#"{"input_tokens": 1, "cache_read_tokens": 9, "output_tokens": "x"}"#);
    assert_eq!((x.input, x.cache_read, x.output), (0, 1, 0));
  }
}
