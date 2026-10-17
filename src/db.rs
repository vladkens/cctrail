// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rusqlite::{Connection, OpenFlags, Row, params};

use crate::cli::VERSION;
use crate::config::{config_dir, home_dir};
use crate::model::{Event, Prompt, Provider, Session, Speed, Usage};
use crate::swear::Dict;

/// The format of an export, in its `meta`: bumped when the tables change.
const FORMAT_VERSION: &str = "1";

// MARK: Export

/// `cctrail export`: the sessions the reports read, imported ones and prompts' swear counts
/// included. Returns the sessions written.
pub fn export(file: &Path) -> Result<usize> {
  let dict = Dict::load(config_dir().map(|x| x.join("swear")).as_deref())?;
  let mut sessions = crate::parser::parse_history(None, Some(&dict));
  merge(&mut sessions, None);
  let home = home();
  replace(file, |tmp| write(tmp, &sessions, home.as_deref()))?;
  Ok(sessions.len())
}

fn write(path: &Path, sessions: &[Session], home: Option<&str>) -> Result<()> {
  let qs = "
  CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
  CREATE TABLE sessions (
    id INTEGER PRIMARY KEY, harness TEXT NOT NULL, session_id TEXT NOT NULL,
    root_id TEXT NOT NULL, is_main BOOLEAN NOT NULL, workdir TEXT NOT NULL, cwd TEXT NOT NULL,
    remote TEXT
  );
  -- Times in microseconds since the Unix epoch; tokens NULL for an event without usage.
  CREATE TABLE events (
    session INTEGER NOT NULL REFERENCES sessions (id), ts INTEGER, model TEXT,
    input INTEGER, cache_write_5m INTEGER, cache_write_1h INTEGER, cache_read INTEGER,
    output INTEGER, reasoning INTEGER, speed TEXT, effort TEXT,
    message_id TEXT, request_id TEXT, cost REAL, summed BOOLEAN NOT NULL,
    background BOOLEAN NOT NULL, sidechain BOOLEAN NOT NULL, failed BOOLEAN NOT NULL,
    prompt_ts INTEGER, prompt_model TEXT, prompt_words INTEGER, prompt_swears INTEGER,
    prompt_directed BOOLEAN, prompt_hash INTEGER
  );
  ";

  let mut db = Connection::open(path)?;
  let tx = db.transaction()?;
  tx.execute_batch(qs)?;

  let qs = "INSERT INTO meta (key, value)
    VALUES ('format_version', $1), ('cctrail_version', $2), ('exported_at', $3);";
  tx.execute(qs, params![FORMAT_VERSION, VERSION, Timestamp::now().to_string()])?;

  let qs = "INSERT INTO sessions (harness, session_id, root_id, is_main, workdir, cwd, remote)
    VALUES ($1, $2, $3, $4, $5, $6, $7);";
  let mut session = tx.prepare(qs)?;
  let mut event = tx.prepare(&format!("INSERT INTO events VALUES ({});", ["?"; 24].join(", ")))?;

  for s in sessions {
    let (workdir, cwd) = (rebase(&s.workdir, home, Some("~")), rebase(&s.cwd, home, Some("~")));
    let (harness, main) = (s.provider.name(), s.is_main);
    let row = session.insert(params![harness, s.id, s.root_id, main, workdir, cwd, s.remote])?;

    for e in &s.events {
      // History is untrusted: a crafted count past SQLite's integers saturates.
      let t = |f: fn(Usage) -> u64| e.usage.map(|x| f(x).min(i64::MAX as u64) as i64);
      let speed = e.speed.map(|x| if x == Speed::Fast { "fast" } else { "standard" });
      let (id, p) = (e.identity.as_ref(), e.prompt.as_deref());
      #[rustfmt::skip]
      let values = params![
        row, e.ts, e.model, t(|x| x.input), t(|x| x.cache_write_5m), t(|x| x.cache_write_1h),
        t(|x| x.cache_read), t(|x| x.output), t(|x| x.reasoning), speed, e.effort,
        id.map(|x| &x.0), id.and_then(|x| x.1.as_ref()), e.cost,
        e.summed, e.background, e.sidechain, e.failed,
        p.map(|x| x.ts), p.and_then(|x| x.model.as_ref()), p.map(|x| x.words),
        p.map(|x| x.swears), p.map(|x| x.directed), p.map(|x| x.hash as i64),
      ];
      event.execute(values)?;
    }
  }

  drop((session, event));
  Ok(tx.commit()?)
}

