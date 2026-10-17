// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use super::{Obj, Replies, Val, jsonl, jsonl_files, model_id, own_words, parse_ts};
use crate::config::{expand_path, home_dir};
use crate::model::{Event, Provider, Session, Ts, Usage};
use crate::swear::Dict;

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
  /// The path of the session it was forked from.
  parent: Option<String>,
  session: Session,
  keys: Vec<Key>,
}

/// A path from pi's settings or environment with `~` for the home directory; pi on Windows takes
/// `~\` too.
fn expand_home(path: &str, home: Option<&Path>, windows: bool) -> Option<PathBuf> {
  let path = match path.strip_prefix("~\\").filter(|_| windows) {
    Some(x) => format!("~/{x}"),
    None => path.to_string(),
  };
  expand_path(&path, home).ok()
}

/// pi's agent directory: `PI_CODING_AGENT_DIR`, else `~/.pi/agent`.
pub fn agent_dir() -> PathBuf {
  let home = home_dir();
  match env::var("PI_CODING_AGENT_DIR").ok().filter(|x| !x.is_empty()) {
    Some(x) => expand_home(&x, home.as_deref(), cfg!(windows)).unwrap_or_else(|| x.into()),
    None => home.unwrap_or_else(|| ".".into()).join(".pi/agent"),
  }
}

/// pi's session directories: `PI_CODING_AGENT_SESSION_DIR` when set, else `sessions` in the agent
/// directory and the absolute `sessionDir` of its settings.
pub fn roots() -> Vec<PathBuf> {
  let var = |x: &str| env::var(x).ok().filter(|x| !x.is_empty());
  let home = home_dir();
  let expand = |x: &str| expand_home(x, home.as_deref(), cfg!(windows));
  if let Some(x) = var("PI_CODING_AGENT_SESSION_DIR") {
    return vec![expand(&x).unwrap_or_else(|| x.into())];
  }
  let agent = agent_dir();
  let settings = std::fs::read(agent.join("settings.json")).ok();
  let settings = settings.and_then(|x| {
    let x = x.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&x);
    serde_json::from_slice::<serde_json::Value>(x).ok()
  });
  // A relative `sessionDir` is below each project's directory, where cctrail can't find it.
  let custom = settings.as_ref().and_then(|x| expand(x.get("sessionDir")?.as_str()?));
  let default = agent.join("sessions");
  let custom = custom.filter(|x| *x != default);
  std::iter::once(default).chain(custom).collect()
}

