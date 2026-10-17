// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rusqlite::{Connection, OpenFlags, Row, params};

use crate::cli::VERSION;
use crate::config::{config_dir, home_dir};
use crate::model::{Event, Prompt, Provider, Session, Speed, Usage};
use crate::parser::{fx, history_roots, pi};
use crate::swear::Dict;

/// The format of an export, in its `meta`: bumped when the tables change.
const FORMAT_VERSION: &str = "1";

// MARK: Export

/// `cctrail export`: the sessions the reports read, imported ones and prompts' swear counts
/// included. Returns the sessions written.
pub fn export(file: &Path) -> Result<usize> {
  outside_history(file)?;
  let dict = Dict::load(config_dir().map(|x| x.join("swear")).as_deref())?;
  let mut sessions = crate::parser::parse_history(None, Some(&dict));
  merge(&mut sessions, None);
  let home = home();
  replace(file, |tmp| write(tmp, &sessions, home.as_deref()))?;
  Ok(sessions.len())
}

/// Refuses a file in a harness's home (its profiles, pi's session directories), which cctrail only
/// reads.
fn outside_history(file: &Path) -> Result<()> {
  let dir = file.parent().filter(|x| !x.as_os_str().is_empty()).unwrap_or(Path::new("."));
  // A missing directory fails the write, with its path.
  let Ok(dir) = dir.canonicalize() else { return Ok(()) };
  let mut roots = history_roots("CLAUDE_CONFIG_DIR", ".claude");
  roots.extend(history_roots("CODEX_HOME", ".codex").into_iter().chain(pi::roots()));
  roots.extend([pi::agent_dir(), fx::root()]);
  match roots.iter().filter_map(|x| x.canonicalize().ok()).find(|x| dir.starts_with(x)) {
    Some(x) => {
      bail!("{} is in the history in {}, which cctrail only reads", file.display(), x.display())
    }
    None => Ok(()),
  }
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
  replace(&dest, |tmp| {
    // Into the owner-only file: `fs::copy` would give it the export's mode.
    let mut out = std::fs::OpenOptions::new().write(true).open(tmp)?;
    std::io::copy(&mut std::fs::File::open(file)?, &mut out)?;
    Ok(())
  })?;
  Ok(Some(sessions.len()))
}

