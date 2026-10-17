// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use rayon::prelude::*;
use rusqlite::{Connection, params};

use crate::config::home_dir;
use crate::model::Speed;
use crate::parser::{self, Links, Source};
use crate::swear::Dict;

/// What the parsers read, kept with each file: bumped when a parser changes what it reads.
pub const PARSER_VERSION: i64 = 1;

/// The format of the file, in `meta`.
const FORMAT_VERSION: &str = "1";

/// A file's one session or none, an event's usage in six token columns, its flags (with its
/// prompt's `directed`) as bits; models and efforts in `strings`. What a later `import` needs too:
/// each file as read alone, with the links its harness's work across files takes.
const SCHEMA: &str = "
  CREATE TABLE files (id INTEGER PRIMARY KEY, harness TEXT NOT NULL, root TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE, size INTEGER NOT NULL, mtime INTEGER NOT NULL,
    parser_version INTEGER NOT NULL, dict INTEGER NOT NULL);
  CREATE TABLE sessions (id INTEGER PRIMARY KEY, file INTEGER NOT NULL UNIQUE REFERENCES files,
    harness TEXT NOT NULL, session_id TEXT NOT NULL, is_main INTEGER NOT NULL,
    workdir TEXT NOT NULL, cwd TEXT NOT NULL, remote TEXT, links TEXT NOT NULL);
  CREATE INDEX sessions_by_id ON sessions (harness, session_id);
  CREATE TABLE strings (id INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE);
  CREATE TABLE events (session INTEGER NOT NULL REFERENCES sessions, seq INTEGER NOT NULL,
    ts INTEGER, model INTEGER REFERENCES strings, input INTEGER, cache_write_5m INTEGER,
    cache_write_1h INTEGER, cache_read INTEGER, output INTEGER, reasoning INTEGER, speed INTEGER,
    effort INTEGER REFERENCES strings, cost REAL, flags INTEGER NOT NULL, message_id TEXT,
    request_id TEXT, prompt_ts INTEGER, prompt_model INTEGER REFERENCES strings, words INTEGER,
    swears INTEGER, prompt_hash INTEGER, PRIMARY KEY (session, seq)) WITHOUT ROWID;
  CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
  PRAGMA user_version = 1;
";

/// The bits of `flags`: summed, background, sidechain, failed, the prompt's `directed`.
const FLAGS: [u32; 5] = [1, 2, 4, 8, 16];

/// `cctrail export`: every harness's history as read, prompts' swear counts included, into a new
/// SQLite file. Paths under the home directory are kept as `~/…`. Returns the sessions written.
pub fn export(file: &Path) -> Result<usize> {
  let dict = Dict::user()?;
  let harnesses = parser::harnesses(None).into_par_iter();
  let read = |(p, roots): (_, Vec<_>)| parser::read(p, &roots, Some(&dict), dict.fingerprint());
  let sources: Vec<Source> = harnesses.flat_map(read).collect();
  let home = home_dir().map(|x| x.to_string_lossy().trim_end_matches(['/', '\\']).to_string());
  write(file, &sources, home.as_deref().filter(|x| !x.is_empty()))
}

/// Writes the sources into a new database at `file`, through a temporary file of its own beside
/// it, so a failed export leaves an earlier one as it was.
fn write(file: &Path, sources: &[Source], home: Option<&str>) -> Result<usize> {
  let name = file.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
  let tmp = file.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
  let context = || format!("failed to write {}", file.display());
  std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).with_context(context)?;
  let res = (|| -> Result<usize> {
    let mut conn = Connection::open(&tmp)?;
    let tx = conn.transaction()?;
    tx.execute_batch(SCHEMA)?;
    let now = jiff::Timestamp::now().to_string();
    let meta = [("format_version", FORMAT_VERSION), ("cctrail_version", crate::cli::VERSION)];
    for (key, value) in meta.into_iter().chain([("exported_at", now.as_str())]) {
      tx.execute("INSERT INTO meta VALUES (?1, ?2)", [key, value])?;
    }
    let n = insert(&tx, |x| fold(home, x), sources)?;
    tx.commit()?;
    drop(conn);
    std::fs::rename(&tmp, file)?;
    Ok(n)
  })();
  if res.is_err() {
    let _ = std::fs::remove_file(&tmp);
  }
  res.with_context(context)
}

/// A path under the home directory as `~/…`; one starting with `~` gets another.
fn fold(home: Option<&str>, path: &str) -> String {
  let rest = home.and_then(|x| path.strip_prefix(x));
  match rest.filter(|x| x.is_empty() || x.starts_with(['/', '\\'])) {
    _ if path.starts_with('~') => format!("~{path}"),
    Some(rest) => format!("~{rest}"),
    None => path.to_string(),
  }
}