// MARK: Import

/// `cctrail import`: copies an export into the data directory, read on every run beside the logs.
/// Returns the sessions in it, none when it was imported before.
pub fn import(file: &Path) -> Result<Option<usize>> {
  let (at, sessions) =
    read(file, None).with_context(|| format!("failed to import {}", file.display()))?;
  let Some(dir) = dir() else { bail!("cannot find the data directory without HOME") };
  let dest = dir.join(format!("{}.db", format!("{at:.6}").replace(':', "-")));
  if dest.exists() {
    return Ok(None);
  }

  std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
  replace(&dest, |tmp| Ok(std::fs::copy(file, tmp).map(|_| ())?))?;
  Ok(Some(sessions.len()))
}

/// Adds the imported sessions the logs lack, newest import first: of a session in both (by harness
/// and id), the logs' copy wins. Their prompts stay: only the Swears view reads them.
pub fn merge(sessions: &mut Vec<Session>, provider: Option<Provider>) {
  let key = |x: &Session| (x.provider, x.id.clone());
  let mut seen: HashSet<_> = sessions.iter().map(key).collect();
  let home = home();

  let files = dir().and_then(|x| std::fs::read_dir(x).ok()).into_iter().flatten().flatten();
  let mut files: Vec<PathBuf> =
    files.map(|x| x.path()).filter(|x| x.extension().is_some_and(|x| x == "db")).collect();
  files.sort();

  for path in files.iter().rev() {
    let items = read(path, home.as_deref()).map(|x| x.1).unwrap_or_else(|e| {
      eprintln!("cctrail: skipped {}: {e:#}", path.display());
      vec![]
    });
    let want = |x: &Session| provider.is_none_or(|p| p == x.provider) && !seen.contains(&key(x));
    let items: Vec<Session> = items.into_iter().filter(want).collect();
    seen.extend(items.iter().map(key));
    sessions.extend(items);
  }
}

/// An export's time and sessions, its `~/…` paths in `home`.
fn read(path: &Path, home: Option<&str>) -> Result<(Timestamp, Vec<Session>)> {
  let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
  let meta = |key: &str| {
    db.query_row("SELECT value FROM meta WHERE key = $1;", [key], |row| row.get::<_, String>(0))
  };
  let version = meta("format_version").context("not a cctrail export")?;
  if version != FORMAT_VERSION {
    bail!("an export of format {version}, this cctrail reads format {FORMAT_VERSION}");
  }

  let (mut sessions, mut index) = (vec![], HashMap::new());
  let mut stmt = db.prepare("SELECT * FROM sessions ORDER BY id;")?;
  let mut rows = stmt.query([])?;
  while let Some(row) = rows.next()? {
    // A harness this cctrail doesn't know, from a newer one's export, is skipped.
    let Some(provider) = Provider::parse(&row.get::<_, String>("harness")?) else { continue };
    index.insert(row.get::<_, i64>("id")?, sessions.len());
    sessions.push(session_from_row(row, provider, home)?);
  }

  let mut stmt = db.prepare("SELECT * FROM events ORDER BY rowid;")?;
  for x in stmt.query_map([], event_from_row)? {
    let (id, event) = x?;
    if let Some(i) = index.get(&id) {
      sessions[*i].events.push(event);
    }
  }

  Ok((meta("exported_at")?.parse()?, sessions))
}