/// Adds the imported sessions the logs lack, newest import first: of a session in both (by harness
/// and id), the logs' copy wins. Their prompts stay: only the Swears view reads them.
pub fn merge(sessions: &mut Vec<Session>, provider: Option<Provider>) {
  let key = |x: &Session| (x.provider, x.id.clone());
  let mut seen: HashSet<_> = sessions.iter().map(key).collect();
  let fx = |x: &Session| (x.provider == Provider::Fx).then(|| x.root_id.clone());
  let mut trees: HashSet<_> = sessions.iter().filter_map(fx).collect();
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
    // The logs win even where they lack what the import has: a Claude agent whose log alone is
    // gone stays the parent's fallback, with its tokens but not its model, and an fx agent whose
    // main session is gone gets no share of the tree's usage back.
    let want = |x: &Session| provider.is_none_or(|p| p == x.provider) && !seen.contains(&key(x));
    let mut items: Vec<Session> = items.into_iter().filter(want).collect();
    // fx spreads a tree's usage over its sessions: in a tree already read, a session adds only its
    // time.
    let known = items.iter_mut().filter(|x| fx(x).is_some_and(|x| trees.contains(&x)));
    known.flat_map(|x| &mut x.events).for_each(|x| x.usage = None);
    seen.extend(items.iter().map(key));
    trees.extend(items.iter().filter_map(fx));
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
  // A time past jiff's range, only in a crafted file, is none.
  let ts = |key: &str| {
    let x = row.get::<_, Option<i64>>(key);
    x.map(|x| x.filter(|x| Timestamp::from_microsecond(*x).is_ok()))
  };
  #[rustfmt::skip]
  let usage = Usage {
    input: t("input")?, cache_write_5m: t("cache_write_5m")?, cache_write_1h: t("cache_write_1h")?,
    cache_read: t("cache_read")?, output: t("output")?, reasoning: t("reasoning")?,
  };
  let prompt = match ts("prompt_ts")? {
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
    ts: ts("ts")?, usage: row.get::<_, Option<i64>>("input")?.map(|_| usage),
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
  // Owner-only, as the history is private: SQLite's journal takes the same mode.
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
  let res = options.open(&tmp).map_err(Into::into).and_then(|_| write(&tmp));
  let res = res.and_then(|_| Ok(std::fs::rename(&tmp, path)?));
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

/// `path` with its leading directory `from` as `to`: the home as `~` and back. On Windows the rest
/// is kept with `/` after `~` and read back with `\\`, so it moves to any platform.
fn rebase(path: &str, from: Option<&str>, to: Option<&str>) -> String {
  // Windows paths compare with either separator and in any case, as in `workdir`.
  let same = |x: &str| x.replace('\\', "/").to_lowercase();
  let strip = |x: &str| match cfg!(windows) {
    true => path.get(..x.len()).filter(|p| same(p) == same(x)).map(|_| &path[x.len()..]),
    false => path.strip_prefix(x),
  };
  match (from.and_then(strip), to) {
    (Some(x), Some(to)) if x.is_empty() || x.starts_with(['/', '\\']) => match cfg!(windows) {
      true => format!("{to}{}", x.replace(['/', '\\'], if to == "~" { "/" } else { "\\" })),
      false => format!("{to}{x}"),
    },
    _ => path.to_string(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::parser::{claude, codex};

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

  #[cfg(unix)]
  #[test]
  fn writes_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let file = tmp("mode").join("export.db");
    std::fs::write(&file, "old").unwrap();
    replace(&file, |tmp| write(tmp, &parse("fixtures"), None)).unwrap();
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
  }

  #[test]
  fn folds_paths_under_the_home() {
    let native = |x: &str| x.replace('/', std::path::MAIN_SEPARATOR_STR);
    let fold = |x: &str, home: &str| rebase(&native(x), Some(&native(home)), Some("~"));
    let expand = |x: &str, home: Option<&str>| rebase(x, Some("~"), home);
    // Kept with `/` after `~` on any platform, read back with this one's separators.
    let cases = [
      ("/h/me/x/y", "~/x/y"),
      ("/h/me", "~"),
      ("/h/meow/x", "/h/meow/x"),
      ("/other", "/other"),
      ("C:\\h\\me\\x", "C:\\h\\me\\x"),
    ];
    for (path, folded) in cases {
      let stored = if folded.starts_with('~') { folded.to_string() } else { native(folded) };
      assert_eq!(fold(path, "/h/me"), stored);
      assert_eq!(expand(&stored, Some(&native("/h/me"))), native(path));
    }
    assert_eq!(expand("~/x", Some(&native("/h/you"))), native("/h/you/x"));
    assert_eq!(expand("~/x", None), "~/x");
    assert_eq!(expand("~x", Some("/h/me")), "~x");
    // Windows paths compare with either separator and in any case, mixed ones kept with `/`; Unix
    // ones exactly.
    let home = Some("C:\\Users\\me");
    for path in ["C:/Users/me/repo", "c:\\users\\me\\repo", "C:\\Users\\me/Code\\app"] {
      let folded = match cfg!(windows) {
        true => format!("~{}", path[11..].replace('\\', "/")),
        false if path.starts_with("C:\\Users\\me") => format!("~{}", &path[11..]),
        false => path.to_string(),
      };
      assert_eq!(rebase(path, home, Some("~")), folded);
    }
    let upper = if cfg!(windows) { "~/x" } else { "/H/me/x" };
    assert_eq!(rebase("/H/me/x", Some("/h/me"), Some("~")), upper);
  }
}