/// The paths a session's links hold.
fn link_paths(links: &mut Links) -> Vec<&mut String> {
  match links {
    Links::Codex(x) => vec![&mut x.rollout.workdir],
    Links::Pi(x) => x.parent.iter_mut().collect(),
    _ => vec![],
  }
}

/// Returns the sessions inserted.
fn insert(tx: &Connection, fold: impl Fn(&str) -> String, sources: &[Source]) -> Result<usize> {
  let (mut strings, mut seen, mut n) = (HashMap::new(), HashSet::new(), 0);
  let mut file = tx.prepare(
    "INSERT INTO files (harness, root, path, size, mtime, parser_version, dict)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
  )?;
  let mut session = tx.prepare(
    "INSERT INTO sessions (file, harness, session_id, is_main, workdir, cwd, remote, links)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
  )?;
  let mut event = tx.prepare(&format!("INSERT INTO events VALUES ({})", ["?"; 21].join(", ")))?;
  let mut string = tx.prepare("INSERT INTO strings (value) VALUES (?1)")?;
  let mut intern = |x: Option<&String>| -> Result<Option<i64>> {
    let Some(x) = x else { return Ok(None) };
    if !strings.contains_key(x) {
      strings.insert(x.clone(), string.insert([x])?);
    }
    Ok(strings.get(x).copied())
  };

  // A file under two roots (pi's) once.
  for x in sources.iter().filter(|x| seen.insert(&x.path)) {
    let (harness, path) = (x.provider.name(), fold(&x.path.to_string_lossy()));
    let (size, root, (parser, dict)) = (x.size as i64, fold(&x.root), x.version);
    let row = file.insert(params![harness, root, path, size, x.mtime, parser, dict])?;
    let Some(s) = &x.unit.session else { continue };
    let mut links = x.unit.links.clone();
    link_paths(&mut links).into_iter().for_each(|x| *x = fold(x));
    let (workdir, cwd, links) = (fold(&s.workdir), fold(&s.cwd), serde_json::to_string(&links)?);
    let row =
      session.insert(params![row, harness, s.id, s.is_main, workdir, cwd, s.remote, links])?;
    n += 1;
    for (seq, e) in s.events.iter().enumerate() {
      let u = e.usage.map(|x| {
        [x.input, x.cache_write_5m, x.cache_write_1h, x.cache_read, x.output, x.reasoning]
      });
      let u = |i: usize| u.map(|x| x[i] as i64);
      let p = e.prompt.as_deref();
      let bits = [e.summed, e.background, e.sidechain, e.failed, p.is_some_and(|x| x.directed)];
      let flags: u32 = bits.iter().zip(FLAGS).filter(|x| *x.0).map(|x| x.1).sum();
      let (id, request) = e.identity.as_ref().map(|x| (&x.0, x.1.as_ref())).unzip();
      let (model, effort) = (intern(e.model.as_ref())?, intern(e.effort.as_ref())?);
      let speed = e.speed.map(|x| i64::from(x == Speed::Fast));
      let prompt_model = intern(p.and_then(|x| x.model.as_ref()))?;
      event.execute(params![
        row,
        seq as i64,
        e.ts,
        model,
        u(0),
        u(1),
        u(2),
        u(3),
        u(4),
        u(5),
        speed,
        effort,
        e.cost,
        flags,
        id,
        request.flatten(),
        p.map(|x| x.ts),
        prompt_model,
        p.map(|x| x.words),
        p.map(|x| x.swears),
        p.map(|x| x.hash as i64),
      ])?;
    }
  }
  Ok(n)
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use anyhow::bail;
  use rusqlite::Row;

  use super::*;
  use crate::model::{Event, Prompt, Provider, Session, Usage};

  /// All rows of a query, each through `f`.
  fn rows(conn: &Connection, sql: &str, mut f: impl FnMut(&Row) -> Result<()>) -> Result<()> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    while let Some(x) = rows.next()? {
      f(x)?;
    }
    Ok(())
  }

  /// A folded path in this home; one folded on another platform with this one's separators.
  fn unfold(home: Option<&str>, path: &str) -> String {
    let foreign = if cfg!(windows) { '/' } else { '\\' };
    match (path.strip_prefix('~'), home) {
      (Some(rest), _) if rest.starts_with('~') => rest.to_string(),
      (Some(rest), Some(home)) if rest.starts_with(foreign) => {
        format!("{home}{}", rest.replace(foreign, std::path::MAIN_SEPARATOR_STR))
      }
      (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
        format!("{home}{rest}")
      }
      _ => path.to_string(),
    }
  }

  fn load(conn: &Connection, home: Option<&str>) -> Result<Vec<Source>> {
    let unfold = |x: String| unfold(home, &x);
    let mut strings: HashMap<i64, String> = HashMap::new();
    rows(conn, "SELECT id, value FROM strings", |x| Ok(_ = strings.insert(x.get(0)?, x.get(1)?)))?;
    let (mut sources, mut index) = (vec![], HashMap::new());
    let sql = "SELECT f.harness, f.root, f.path, f.size, f.mtime, f.parser_version, f.dict, s.id,
        s.session_id, s.is_main, s.workdir, s.cwd, s.remote, s.links
      FROM files f LEFT JOIN sessions s ON s.file = f.id";
    rows(conn, sql, |x| {
      let harness: String = x.get(0)?;
      let Some(provider) = Provider::parse(&harness) else { bail!("unknown harness {harness}") };
      let mut unit = parser::Unit::default();
      if let Some(row) = x.get::<_, Option<i64>>(7)? {
        let id: String = x.get(8)?;
        let mut session = Session::new(&id, x.get(9)?, &unfold(x.get(10)?), provider);
        (session.cwd, session.remote) = (unfold(x.get(11)?), x.get(12)?);
        unit.links = serde_json::from_str(&x.get::<_, String>(13)?)
          .with_context(|| format!("bad links of {harness} session {id}"))?;
        link_paths(&mut unit.links).into_iter().for_each(|x| *x = unfold(std::mem::take(x)));
        unit.session = Some(session);
        index.insert(row, sources.len());
      }
      let (root, path) = (unfold(x.get(1)?), unfold(x.get(2)?).into());
      let (size, mtime, version) = (x.get::<_, i64>(3)? as u64, x.get(4)?, (x.get(5)?, x.get(6)?));
      sources.push(Source { provider, root, path, size, mtime, version, unit });
      Ok(())
    })?;
    rows(conn, "SELECT * FROM events ORDER BY session, seq", |x| {
      let event = read_event(x, &strings)?;
      let session = index.get(&x.get::<_, i64>(0)?).and_then(|i| sources[*i].unit.session.as_mut());
      if let Some(x) = session {
        x.events.push(event);
      }
      Ok(())
    })?;
    Ok(sources)
  }

  fn read_event(x: &Row, strings: &HashMap<i64, String>) -> Result<Event> {
    let int = |i: usize| x.get::<_, Option<i64>>(i).map(|x| x.map(|x| x as u64));
    let string = |i: usize| -> Result<_> {
      Ok(x.get::<_, Option<i64>>(i)?.and_then(|x| strings.get(&x).cloned()))
    };
    let flag = |i: usize| x.get::<_, u32>(13).map(|x| x & FLAGS[i] != 0);
    let tokens = [int(4)?, int(5)?, int(6)?, int(7)?, int(8)?, int(9)?];
    let usage = tokens[0].map(|input| {
      let [_, cache_write_5m, cache_write_1h, cache_read, output, reasoning] =
        tokens.map(Option::unwrap_or_default);
      Usage { input, cache_write_5m, cache_write_1h, cache_read, output, reasoning }
    });
    let prompt = match x.get::<_, Option<i64>>(16)? {
      Some(ts) => Some(Box::new(Prompt {
        ts,
        model: string(17)?,
        words: x.get(18)?,
        swears: x.get(19)?,
        directed: flag(4)?,
        hash: int(20)?.unwrap_or_default(),
      })),
      None => None,
    };
    let identity = match x.get::<_, Option<String>>(14)? {
      Some(id) => Some((id, x.get(15)?)),
      None => None,
    };
    let speed =
      x.get::<_, Option<i64>>(10)?.map(|x| [Speed::Standard, Speed::Fast][(x == 1) as usize]);
    Ok(Event {
      ts: x.get(2)?,
      usage,
      model: string(3)?,
      speed,
      effort: string(11)?,
      identity,
      cost: x.get(12)?,
      summed: flag(0)?,
      background: flag(1)?,
      sidechain: flag(2)?,
      failed: flag(3)?,
      prompt,
    })
  }

  fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for x in std::fs::read_dir(src).unwrap().map(Result::unwrap) {
      match x.file_type().unwrap().is_dir() {
        true => copy_tree(&x.path(), &dst.join(x.file_name())),
        false => _ = std::fs::copy(x.path(), dst.join(x.file_name())).unwrap(),
      }
    }
  }

  fn query<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> Vec<T> {
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |x| x.get(0)).unwrap().map(Result::unwrap).collect()
  }

  #[test]
  fn exports_what_the_parse_reads() {
    let dict = Dict::load(None).unwrap();
    for set in ["fixtures", "swears"] {
      let dir = std::env::temp_dir().join(format!("cctrail-export-{}-{set}", std::process::id()));
      let _ = std::fs::remove_dir_all(&dir);
      let home = dir.join("home");
      copy_tree(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join(set), &home);
      // A session in a directory under the home.
      let cwd = home.join("Code/app").to_string_lossy().replace('\\', "\\\\");
      let line = format!(
        r#"{{"type": "user", "message": {{"content": "go"}}, "timestamp": "2026-01-02T00:00:00Z", "cwd": "{cwd}"}}"#
      );
      std::fs::create_dir_all(home.join("claude/projects/app")).unwrap();
      std::fs::write(home.join("claude/projects/app/s-home-1.jsonl"), line).unwrap();
      let harnesses = [
        (Provider::Claude, vec![home.join("claude")]),
        (Provider::Codex, vec![home.join("codex")]),
        // The same files under two roots go in once.
        (Provider::Pi, vec![home.join("pi/sessions"), home.join("pi/sessions")]),
        (Provider::Fx, vec![home.join("fx")]),
      ];
      let read = |(p, roots): &(Provider, Vec<PathBuf>)| {
        parser::read(*p, roots, Some(&dict), dict.fingerprint())
      };
      let sources: Vec<Source> = harnesses.iter().flat_map(read).collect();
      let file = dir.join("export.db");
      std::fs::write(&file, "an earlier export").unwrap();
      let ours = home.to_string_lossy().to_string();
      let n = write(&file, &sources, Some(&ours)).unwrap();
      assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "only the export beside the home");

      // Read back, the sessions and events are the parse's.
      let conn = Connection::open(&file).unwrap();
      let loaded = load(&conn, Some(&ours)).unwrap();
      assert_eq!(n, loaded.iter().filter(|x| x.unit.session.is_some()).count());
      for (p, roots) in &harnesses {
        let mine = loaded.iter().filter(|x| x.provider == *p).cloned().collect();
        let got = format!("{:#?}", parser::combine(*p, roots, mine));
        assert_eq!(got, format!("{:#?}", parser::parse(*p, roots, Some(&dict))), "{set} {p:?}");
        assert!(got.contains("Prompt"));
      }
      // Its events and token totals, each file once.
      let mut seen = HashSet::new();
      let once = sources.iter().filter(|x| seen.insert(&x.path));
      let events: Vec<&Event> =
        once.flat_map(|x| x.unit.session.iter().flat_map(|x| &x.events)).collect();
      let tokens: u64 = events.iter().filter_map(|x| x.usage).map(|x| x.total()).sum();
      let sql = "SELECT count(*), sum(input + cache_write_5m + cache_write_1h + cache_read + output
        + reasoning) FROM events";
      let stored: (i64, i64) = conn.query_row(sql, [], |x| Ok((x.get(0)?, x.get(1)?))).unwrap();
      assert_eq!(stored, (events.len() as i64, tokens as i64), "{set}");
      // Its paths under the home as `~/…`.
      let paths: Vec<String> = query(
        &conn,
        "SELECT root FROM files UNION ALL SELECT path FROM files UNION ALL SELECT workdir FROM sessions
          UNION ALL SELECT links FROM sessions",
      );
      assert!(paths.iter().all(|x| !x.contains(&ours)), "{paths:?}");
      let sep = std::path::MAIN_SEPARATOR;
      assert!(paths.contains(&format!("~{sep}Code{sep}app")));
      assert!(paths.iter().filter(|x| x.starts_with('~')).count() > n);
      assert_eq!(
        query::<String>(&conn, "SELECT value FROM meta WHERE key = 'format_version'"),
        ["1"]
      );
      // Another home reads them in its own.
      let theirs = load(&conn, Some("/elsewhere")).unwrap();
      let paths = theirs.iter().map(|x| x.path.to_string_lossy().to_string());
      assert!(paths.into_iter().all(|x| x.starts_with("/elsewhere")));
      drop(conn);
      std::fs::remove_dir_all(&dir).unwrap();
    }
  }

  #[test]
  fn folds_paths_under_the_home() {
    let native = |x: &str| x.replace('/', std::path::MAIN_SEPARATOR_STR);
    let home = native("/h/me");
    let cases = [
      ("/h/me/x", "~/x"),
      ("/h/me", "~"),
      ("/h/meow/x", "/h/meow/x"),
      ("~x", "~~x"),
      ("/other", "/other"),
      ("unknown", "unknown"),
    ];
    for (path, folded) in cases.map(|(x, y)| (native(x), native(y))) {
      assert_eq!(fold(Some(&home), &path), folded);
      assert_eq!(unfold(Some(&home), &folded), path);
    }
    assert_eq!(unfold(Some(&native("/h/you")), &native("~/x")), native("/h/you/x"));
    // Folded on another platform.
    let foreign = if cfg!(windows) { "~/x/y" } else { "~\\x\\y" };
    assert_eq!(unfold(Some(&home), foreign), native("/h/me/x/y"));
  }
}