fn session_from_row(row: &Row<'_>, provider: Provider, home: Option<&str>) -> Result<Session> {
  let path = |key: &str| row.get::<_, String>(key).map(|x| rebase(&x, Some("~"), home));
  let mut x = Session::new(&row.get::<_, String>("session_id")?, row.get("is_main")?, "", provider);
  (x.root_id, x.workdir, x.cwd, x.remote) =
    (row.get("root_id")?, path("workdir")?, path("cwd")?, row.get("remote")?);
  Ok(x)
}

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<(i64, Event)> {
  let t = |key: &str| row.get::<_, Option<i64>>(key).map(|x| x.unwrap_or(0).max(0) as u64);
  #[rustfmt::skip]
  let usage = Usage {
    input: t("input")?, cache_write_5m: t("cache_write_5m")?, cache_write_1h: t("cache_write_1h")?,
    cache_read: t("cache_read")?, output: t("output")?, reasoning: t("reasoning")?,
  };
  let prompt = match row.get::<_, Option<i64>>("prompt_ts")? {
    #[rustfmt::skip]
    Some(ts) => Some(Box::new(Prompt {
      ts, model: row.get("prompt_model")?, words: row.get("prompt_words")?,
      swears: row.get("prompt_swears")?, directed: row.get("prompt_directed")?,
      hash: row.get::<_, i64>("prompt_hash")? as u64,
    })),
    None => None,
  };

  let (message_id, request_id): (Option<String>, _) =
    (row.get("message_id")?, row.get("request_id")?);
  #[rustfmt::skip]
  let event = Event {
    ts: row.get("ts")?, usage: row.get::<_, Option<i64>>("input")?.map(|_| usage),
    model: row.get("model")?, effort: row.get("effort")?,
    speed: row.get::<_, Option<String>>("speed")?.as_deref().and_then(Speed::parse),
    identity: message_id.map(|x| (x, request_id)), cost: row.get("cost")?,
    summed: row.get("summed")?, background: row.get("background")?,
    sidechain: row.get("sidechain")?, failed: row.get("failed")?, prompt,
  };
  Ok((row.get("session")?, event))
}

// MARK: Files

/// Writes `path` through a temporary file beside it, so a failed write leaves the old file as it
/// was.
fn replace(path: &Path, write: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
  let tmp = path.with_added_extension(format!("{}.tmp", std::process::id()));
  let res = write(&tmp).and_then(|_| Ok(std::fs::rename(&tmp, path)?));
  if res.is_err() {
    let _ = std::fs::remove_file(&tmp);
  }
  res.with_context(|| format!("failed to write {}", path.display()))
}

/// `cctrail` in the data directory: `XDG_DATA_HOME`, else `%LOCALAPPDATA%` on Windows and
/// `~/.local/share` elsewhere.
fn dir() -> Option<PathBuf> {
  let var = |x: &str| std::env::var_os(x).filter(|x| !x.is_empty()).map(PathBuf::from);
  let root = match var("XDG_DATA_HOME") {
    Some(x) => x,
    None if cfg!(windows) => var("LOCALAPPDATA")?,
    None => home_dir()?.join(".local/share"),
  };
  Some(root.join("cctrail"))
}

/// The home directory without a trailing separator; none for a root.
fn home() -> Option<String> {
  let home = home_dir()?.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
  (!home.is_empty()).then_some(home)
}

