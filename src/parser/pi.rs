// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Deserialize;

use super::{Obj, Val, jsonl, jsonl_files, parse_ts};
use crate::config::{expand_path, home_dir};
use crate::model::{Event, Provider, Session, Ts, Usage};

// MARK: Lines

#[derive(Deserialize, Default)]
#[serde(default)]
struct Line {
  #[serde(rename = "type")]
  kind: Val,
  id: Val,
  timestamp: Val,
  cwd: Val,
  #[serde(rename = "parentSession")]
  parent: Val,
  #[serde(rename = "modelId")]
  model_id: Val,
  model: Val,
  #[serde(rename = "thinkingLevel")]
  thinking: Val,
  usage: Obj<RawUsage>,
  message: Obj<Message>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Message {
  role: Val,
  model: Val,
  #[serde(rename = "responseModel")]
  response_model: Val,
  usage: Obj<RawUsage>,
  #[serde(rename = "stopReason")]
  stop: Val,
  #[serde(rename = "thinkingLevel")]
  thinking: Val,
  timestamp: Val,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUsage {
  input: Val,
  output: Val,
  #[serde(rename = "cacheRead")]
  cache_read: Val,
  #[serde(rename = "cacheWrite")]
  cache_write: Val,
  #[serde(rename = "cacheWrite1h")]
  cache_write_1h: Val,
  reasoning: Val,
  cost: Obj<Cost>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Cost {
  total: Val,
}

impl RawUsage {
  /// pi's input excludes cache reads; its output includes reasoning, its cache writes the 1h ones.
  fn usage(&self) -> Usage {
    let (write, output) = (self.cache_write.int(), self.output.int());
    let write_1h = self.cache_write_1h.int().min(write);
    let reasoning = self.reasoning.int().min(output);
    Usage {
      input: self.input.int(),
      cache_write_5m: write - write_1h,
      cache_write_1h: write_1h,
      cache_read: self.cache_read.int(),
      output: output - reasoning,
      reasoning,
    }
  }

  /// pi's own estimate from its price list; 0 means it has no price.
  fn cost(&self) -> Option<f64> {
    self.cost.0.as_ref()?.total.float().filter(|x| *x > 0.0)
  }
}

// MARK: Sessions

/// What a copy of an entry shares with the original: its kind, the entry's and the message's
/// time and its usage. `/fork`, `/clone` and `--fork` copy the entries of the source session into
/// the new file verbatim, but a v1 source migrated on the way gets new entry ids, and a branch
/// copied alone lacks the model changes of the others.
type Key = (&'static str, Option<Ts>, Option<i64>, Option<Usage>);

struct Parsed {
  /// When the session was created, from its header.
  created: Option<Ts>,
  /// The file name of the session it was forked from.
  parent: Option<String>,
  session: Session,
  keys: Vec<Key>,
}

/// pi's session directories: `PI_CODING_AGENT_SESSION_DIR` when set, else `sessions` in the agent
/// directory (`PI_CODING_AGENT_DIR`, else `~/.pi/agent`) and the absolute `sessionDir` of its
/// settings.
pub fn roots() -> Vec<PathBuf> {
  let var = |x: &str| env::var(x).ok().filter(|x| !x.is_empty());
  let home = home_dir();
  let expand = |x: &str| expand_path(x, home.as_deref()).ok();
  if let Some(x) = var("PI_CODING_AGENT_SESSION_DIR") {
    return vec![expand(&x).unwrap_or_else(|| x.into())];
  }
  let agent = match var("PI_CODING_AGENT_DIR") {
    Some(x) => expand(&x).unwrap_or_else(|| x.into()),
    None => home.clone().unwrap_or_else(|| ".".into()).join(".pi/agent"),
  };
  let settings = std::fs::read(agent.join("settings.json")).ok();
  let settings = settings.and_then(|x| serde_json::from_slice::<serde_json::Value>(&x).ok());
  // A relative `sessionDir` is below each project's directory, where cctrail can't find it.
  let custom = settings.as_ref().and_then(|x| expand(x.get("sessionDir")?.as_str()?));
  let default = agent.join("sessions");
  let custom = custom.filter(|x| *x != default);
  std::iter::once(default).chain(custom).collect()
}

/// Sessions under the roots, each its own main session in the directory of its header. A fork's
/// copies of its source's entries count only in the source; when the source is gone, in the first
/// fork created.
pub fn parse(roots: &[PathBuf]) -> Vec<Session> {
  let name = |x: &Path| x.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
  let size = |x: &Path| std::fs::metadata(x).map_or(0, |x| x.len());
  // A session file found under two roots counts once, the longest copy. The pi-subagents
  // extension keeps debug copies of its agents' sessions in `subagent-artifacts`.
  let mut longest: HashMap<String, PathBuf> = HashMap::new();
  for x in jsonl_files(roots) {
    if x.components().any(|x| x.as_os_str() == "subagent-artifacts") {
      continue;
    }
    let best = longest.entry(name(&x)).or_insert_with(|| x.clone());
    if size(&x) > size(best) {
      *best = x;
    }
  }
  let mut files: Vec<PathBuf> = longest.into_values().collect();
  files.sort();
  let parsed: Vec<Parsed> = files.par_iter().map(|x| parse_file(x)).collect();

  // The source is found by its file name, unique by its id, wherever its directory is now.
  let index: HashMap<String, usize> = files.iter().enumerate().map(|(i, x)| (name(x), i)).collect();
  let mut order: Vec<usize> = (0..parsed.len()).collect();
  order.sort_by_key(|i| (parsed[*i].created.is_none(), parsed[*i].created));
  let mut orphans: HashMap<&str, HashSet<&Key>> = HashMap::new();
  let mut keep: Vec<Vec<bool>> = vec![vec![]; parsed.len()];
  for i in order {
    let x = &parsed[i];
    let Some(parent) = x.parent.as_deref() else {
      keep[i] = vec![true; x.keys.len()];
      continue;
    };
    keep[i] = match index.get(parent).filter(|x| **x != i) {
      Some(j) => {
        let copied: HashSet<&Key> = parsed[*j].keys.iter().collect();
        x.keys.iter().map(|k| !copied.contains(k)).collect()
      }
      None => {
        let copied = orphans.entry(parent).or_default();
        let keep = x.keys.iter().map(|k| !copied.contains(k)).collect();
        copied.extend(&x.keys);
        keep
      }
    };
  }

  let mut sessions = vec![];
  for (x, keep) in parsed.into_iter().zip(keep) {
    let mut session = x.session;
    let events = std::mem::take(&mut session.events);
    session.events = events.into_iter().zip(keep).filter_map(|(e, k)| k.then_some(e)).collect();
    if !session.events.is_empty() {
      sessions.push(session);
    }
  }
  sessions
}

fn parse_file(path: &Path) -> Parsed {
  // `<time>_<id>.jsonl`, the header's id preferred.
  let stem = path.file_stem().map(|x| x.to_string_lossy()).unwrap_or_default();
  let id = stem.rsplit_once('_').map_or(&*stem, |x| x.1);
  let mut session = Session::new(id, true, "unknown", Provider::Pi);
  let mut created = None;
  let mut parent = None;
  let mut header = false;
  // The session's model and thinking level as last selected or used.
  let mut model: Option<String> = None;
  let mut effort: Option<String> = None;
  let mut keys = vec![];

  jsonl(path, |line: Line, _| {
    let message = line.message.0.as_ref();
    let millis = message.and_then(|x| x.timestamp.ordinal());
    let at = millis.and_then(|x| jiff::Timestamp::from_millisecond(x).ok());
    let ts = parse_ts(&line.timestamp).or(at.map(|x| x.as_microsecond()));
    let new = |usage: &RawUsage, model: Option<String>| Event {
      ts,
      usage: Some(usage.usage()),
      model,
      cost: usage.cost(),
      ..Default::default()
    };
    // Usage that pi sums over several requests: a split turn's compaction (the history and the
    // turn's prefix), a tool's nested calls.
    let summed =
      |usage: &RawUsage, model: Option<String>| Event { summed: true, ..new(usage, model) };

    let event = match (line.kind.str(), message.map(|x| (x, x.role.str()))) {
      (Some("session"), _) if !header => {
        header = true;
        created = ts;
        parent = line.parent.str().and_then(|x| x.rsplit(['/', '\\']).next()).map(String::from);
        if let Some(x) = line.id.str().filter(|x| !x.is_empty()) {
          session.id = x.to_string();
          session.root_id = x.to_string();
        }
        if let Some(x) = line.cwd.str() {
          session.workdir = x.to_string();
        }
        None
      }
      (Some("model_change"), _) => {
        model = line.model_id.str().map(String::from).or(model.take());
        None
      }
      (Some("thinking_level_change"), _) => {
        effort = line.thinking.str().map(String::from).or(effort.take());
        None
      }
      // A prompt, or a shell command the user ran (`!cmd`).
      (Some("message"), Some((_, Some("user" | "bashExecution")))) => {
        ts.map(|x| ("user", Event::activity(x)))
      }
      (Some("message"), Some((m, Some("assistant")))) => {
        if let Some(x) = m.model.str() {
          model = Some(x.to_string());
        }
        let raw = m.usage.0.as_ref();
        let usage = raw.map(RawUsage::usage);
        let event = Event {
          ts,
          usage,
          // The model that answered, when the provider served another (a fallback, a router's).
          model: m.response_model.str().map(String::from).or_else(|| model.clone()),
          effort: m.thinking.str().map(String::from).or_else(|| effort.clone()),
          cost: raw.and_then(RawUsage::cost),
          // An error after the response began still bills the tokens it carries.
          failed: m.stop.str() == Some("error") && usage.is_none_or(|x| x.total() == 0),
          ..Default::default()
        };
        Some(("assistant", event))
      }
      // Requests apart from the turns, which pi counts in a session's totals too. A tool's nested
      // calls may go to any models, so they have none.
      (Some("message"), Some((m, Some("toolResult")))) => {
        m.usage.0.as_ref().map(|x| ("tool", summed(x, None)))
      }
      // Requests outside the conversation, such as cache warming, name their model.
      (Some("usage"), _) => line.usage.0.as_ref().map(|x| {
        let name = line.model.str().map(String::from).or_else(|| model.clone());
        ("usage", new(x, name))
      }),
      // A summary of compacted or abandoned history runs on the session's model, which pi
      // doesn't log with it.
      (Some("compaction" | "branch_summary"), _) => {
        line.usage.0.as_ref().map(|x| ("summary", summed(x, model.clone())))
      }
      _ => None,
    };
    if let Some((kind, event)) = event {
      keys.push((kind, ts, millis, event.usage));
      session.events.push(event);
    }
    true
  });

  Parsed { created, parent, session, keys }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/sessions")
  }

  fn total(session: &Session) -> u64 {
    session.events.iter().filter_map(|x| x.usage).map(|x| x.total()).sum()
  }

  #[test]
  fn counts_entries_a_fork_copied_once() {
    let sessions = parse(&[fixtures()]);
    let find = |id: &str| sessions.iter().find(|x| x.id == id).unwrap();
    let (main, fork) = (find("pi-main-1"), find("pi-fork-1"));
    assert_eq!(sessions.len(), 2);
    assert_eq!((main.workdir.as_str(), main.root_id.as_str()), ("/repo", "pi-main-1"));
    assert_eq!((fork.workdir.as_str(), fork.root_id.as_str()), ("/other", "pi-fork-1"));
    // The fork holds only its own prompt and response: the source keeps the copied ones, the one
    // with a new id too, wherever the source's directory is now.
    assert_eq!((fork.events.len(), total(fork)), (2, 10));
    assert_eq!(total(main), 11_220);

    // The same sessions under a second root count once.
    let twice = parse(&[fixtures(), fixtures()]);
    assert_eq!(twice.iter().map(total).sum::<u64>(), 11_230);
  }

  #[test]
  fn counts_copies_once_by_lineage() {
    let dir = std::env::temp_dir().join(format!("cctrail-pi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let header = |id: &str, parent: &str| {
      format!(
        r#"{{"type":"session","id":"{id}","timestamp":"2026-01-01T00:00:00Z","cwd":"/r","parentSession":"{parent}"}}"#
      )
    };
    let usage = |model: &str| {
      format!(
        r#"{{"type":"usage","timestamp":"2026-01-01T00:00:01Z","model":"{model}","usage":{{"input":100}}}}"#
      )
    };
    let reply = r#"{"type":"message","timestamp":"2026-01-01T00:00:02Z","message":{"role":"assistant","model":"a","responseModel":"b","usage":{"input":10}}}"#;
    let late = r#"{"type":"message","message":{"role":"user","timestamp":300000000000000}}"#;
    let file =
      |name: &str, lines: &[&str]| std::fs::write(dir.join(name), lines.join("\n")).unwrap();
    // Alike entries in one session, in a session that isn't their fork, in a fork whose source is
    // gone: all count. A time out of range is no time.
    file("1_one.jsonl", &[&header("one", ""), &usage("x"), &usage("y"), reply, late]);
    file("2_two.jsonl", &[&header("two", "/gone/0_zero.jsonl"), &usage("x"), reply]);
    let sessions = parse(std::slice::from_ref(&dir));
    let find = |id: &str| sessions.iter().find(|x| x.id == id).unwrap();
    assert_eq!((total(find("one")), total(find("two"))), (210, 110));
    assert_eq!(find("one").events.len(), 3);
    // The model that answered.
    assert_eq!(find("two").events[1].model.as_deref(), Some("b"));

    // Another fork of the gone source, created later, shares the first one's copies. A fork of it
    // copied a summary without the model change before it, as a branch copied alone.
    let summary = r#"{"type":"compaction","timestamp":"2026-01-01T00:00:03Z","usage":{"input":5}}"#;
    let change = r#"{"type":"model_change","timestamp":"2026-01-01T00:00:02Z","modelId":"m"}"#;
    let later = header("three", "0_zero.jsonl").replace("00:00:00", "00:00:09");
    file("3_three.jsonl", &[&later, change, &usage("x"), reply, summary]);
    file("4_four.jsonl", &[&header("four", "3_three.jsonl"), summary]);
    // A longer copy of `one` under a second root wins.
    let other = dir.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let longer = [&header("one", "") as &str, &usage("x"), &usage("y"), reply, &usage("z")];
    std::fs::write(other.join("1_one.jsonl"), longer.join("\n")).unwrap();
    let sessions = parse(&[dir.clone(), other]);
    let total = |id: &str| sessions.iter().find(|x| x.id == id).map_or(0, total);
    let totals = ["one", "two", "three", "four"].map(total);
    assert_eq!(totals, [310, 110, 5, 0]);
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn reads_turns_summaries_and_failures() {
    let sessions = parse(&[fixtures()]);
    let main = sessions.iter().find(|x| x.id == "pi-main-1").unwrap();
    let billed: Vec<_> = main.events.iter().filter(|x| x.usage.is_some()).collect();
    let models: Vec<_> = billed.iter().map(|x| x.model.as_deref().unwrap_or("-")).collect();
    let bedrock = "us.anthropic.claude-sonnet-4-5-20250929-v1:0";
    assert_eq!(models, ["gpt-5", bedrock, bedrock, bedrock, "glm-5", "-"]);
    let failed: Vec<_> = billed.iter().map(|x| x.failed).collect();
    assert_eq!(failed, [false, false, true, false, false, false]);
    // The thinking level of the response, else the one last selected.
    let effort: Vec<_> = billed.iter().map(|x| x.effort.as_deref()).collect();
    assert_eq!(effort, [Some("high"), Some("medium"), Some("medium"), None, Some("medium"), None]);
    let costs: Vec<_> = billed.iter().map(|x| x.cost).collect();
    assert_eq!(costs, [Some(0.01), None, None, None, Some(0.25), Some(0.02)]);
    // Two prompts and a shell command.
    assert_eq!(main.events.iter().filter(|x| x.usage.is_none()).count(), 3);
  }

  #[test]
  fn splits_reasoning_and_cache_writes() {
    let raw = |s: &str| serde_json::from_str::<RawUsage>(s).unwrap().usage();
    let x = raw(
      r#"{"input": 100, "output": 50, "reasoning": 20, "cacheRead": 7, "cacheWrite": 30,
      "cacheWrite1h": 10}"#,
    );
    let expected = Usage {
      input: 100,
      cache_write_5m: 20,
      cache_write_1h: 10,
      cache_read: 7,
      output: 30,
      reasoning: 20,
    };
    assert_eq!(x, expected);
    // Counts that don't add up are clamped, never wrapped.
    let x = raw(r#"{"output": 5, "reasoning": 9, "cacheWrite": 1, "cacheWrite1h": 4}"#);
    assert_eq!((x.output, x.reasoning, x.cache_write_5m, x.cache_write_1h), (0, 5, 0, 1));
    assert_eq!(raw(r#"{"input": "x", "output": -1}"#), Usage::default());
  }
}