/// The file name in a path as pi logs it, from any platform.
fn base(path: &str) -> &str {
  path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Sessions under the roots, each its own main session in the directory of its header. An entry
/// copied between forks counts once in their lineage (the sessions forked from one another): in
/// the session that holds it nearest its oldest ancestor. A fork of a gone session can't name the
/// rest of its lineage, so its copies are looked for in every other session.
pub fn parse(roots: &[PathBuf], swears: Option<&Dict>) -> Vec<Session> {
  // The pi-subagents extension keeps debug copies of its agents' sessions in `subagent-artifacts`.
  let files: Vec<PathBuf> = jsonl_files(roots)
    .into_iter()
    .filter(|x| !x.components().any(|x| x.as_os_str() == "subagent-artifacts"))
    .collect();
  let parsed: Vec<Parsed> = files.par_iter().map(|x| parse_file(x, swears)).collect();

  // A session file found under two roots, the same name and id, counts once: the longest copy.
  // `--session` may give independent sessions the same name.
  let name = |x: &Path| x.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
  let size = |x: &Path| std::fs::metadata(x).map_or(0, |x| x.len());
  let mut longest: HashMap<(String, &str), usize> = HashMap::new();
  for (i, x) in parsed.iter().enumerate() {
    let best = longest.entry((name(&files[i]), x.session.id.as_str())).or_insert(i);
    if size(&files[i]) > size(&files[*best]) {
      *best = i;
    }
  }
  let winners: HashSet<usize> = longest.into_values().collect();
  let (files, parsed): (Vec<PathBuf>, Vec<Parsed>) = files
    .into_iter()
    .zip(parsed)
    .enumerate()
    .filter_map(|(i, x)| winners.contains(&i).then_some(x))
    .unzip();

  // A source by its path, else by its file name (unique by its id) wherever its directory is now.
  let by_path: HashMap<String, usize> =
    files.iter().enumerate().map(|(i, x)| (x.to_string_lossy().to_string(), i)).collect();
  let mut by_name: HashMap<String, Option<usize>> = HashMap::new();
  for (i, x) in files.iter().enumerate() {
    by_name.entry(name(x)).and_modify(|x| *x = None).or_insert(Some(i));
  }
  let source =
    |x: &str| by_path.get(x).copied().or_else(|| by_name.get(base(x)).copied().flatten());
  // A lineage is named by its oldest ancestor, a session file or a gone one; the depth counts the
  // forks down from it, the top is its oldest session left.
  let lineage = |i: usize| {
    let (mut at, mut depth) = (i, 0);
    while depth < files.len() {
      let Some(parent) = parsed[at].parent.as_deref() else { break };
      match source(parent) {
        Some(j) if j != at => (at, depth) = (j, depth + 1),
        Some(_) => break,
        None => return (format!("gone {}", base(parent)), depth + 1, true, at),
      }
    }
    (format!("file {at}"), depth, false, at)
  };
  let lineages: Vec<(String, usize, bool, usize)> = (0..parsed.len()).map(lineage).collect();
  // Sources before their forks whatever their headers' times say. Forks of gone sessions come last,
  // in pieces by when the oldest session left of each was created: a piece cut off further down a
  // lineage (a → b → c → d → e with a and d gone: b and c, then e) holds later copies.
  let created = |i: usize| (parsed[i].created.is_none(), parsed[i].created);
  let mut order: Vec<usize> = (0..parsed.len()).collect();
  order.sort_by_key(|i| {
    let (_, depth, gone, top) = lineages[*i];
    (gone, created(top), depth, created(*i))
  });
  let mut held: HashMap<&str, HashSet<&Key>> = HashMap::new();
  let mut all: HashSet<&Key> = HashSet::new();
  let mut keep: Vec<Vec<bool>> = vec![vec![]; parsed.len()];
  for i in order {
    let (keys, (root, _, gone, _)) = (&parsed[i].keys, &lineages[i]);
    let copied = held.entry(root).or_default();
    keep[i] = keys.iter().map(|k| !copied.contains(k) && !(*gone && all.contains(k))).collect();
    copied.extend(keys);
    all.extend(keys);
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

fn parse_file(path: &Path, swears: Option<&Dict>) -> Parsed {
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
  let mut replies = Replies::default();

  jsonl(path, |line: Line, raw| {
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
        parent = line.parent.str().filter(|x| !x.is_empty()).map(String::from);
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
        model = line.model_id.str().map(model_id).or(model.take());
        None
      }
      (Some("thinking_level_change"), _) => {
        effort = line.thinking.str().map(String::from).or(effort.take());
        None
      }
      // A prompt, or a shell command the user ran (`!cmd`).
      (Some("message"), Some((_, Some(role @ ("user" | "bashExecution"))))) => ts.map(|x| {
        let mut event = Event::activity(x);
        let line = || serde_json::from_slice::<Value>(raw).ok();
        if let Some(dict) = swears.filter(|_| role == "user")
          && let Some(words) = line().and_then(|x| own_words(&x["message"]["content"]))
        {
          replies.prompt(&mut event, dict, x, &words);
        }
        ("user", event)
      }),
      (Some("message"), Some((m, Some("assistant")))) => {
        if let Some(x) = m.model.str() {
          model = Some(model_id(x));
        }
        let raw = m.usage.0.as_ref();
        let usage = raw.map(RawUsage::usage);
        let event = Event {
          ts,
          usage,
          // The model that answered, when the provider served another (a fallback, a router's).
          model: m.response_model.str().map(model_id).or_else(|| model.clone()),
          effort: m.thinking.str().map(String::from).or_else(|| effort.clone()),
          cost: raw.and_then(RawUsage::cost),
          // An error after the response began still bills the tokens it carries.
          failed: m.stop.str() == Some("error") && usage.is_none_or(|x| x.total() == 0),
          ..Default::default()
        };
        if swears.is_some()
          && let Some(x) = event.model.as_deref().filter(|_| !event.failed)
        {
          replies.answer(x);
        }
        Some(("assistant", event))
      }
      // Requests apart from the turns, which pi counts in a session's totals too. A tool's nested
      // calls may go to any models, so they have none.
      (Some("message"), Some((m, Some("toolResult")))) => {
        m.usage.0.as_ref().map(|x| ("tool", summed(x, None)))
      }
      // Requests outside the conversation, such as cache warming while idle, name their model.
      (Some("usage"), _) => line.usage.0.as_ref().map(|x| {
        let name = line.model.str().map(model_id).or_else(|| model.clone());
        ("usage", Event { background: true, ..new(x, name) })
      }),
      // A summary of compacted or abandoned history runs on the session's model, which pi
      // doesn't log with it. A split turn's compaction is two requests, a branch's summary one.
      (Some("compaction"), _) => {
        line.usage.0.as_ref().map(|x| ("summary", summed(x, model.clone())))
      }
      (Some("branch_summary"), _) => {
        line.usage.0.as_ref().map(|x| ("summary", new(x, model.clone())))
      }
      _ => None,
    };
    if let Some((kind, event)) = event {
      keys.push((kind, ts, millis, event.usage));
      session.events.push(event);
    }
    true
  });

  replies.finish(&mut session.events);
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
  fn reads_typed_prompts_only() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/swears/pi/sessions");
    // A skill's body and a shell command are no prompts; a failed response answers nothing.
    let sonnet = "claude-sonnet-4-5-20250929";
    // `fuck, start`, `now tidy up`, `блять`.
    let expected = [("gpt-5", 2, 1), ("gpt-5", 3, 0), (sonnet, 1, 1)];
    let sessions = parse(std::slice::from_ref(&root), Some(&super::super::tests::dict()));
    assert_eq!(super::super::tests::prompts(&sessions), expected);
    let sessions = parse(&[root], None);
    assert!(sessions.iter().flat_map(|x| &x.events).all(|x| x.prompt.is_none()));
  }

  #[test]
  fn counts_entries_a_fork_copied_once() {
    let sessions = parse(&[fixtures()], None);
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
    let twice = parse(&[fixtures(), fixtures()], None);
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
    // Alike entries in one session count, and a fork's whose source is gone. A time out of range
    // is no time.
    let (x, y) = (usage("x"), usage("y"));
    let moved = |x: &str| x.replace("00:00:0", "00:00:5");
    let (own_x, own_reply) = (moved(&x), moved(reply));
    file("1_one.jsonl", &[&header("one", ""), &x, &y, reply, late]);
    file("2_two.jsonl", &[&header("two", "/gone/0_zero.jsonl"), &own_x, &own_reply]);
    let sessions = parse(std::slice::from_ref(&dir), None);
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
    file("3_three.jsonl", &[&later, change, &own_x, &own_reply, summary]);
    file("4_four.jsonl", &[&header("four", "3_three.jsonl"), summary]);
    // A longer copy of `one` under a second root wins.
    let other = dir.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let longer = [&header("one", "") as &str, &usage("x"), &usage("y"), reply, &usage("z")];
    std::fs::write(other.join("1_one.jsonl"), longer.join("\n")).unwrap();
    let sessions = parse(&[dir.clone(), other], None);
    let total = |id: &str| sessions.iter().find(|x| x.id == id).map_or(0, total);
    let totals = ["one", "two", "three", "four"].map(total);
    assert_eq!(totals, [310, 110, 5, 0]);
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn counts_copies_once_past_a_gone_fork() {
    let dir = std::env::temp_dir().join(format!("cctrail-pi-chain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let header = |id: &str, t: u8, parent: &str| {
      let parent = if parent.is_empty() { "null".into() } else { format!(r#""/s/{parent}""#) };
      format!(
        r#"{{"type":"session","id":"{id}","timestamp":"2026-01-01T00:00:0{t}Z","cwd":"/r","parentSession":{parent}}}"#
      )
    };
    let reply = |t: u8, n: u64| {
      format!(
        r#"{{"type":"message","timestamp":"2026-01-01T00:01:0{t}Z","message":{{"role":"assistant","model":"m","usage":{{"input":{n}}}}}}}"#
      )
    };
    let file =
      |name: &str, lines: &[&str]| std::fs::write(dir.join(name), lines.join("\n")).unwrap();
    // a → b → c, with b gone: c's copy of a's reply counts in a, its copy of b's in c.
    file("1_a.jsonl", &[&header("a", 1, ""), &reply(1, 100)]);
    file(
      "3_c.jsonl",
      &[&header("c", 3, "2_b.jsonl"), &reply(1, 100), &reply(2, 200), &reply(3, 300)],
    );
    // An unrelated session alike to a's reply, created before c, still counts.
    file("0_z.jsonl", &[&header("z", 0, ""), &reply(1, 100)]);
    let sessions = parse(std::slice::from_ref(&dir), None);
    let first = |id: &str| sessions.iter().find(|x| x.id == id).map_or(0, total);
    assert_eq!((first("a"), first("c"), first("z")), (100, 500, 100));

    // a → b → c → d → e with d gone: e, near its gone source, still shares c's copies.
    let put = |path: PathBuf, lines: Vec<String>| std::fs::write(path, lines.join("\n")).unwrap();
    let replies = |n: u8| (1..=n).map(|t| reply(t, t as u64 * 100));
    put(
      dir.join("2_b.jsonl"),
      [header("b", 2, "1_a.jsonl")].into_iter().chain(replies(2)).collect(),
    );
    put(
      dir.join("3_c.jsonl"),
      [header("c", 3, "2_b.jsonl")].into_iter().chain(replies(3)).collect(),
    );
    put(
      dir.join("5_e.jsonl"),
      [header("e", 5, "4_d.jsonl")].into_iter().chain(replies(5)).collect(),
    );
    // Another session that names its file alike, its own.
    std::fs::create_dir_all(dir.join("other")).unwrap();
    put(dir.join("other/0_z.jsonl"), vec![header("y", 1, ""), reply(9, 50)]);
    let sessions = parse(std::slice::from_ref(&dir), None);
    let sum = |id: &str| sessions.iter().find(|x| x.id == id).map_or(0, total);
    assert_eq!(["a", "b", "c", "e", "y", "z"].map(sum), [100, 200, 300, 900, 50, 100]);

    // a gone too: b, c and e each hold what they added, whatever their depth below a gone session.
    std::fs::remove_file(dir.join("1_a.jsonl")).unwrap();
    std::fs::remove_file(dir.join("0_z.jsonl")).unwrap();
    let sessions = parse(std::slice::from_ref(&dir), None);
    let sum = |id: &str| sessions.iter().find(|x| x.id == id).map_or(0, total);
    assert_eq!(["b", "c", "e"].map(sum), [300, 300, 900]);
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn prices_a_branch_summary_as_one_request() {
    let dir = std::env::temp_dir().join(format!("cctrail-pi-summary-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lines = [
      r#"{"type":"session","id":"s","timestamp":"2026-01-01T00:00:00Z","cwd":"/r"}"#,
      r#"{"type":"model_change","timestamp":"2026-01-01T00:00:01Z","modelId":"m"}"#,
      r#"{"type":"branch_summary","timestamp":"2026-01-01T00:00:02Z","usage":{"input":5}}"#,
      r#"{"type":"compaction","timestamp":"2026-01-01T00:00:03Z","usage":{"input":7}}"#,
      r#"{"type":"usage","kind":"cache_warm","timestamp":"2026-01-01T00:05:00Z","model":"w","usage":{"cacheRead":9}}"#,
    ];
    std::fs::write(dir.join("1_s.jsonl"), lines.join("\n")).unwrap();
    let sessions = parse(std::slice::from_ref(&dir), None);
    // A compaction may sum a split turn's two requests; a branch's summary is one. Cache warming
    // runs while no one is at work.
    let flags: Vec<_> =
      sessions[0].events.iter().map(|x| (x.model.as_deref(), x.summed, x.background)).collect();
    let expected = [(Some("m"), false, false), (Some("m"), true, false), (Some("w"), false, true)];
    assert_eq!(flags, expected);
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn reads_turns_summaries_and_failures() {
    let sessions = parse(&[fixtures()], None);
    let main = sessions.iter().find(|x| x.id == "pi-main-1").unwrap();
    let billed: Vec<_> = main.events.iter().filter(|x| x.usage.is_some()).collect();
    let models: Vec<_> = billed.iter().map(|x| x.model.as_deref().unwrap_or("-")).collect();
    let bedrock = "claude-sonnet-4-5-20250929";
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
  fn expands_the_home_directory_as_pi() {
    let dir = std::env::temp_dir();
    let home = Some(dir.as_path());
    assert_eq!(expand_home("~/s", home, false), Some(dir.join("s")));
    assert_eq!(expand_home("~\\s", home, true), Some(dir.join("s")));
    assert_eq!(expand_home("~\\s", home, false), None);
    assert_eq!(expand_home(&dir.to_string_lossy(), home, true), Some(dir.clone()));
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