/// `path` with its leading directory `from` as `to`: the home as `~` and back.
fn rebase(path: &str, from: Option<&str>, to: Option<&str>) -> String {
  match (from.and_then(|x| path.strip_prefix(x)), to) {
    (Some(x), Some(to)) if x.is_empty() || x.starts_with(['/', '\\']) => format!("{to}{x}"),
    _ => path.to_string(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::parser::{claude, codex, fx, pi};

  fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cctrail-db-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
  }

  fn parse(set: &str) -> Vec<Session> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join(set);
    let dict = Dict::load(None).unwrap();
    let mut sessions = claude::parse(&[dir.join("claude")], Some(&dict));
    sessions.extend(codex::parse(&[dir.join("codex")], Some(&dict)));
    sessions.extend(pi::parse(&[dir.join("pi/sessions")], Some(&dict)));
    sessions.extend(fx::parse(&dir.join("fx"), Some(&dict)));
    sessions
  }

  #[test]
  fn round_trips_the_sessions() -> Result<()> {
    for set in ["fixtures", "swears"] {
      let sessions = parse(set);
      let file = tmp(set).join("export.db");
      write(&file, &sessions, Some("/repo"))?;

      let db = Connection::open(&file)?;
      let count = |qs: &str| db.query_row(qs, [], |row| row.get::<_, i64>(0));
      let events: Vec<&Event> = sessions.iter().flat_map(|x| &x.events).collect();
      let tokens: u64 = events.iter().filter_map(|x| x.usage).map(|x| x.total()).sum();
      let prompts: u32 = events.iter().filter_map(|x| x.prompt.as_ref()).map(|x| x.swears).sum();
      assert_eq!(count("SELECT count(*) FROM sessions")?, sessions.len() as i64);
      assert_eq!(count("SELECT count(*) FROM events")?, events.len() as i64);
      let qs =
        "SELECT sum(input + cache_write_5m + cache_write_1h + cache_read + output + reasoning)
        FROM events";
      assert_eq!(count(qs)?, tokens as i64, "{set}");
      assert_eq!(count("SELECT coalesce(sum(prompt_swears), 0) FROM events")?, prompts as i64);
      assert!(prompts > 0 || set == "fixtures");

      // Paths under the home as `~/…`.
      let under = sessions.iter().filter(|x| x.workdir == "/repo").count();
      assert!(under > 0);
      assert_eq!(count("SELECT count(*) FROM sessions WHERE workdir = '~'")?, under as i64);
      assert_eq!(count("SELECT count(*) FROM sessions WHERE workdir LIKE '/repo%'")?, 0);

      // Read back in another home: the same sessions, their paths there.
      let mut expected = sessions.clone();
      let paths = expected.iter_mut().flat_map(|x| [&mut x.workdir, &mut x.cwd]);
      paths.filter(|x| *x == "/repo").for_each(|x| *x = "/home/b".into());
      assert!(expected.iter().any(|x| x.workdir == "/home/b"));
      assert_eq!(
        format!("{:?}", read(&file, Some("/home/b"))?.1),
        format!("{expected:?}"),
        "{set}"
      );
    }
    Ok(())
  }

  #[test]
  fn keeps_the_old_file_when_writing_fails() {
    let file = tmp("fails").join("export.db");
    std::fs::write(&file, "old").unwrap();
    let fail = |tmp: &Path| {
      write(tmp, &parse("fixtures"), None)?;
      bail!("no")
    };
    assert!(replace(&file, fail).is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
    assert_eq!(std::fs::read_dir(file.parent().unwrap()).unwrap().count(), 1);
  }

  #[test]
  fn folds_paths_under_the_home() {
    let fold = |x, home| rebase(x, home, Some("~"));
    let expand = |x, home| rebase(x, Some("~"), home);
    let cases = [
      ("/h/me/x", "~/x"),
      ("/h/me", "~"),
      ("/h/meow/x", "/h/meow/x"),
      ("/other", "/other"),
      ("C:\\h\\me\\x", "C:\\h\\me\\x"),
    ];
    for (path, folded) in cases {
      assert_eq!(fold(path, Some("/h/me")), folded);
      assert_eq!(expand(folded, Some("/h/me")), path);
    }
    assert_eq!(fold("C:\\h\\me\\x", Some("C:\\h\\me")), "~\\x");
    assert_eq!(expand("~/x", Some("/h/you")), "/h/you/x");
    assert_eq!(expand("~/x", None), "~/x");
    assert_eq!(expand("~x", Some("/h/me")), "~x");
  }
}
