// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags, Row, params};

use crate::config::home_dir;
use crate::model::{Event, Prompt, Provider, Session, Speed, Usage};
use crate::parser::{self, Links, Source};
use crate::swear::Dict;

/// What the parsers read, as the ledger keeps it: bumped when a parser changes, so the files still
/// here are read again (after a backup). What was read of the files gone since stays as it was.
pub const PARSER_VERSION: i64 = 1;

/// A file's one session or none, an event's usage in six token columns, its flags (with its
/// prompt's `directed`) as bits; models and efforts in `strings`.
const SCHEMA: &str = "
  PRAGMA journal_mode = WAL;
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
  PRAGMA user_version = 1;
";

/// The bits of `flags`: summed, background, sidechain, failed, the prompt's `directed`.
const FLAGS: [u32; 5] = [1, 2, 4, 8, 16];

pub fn enabled() -> bool {
  std::env::var_os("CCTRAIL_NO_LEDGER").is_none_or(|x| x.is_empty() || x == "0")
}

/// The harnesses' sessions through the ledger; see `Ledger::sessions`.
pub fn sessions(
  harnesses: &[(Provider, Vec<PathBuf>)],
  swears: Option<&Dict>,
) -> Result<Vec<Session>> {
  // The ledger keeps the prompts' counts for the interactive view whatever the command.
  match swears {
    Some(x) => Ledger::new()?.sessions(harnesses, x, false),
    None => Ledger::new()?.sessions(harnesses, &Dict::user()?, true),
  }
}

/// The parsed history in a database: `cctrail/ledger.db` in `$XDG_DATA_HOME`, else in
/// `~/.local/share`, on Windows in `%LOCALAPPDATA%`. Paths under the home directory are kept as
/// `~/…`.
pub struct Ledger {
  path: PathBuf,
  home: Option<String>,
}

impl Ledger {
  pub fn new() -> Result<Self> {
    let var = |x: &str| std::env::var_os(x).filter(|x| !x.is_empty()).map(PathBuf::from);
    let dir = match var("XDG_DATA_HOME") {
      Some(x) => x,
      None if cfg!(windows) => var("LOCALAPPDATA").context("cannot find LOCALAPPDATA")?,
      None => home_dir().context("cannot find the ledger without HOME")?.join(".local/share"),
    };
    let home = home_dir().map(|x| x.to_string_lossy().trim_end_matches(['/', '\\']).to_string());
    Ok(Self { path: dir.join("cctrail/ledger.db"), home: home.filter(|x| !x.is_empty()) })
  }

  /// The harnesses' sessions with the ledger brought up to date: their files read again where
  /// they changed, the sessions of those gone kept. Without the prompts as the reports read them.
  pub fn sessions(
    &self,
    harnesses: &[(Provider, Vec<PathBuf>)],
    dict: &Dict,
    reports: bool,
  ) -> Result<Vec<Session>> {
    let x = self.update(harnesses, dict)?;
    let mut groups: HashMap<Provider, (Vec<Source>, Vec<Source>)> = HashMap::new();
    x.live.into_iter().for_each(|x| groups.entry(x.provider).or_default().0.push(x));
    x.gone.into_iter().for_each(|x| groups.entry(x.provider).or_default().1.push(x));
    let mut take = |p: Provider| groups.remove(&p).map(|x| current(x.0, x.1)).unwrap_or_default();
    let jobs: Vec<_> = harnesses.iter().map(|(p, roots)| (*p, roots, take(*p))).collect();
    let sessions = jobs.into_par_iter().map(|(p, roots, x)| parser::combine(p, roots, x));
    let mut sessions = sessions.collect::<Vec<_>>().concat();
    if reports {
      crate::swear::take_prompts(&mut sessions);
    }
    Ok(sessions)
  }

  /// `cctrail export`: the ledger, brought up to date unless it is off, copied into `file`.
  /// Returns its sessions.
  pub fn export(&self, file: &Path, update: bool) -> Result<usize> {
    if update {
      self.update(&parser::harnesses(None), &Dict::user()?)?;
    }
    let conn = open(&self.path, false)?;
    match std::fs::remove_file(file) {
      Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
      _ => Ok(()),
    }
    .with_context(|| format!("failed to replace {}", file.display()))?;
    copy(&conn, file)?;
    // A file on its own, without a write-ahead log beside it.
    open(file, true)?.query_row("PRAGMA journal_mode = DELETE", [], |_| Ok(()))?;
    Ok(conn.query_row("SELECT count(*) FROM sessions", [], |x| x.get::<_, i64>(0))? as usize)
  }

  /// `cctrail import`: merges an exported ledger into this one, adding and replacing only.
  /// Returns the sessions added, replaced and kept.
  pub fn import(&self, file: &Path) -> Result<(usize, usize, usize)> {
    let theirs = load(&open(file, false)?, self.home.as_deref())?;
    let mut conn = open(&self.path, true)?;
    let (write, counts) = merge(&load(&conn, self.home.as_deref())?, theirs);
    store(&mut conn, self.home.as_deref(), &write.iter().collect::<Vec<_>>())?;
    Ok(counts)
  }

  /// Opens the ledger and brings the harnesses' files in it up to date.
  fn update(&self, harnesses: &[(Provider, Vec<PathBuf>)], dict: &Dict) -> Result<Refresh> {
    let mut conn = open(&self.path, true)?;
    let x = refresh(load(&conn, self.home.as_deref())?, harnesses, dict);
    if let Some(version) = x.older {
      backup(&conn, &self.path, version)?;
    }
    let fresh: Vec<&Source> = x.fresh.iter().map(|i| &x.live[*i]).collect();
    if !fresh.is_empty() {
      store(&mut conn, self.home.as_deref(), &fresh)
        .with_context(|| format!("failed to update {}", self.path.display()))?;
    }
    Ok(x)
  }
}

#[derive(Default)]
struct Refresh {
  /// The sources of the files here.
  live: Vec<Source>,
  /// Those read now, to store.
  fresh: Vec<usize>,
  /// The oldest parser version of the stored sources read again.
  older: Option<i64>,
  /// The sources of the files gone.
  gone: Vec<Source>,
}

/// The ledger's sources with the harnesses' files: unchanged ones (size, mtime, parser version and
/// dictionary) as stored, the others read again, keeping the stored ones when a file that had
/// events reads to none (unreadable, truncated).
fn refresh(old: Vec<Source>, harnesses: &[(Provider, Vec<PathBuf>)], dict: &Dict) -> Refresh {
  let version = (PARSER_VERSION, dict.fingerprint());
  let events = |x: &Source| x.unit.session.as_ref().map_or(0, |x| x.events.len());
  let read = |(x, old): (Source, Option<Source>)| {
    let new = x.read(Some(dict), version.1);
    match old {
      Some(old) if events(&new) == 0 && events(&old) > 0 => (old, false, None),
      old => (new, true, old.map(|x| x.version.0).filter(|x| *x < PARSER_VERSION)),
    }
  };
  let mut groups: HashMap<Provider, HashMap<PathBuf, Source>> = HashMap::new();
  for x in old {
    groups.entry(x.provider).or_default().insert(x.path.clone(), x);
  }
  let jobs: Vec<_> =
    harnesses.iter().map(|(p, roots)| (*p, roots, groups.remove(p).unwrap_or_default())).collect();
  let parts: Vec<_> = jobs
    .into_par_iter()
    .map(|(p, roots, mut old)| {
      let (mut kept, mut todo, mut seen) = (vec![], vec![], HashSet::new());
      for x in parser::find(p, roots) {
        match old.remove(&x.path) {
          _ if !seen.insert(x.path.clone()) => {}
          Some(s) if (s.size, s.mtime, s.version) == (x.size, x.mtime, version) => kept.push(s),
          s => todo.push((x, s)),
        }
      }
      let read: Vec<_> = todo.into_par_iter().map(read).collect();
      (kept, read, old.into_values().collect::<Vec<_>>())
    })
    .collect();
  let mut x = Refresh::default();
  for (kept, read, gone) in parts {
    x.live.extend(kept);
    for (source, store, older) in read {
      if store {
        x.fresh.push(x.live.len());
      }
      x.older = x.older.into_iter().chain(older).min();
      x.live.push(source);
    }
    x.gone.extend(gone);
  }
  x.gone.extend(groups.into_values().flat_map(|x| x.into_values()));
  x
}

/// A harness's sources to count: the live ones, and of the gone ones those whose session no live
/// one has (not a moved file), the last read of each.
fn current(mut live: Vec<Source>, gone: Vec<Source>) -> Vec<Source> {
  let id = |x: &Source| Some(x.unit.session.as_ref()?.id.clone());
  let here: HashSet<String> = live.iter().filter_map(id).collect();
  let mut last: HashMap<String, Source> = HashMap::new();
  for x in gone {
    let Some(key) = id(&x).filter(|x| !here.contains(x)) else { continue };
    match last.get(&key) {
      Some(at) if (at.mtime, at.size, &at.path) >= (x.mtime, x.size, &x.path) => {}
      _ => _ = last.insert(key, x),
    }
  }
  live.extend(last.into_values());
  live
}

/// The imported sources to store, matched to ours by path, else by harness and session id: one is
/// added when we have none, and replaces ours when our file is gone and was read before theirs.
/// Returns them and the counts added, replaced and kept.
fn merge(ours: &[Source], theirs: Vec<Source>) -> (Vec<Source>, (usize, usize, usize)) {
  let key = |x: &Source| Some((x.provider, x.unit.session.as_ref()?.id.clone()));
  // Ours here first, else the last read.
  let rank = |x: &Source| (x.path.exists(), x.mtime);
  let by_path: HashMap<&Path, &Source> = ours.iter().map(|x| (x.path.as_path(), x)).collect();
  let mut by_id: HashMap<_, &Source> = HashMap::new();
  for x in ours {
    let Some(k) = key(x) else { continue };
    let at = by_id.entry(k).or_insert(x);
    if rank(x) > rank(at) {
      *at = x;
    }
  }
  let (mut write, mut counts) = (vec![], (0, 0, 0));
  for x in theirs {
    let Some(k) = key(&x) else { continue };
    match by_path.get(x.path.as_path()).or_else(|| by_id.get(&k)) {
      None => counts.0 += 1,
      Some(o) if !o.path.exists() && x.mtime > o.mtime => counts.1 += 1,
      Some(_) => {
        counts.2 += 1;
        continue;
      }
    }
    write.push(x);
  }
  (write, counts)
}

// MARK: Storage

fn open(path: &Path, write: bool) -> Result<Connection> {
  let flags = match write {
    true => {
      if let Some(dir) = path.parent().filter(|x| !x.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
          .with_context(|| format!("failed to create {}", dir.display()))?;
      }
      OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
    }
    false => OpenFlags::SQLITE_OPEN_READ_ONLY,
  };
  let context = || format!("failed to open {}", path.display());
  let conn = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)
    .with_context(context)?;
  conn.busy_timeout(Duration::from_secs(30))?;
  let version: i64 =
    conn.query_row("PRAGMA user_version", [], |x| x.get(0)).with_context(context)?;
  match version {
    0 if write => conn.execute_batch(SCHEMA).with_context(context)?,
    1 => {}
    0 => bail!("{} is no cctrail ledger", path.display()),
    _ => bail!("{} is from a newer cctrail", path.display()),
  }
  Ok(conn)
}

/// A consistent copy of the database in a new file.
fn copy(conn: &Connection, to: &Path) -> Result<()> {
  let res = conn.execute("VACUUM INTO ?1", [to.to_string_lossy()]);
  res.map(|_| ()).with_context(|| format!("failed to copy the ledger to {}", to.display()))
}

/// Copies the ledger to `ledger.db.bak-<version>` before sources an older parser read are
/// replaced, once per version; keeps the last two copies.
fn backup(conn: &Connection, path: &Path, version: i64) -> Result<()> {
  let name = path.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
  let to = path.with_file_name(format!("{name}.bak-{version}"));
  if to.exists() {
    return Ok(());
  }
  copy(conn, &to)?;
  let prefix = format!("{name}.bak-");
  let dir = std::fs::read_dir(to.parent().unwrap_or(Path::new("."))).into_iter().flatten();
  let mut copies: Vec<(std::time::SystemTime, PathBuf)> = dir
    .flatten()
    .filter(|x| x.file_name().to_string_lossy().starts_with(&prefix))
    .filter_map(|x| Some((x.metadata().ok()?.modified().ok()?, x.path())))
    .collect();
  copies.sort();
  copies.iter().rev().skip(2).for_each(|x| _ = std::fs::remove_file(&x.1));
  Ok(())
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

fn unfold(home: Option<&str>, path: &str) -> String {
  match (path.strip_prefix('~'), home) {
    (Some(rest), _) if rest.starts_with('~') => rest.to_string(),
    (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
      format!("{home}{rest}")
    }
    _ => path.to_string(),
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

/// All rows of a query, each through `f`.
fn rows(conn: &Connection, sql: &str, mut f: impl FnMut(&Row) -> Result<()>) -> Result<()> {
  let mut stmt = conn.prepare(sql)?;
  let mut rows = stmt.query([])?;
  while let Some(x) = rows.next()? {
    f(x)?;
  }
  Ok(())
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

/// Replaces the stored sources of the paths of `write` with them, in one transaction.
fn store(conn: &mut Connection, home: Option<&str>, write: &[&Source]) -> Result<()> {
  let tx = conn.transaction()?;
  insert(&tx, |x| fold(home, x), write)?;
  Ok(tx.commit()?)
}

fn insert(tx: &Connection, fold: impl Fn(&str) -> String, write: &[&Source]) -> Result<()> {
  let mut strings: HashMap<String, i64> = HashMap::new();
  rows(tx, "SELECT value, id FROM strings", |x| Ok(_ = strings.insert(x.get(0)?, x.get(1)?)))?;
  let mut drop_events = tx.prepare(
    "DELETE FROM events WHERE session IN
       (SELECT s.id FROM sessions s JOIN files f ON s.file = f.id WHERE f.path = ?1)",
  )?;
  let mut drop_session =
    tx.prepare("DELETE FROM sessions WHERE file IN (SELECT id FROM files WHERE path = ?1)")?;
  let mut drop_file = tx.prepare("DELETE FROM files WHERE path = ?1")?;
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

  for x in write {
    let (harness, path) = (x.provider.name(), fold(&x.path.to_string_lossy()));
    drop_events.execute([&path])?;
    drop_session.execute([&path])?;
    drop_file.execute([&path])?;
    let (size, root, (parser, dict)) = (x.size as i64, fold(&x.root), x.version);
    let row = file.insert(params![harness, root, path, size, x.mtime, parser, dict])?;
    let Some(s) = &x.unit.session else { continue };
    let mut links = x.unit.links.clone();
    link_paths(&mut links).into_iter().for_each(|x| *x = fold(x));
    let (workdir, cwd, links) = (fold(&s.workdir), fold(&s.cwd), serde_json::to_string(&links)?);
    let row =
      session.insert(params![row, harness, s.id, s.is_main, workdir, cwd, s.remote, links])?;
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
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A home in a temp directory with copies of the fixtures as its harnesses' history, and a
  /// ledger beside it.
  struct Home {
    dir: PathBuf,
    ledger: Ledger,
    harnesses: Vec<(Provider, Vec<PathBuf>)>,
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

  impl Home {
    /// The fixtures (`tests/<set>`) under `<temp>/<name>/home`.
    fn new(name: &str, set: &str) -> Self {
      let dir = std::env::temp_dir().join(format!("cctrail-ledger-{}-{name}", std::process::id()));
      let _ = std::fs::remove_dir_all(&dir);
      let home = dir.join("home");
      copy_tree(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join(set), &home);
      let harnesses = vec![
        (Provider::Claude, vec![home.join("claude")]),
        (Provider::Codex, vec![home.join("codex")]),
        (Provider::Pi, vec![home.join("pi/sessions")]),
        (Provider::Fx, vec![home.join("fx")]),
      ];
      let home = Some(home.to_string_lossy().to_string());
      Self { ledger: Ledger { path: dir.join("data/ledger.db"), home }, dir, harnesses }
    }

    fn file(&self, path: &str) -> PathBuf {
      self.dir.join("home").join(path)
    }

    fn sessions(&self) -> String {
      let dict = Dict::load(None).unwrap();
      format!("{:#?}", self.ledger.sessions(&self.harnesses, &dict, false).unwrap())
    }

    /// The sessions as read without the ledger.
    fn parsed(&self) -> String {
      let dict = Dict::load(None).unwrap();
      let parse = |(p, roots): &(Provider, Vec<PathBuf>)| parser::parse(*p, roots, Some(&dict));
      format!("{:#?}", self.harnesses.iter().flat_map(parse).collect::<Vec<_>>())
    }

    fn sql(&self, sql: &str) {
      Connection::open(&self.ledger.path).unwrap().execute_batch(sql).unwrap();
    }
  }

  impl Drop for Home {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.dir);
    }
  }

  #[test]
  fn reads_as_a_full_parse_cold_and_warm() {
    for set in ["fixtures", "swears"] {
      let home = Home::new(&format!("full-{set}"), set);
      let parsed = home.parsed();
      assert!(parsed.contains("Prompt"));
      assert_eq!(home.sessions(), parsed, "{set} cold");
      assert_eq!(home.sessions(), parsed, "{set} warm");
      // The reports read no prompts, as without the ledger.
      let dict = Dict::load(None).unwrap();
      let reports = home.ledger.sessions(&home.harnesses, &dict, true).unwrap();
      let parse = |(p, roots): &(Provider, Vec<PathBuf>)| parser::parse(*p, roots, None);
      let parsed: Vec<_> = home.harnesses.iter().flat_map(parse).collect();
      assert_eq!(format!("{reports:?}"), format!("{parsed:?}"), "{set} reports");
    }
  }

  #[test]
  fn reads_again_only_changed_files() {
    let home = Home::new("changed", "fixtures");
    let before = home.sessions();
    // A stored number only an unchanged file keeps.
    home.sql("UPDATE events SET input = input + 1000 WHERE input IS NOT NULL");
    let tampered = home.sessions();
    assert_ne!(tampered, before);
    let path = home.file("claude/projects/testproj/s-main-1.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n{\"type\": \"bogus\"}\n");
    std::fs::write(&path, text).unwrap();
    // The main log's own events read again, its agent's and the other harnesses' as stored.
    let dict = Dict::load(None).unwrap();
    let after = home.ledger.sessions(&home.harnesses, &dict, false).unwrap();
    let input = |id: &str| {
      let x = after.iter().find(|x| x.id == id).unwrap();
      x.events.iter().filter_map(|x| x.usage).map(|x| x.input).collect::<Vec<_>>()
    };
    assert!(input("s-main-1").iter().all(|x| *x < 1000));
    assert!(!input("a-explore-1").is_empty() && input("a-explore-1").iter().all(|x| *x >= 1000));
    assert!(input("codex-main-1").iter().all(|x| *x >= 1000));
  }

  #[test]
  fn keeps_the_sessions_of_gone_files() {
    let home = Home::new("gone", "fixtures");
    let before = home.sessions();
    for x in ["claude/projects", "codex/sessions", "pi/sessions/--repo--", "fx/sessions/fx-agent-1"]
    {
      std::fs::remove_dir_all(home.file(x)).unwrap();
    }
    assert_eq!(home.sessions(), before);
    // A file that reads to nothing keeps what it had.
    let home = Home::new("empty", "fixtures");
    let before = home.sessions();
    std::fs::write(home.file("claude/projects/testproj/s-main-1.jsonl"), "").unwrap();
    assert_eq!(home.sessions(), before);
  }

  #[test]
  fn reads_again_for_a_new_parser_version_after_a_backup() {
    let home = Home::new("version", "fixtures");
    let before = home.sessions();
    home.sql("UPDATE events SET input = input + 1000 WHERE input IS NOT NULL");
    assert_ne!(home.sessions(), before);
    home.sql(&format!("UPDATE files SET parser_version = {}", PARSER_VERSION - 1));
    assert_eq!(home.sessions(), before);
    let backup = home.dir.join(format!("data/ledger.db.bak-{}", PARSER_VERSION - 1));
    let conn = open(&backup, false).unwrap();
    let old: i64 =
      conn.query_row("SELECT max(parser_version) FROM files", [], |x| x.get(0)).unwrap();
    assert_eq!(old, PARSER_VERSION - 1);
  }

  #[test]
  fn exports_and_imports_under_another_home() {
    let ours = Home::new("export", "fixtures");
    // A session in a directory under the home.
    let cwd = ours.file("Code/app");
    let line = format!(
      r#"{{"type": "user", "message": {{"content": "go"}}, "timestamp": "2026-01-02T00:00:00Z", "cwd": "{}"}}"#,
      cwd.to_string_lossy().replace('\\', "\\\\")
    );
    std::fs::write(ours.file("claude/projects/testproj/s-home-1.jsonl"), line).unwrap();
    let before = ours.sessions();
    assert!(before.contains(&*cwd.to_string_lossy()));
    let file = ours.dir.join("export.db");
    let n = ours.ledger.export(&file, false).unwrap();
    let conn = open(&file, false).unwrap();
    let paths: Vec<String> = conn
      .prepare("SELECT path FROM files UNION ALL SELECT workdir FROM sessions")
      .unwrap()
      .query_map([], |x| x.get(0))
      .unwrap()
      .map(Result::unwrap)
      .collect();
    let home = ours.ledger.home.clone().unwrap();
    assert!(paths.iter().all(|x| !x.contains(&home)), "{paths:?}");
    assert!(paths.iter().any(|x| x.starts_with("~/") || x.starts_with("~\\")));
    assert!(paths.contains(&format!("~{}Code{0}app", std::path::MAIN_SEPARATOR)));

    // Another machine without the logs: the same sessions, in its home.
    let theirs = Home::new("import", "fixtures");
    std::fs::remove_dir_all(theirs.dir.join("home")).unwrap();
    assert_eq!(theirs.ledger.import(&file).unwrap(), (n, 0, 0));
    let other = theirs.ledger.home.clone().unwrap();
    assert_eq!(theirs.sessions(), before.replace(&home, &other));
    // Again: nothing newer.
    assert_eq!(theirs.ledger.import(&file).unwrap(), (0, 0, n));
  }

  fn source(id: &str, path: &Path, mtime: i64) -> Source {
    let mut x = Source::found(Provider::Codex, Path::new("/r"), path.to_path_buf(), &[]);
    x.mtime = mtime;
    x.unit.session = Some(Session::new(id, true, "/w", Provider::Codex));
    x
  }

  #[test]
  fn merges_by_path_then_session() {
    let dir = std::env::temp_dir().join(format!("cctrail-merge-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let here = dir.join("here.jsonl");
    std::fs::write(&here, "").unwrap();
    let gone = |x: &str| dir.join(format!("gone-{x}.jsonl"));
    let ours = [
      source("a", &gone("a"), 10),
      source("b", &gone("b"), 10),
      source("c", &here, 10),
      source("d", &gone("d"), 10),
    ];
    let mut theirs = vec![
      source("a", &gone("a"), 20),     // ours gone and older: replaced
      source("b", &gone("b"), 5),      // ours newer: kept
      source("c", &gone("other"), 20), // ours here: kept
      source("d", &gone("moved"), 20), // ours gone, by session: replaced
      source("e", &gone("e"), 1),      // new: added
    ];
    theirs.push(Source::found(Provider::Codex, Path::new("/r"), gone("none"), &[]));
    let (write, counts) = merge(&ours, theirs);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(counts, (1, 2, 2));
    let ids: Vec<_> = write.iter().map(|x| x.unit.session.as_ref().unwrap().id.as_str()).collect();
    assert_eq!(ids, ["a", "d", "e"]);

    // Of the sessions gone, one a live file has counts there, the rest the last read.
    let live = vec![source("c", &here, 1)];
    let gone =
      vec![source("c", &gone("c"), 9), source("d", &gone("d1"), 1), source("d", &gone("d2"), 2)];
    let paths: Vec<_> = current(live, gone).into_iter().map(|x| x.path).collect();
    assert_eq!(paths, [here, dir.join("gone-d2.jsonl")]);
  }

  #[test]
  fn folds_paths_under_the_home() {
    let home = Some("/h/me");
    let cases = [
      ("/h/me/x", "~/x"),
      ("/h/me", "~"),
      ("/h/meow/x", "/h/meow/x"),
      ("~x", "~~x"),
      ("/other", "/other"),
      ("unknown", "unknown"),
    ];
    for (path, folded) in cases {
      assert_eq!(fold(home, path), folded);
      assert_eq!(unfold(home, folded), path);
    }
    assert_eq!(unfold(Some("/h/you"), "~/x"), "/h/you/x");
  }
}
