// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::cell::RefCell;
use std::collections::HashMap;

use jiff::civil::Date;

use crate::config::{By, Config};
use crate::metrics::{self, DateRange, Granularity, Period};
use crate::model::{Event, Provider, Session, Speed, Ts};
use crate::output::{self, workdir_label};
use crate::pricing;

/// The periods the header switches between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
  Today,
  Week,
  Month,
  Days30,
  All,
}

impl Span {
  pub const ALL: [Span; 5] = [Self::Today, Self::Week, Self::Month, Self::Days30, Self::All];

  pub fn label(self) -> &'static str {
    ["Today", "Week", "Month", "30d", "All"][self as usize]
  }

  pub fn range(self, today: Date) -> DateRange {
    match self {
      Self::Today => DateRange::of(Period::Today, today),
      Self::Week => DateRange::of(Period::ThisWeek, today),
      Self::Month => DateRange::of(Period::ThisMonth, today),
      Self::Days30 => {
        DateRange { since: today.checked_sub(jiff::Span::new().days(29)).ok(), until: Some(today) }
      }
      Self::All => DateRange::default(),
    }
  }

  /// Timeline buckets: days, weeks for all of the history.
  pub fn gran(self) -> Granularity {
    if self == Self::All { Granularity::Week } else { Granularity::Day }
  }
}

/// One line of a table: a project, a model, a day.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
  /// What a drill-down looks for: a project name, a model key, a bucket's first day.
  pub key: String,
  pub label: String,
  /// Unconfigured directories and `other` are drawn dim.
  pub dim: bool,
  /// Microseconds.
  pub active: Option<i64>,
  pub tokens: u64,
  pub cost: f64,
  pub sessions: Option<usize>,
  pub day: Option<Date>,
  /// A model without a price: its cost is unknown, not 0.
  pub unpriced: bool,
}

/// Time per project, model, speed and effort, for one span and provider.
type Times = HashMap<Part, i64>;
/// A project, a model, whether fast, an effort (`UNKNOWN` where none is logged).
type Part = (String, String, bool, String);
/// Each timeline bucket's models.
pub type Buckets = HashMap<Date, Vec<Row>>;
/// A value with the span and provider it is for.
type Kept<T> = Option<((Span, Option<Provider>), T)>;

/// The parsed history and what each session maps to.
pub struct Data {
  sessions: Vec<Session>,
  events: Vec<(usize, usize)>,
  config: Config,
  /// The project of each session and whether it is a configured one.
  names: Vec<(String, bool)>,
  pub today: Date,
  /// `model_times` of the last span and provider asked for.
  times: RefCell<Kept<Times>>,
  /// `bucket_models` of the last span and provider asked for.
  buckets: RefCell<Kept<Buckets>>,
}

impl Data {
  pub fn new(mut sessions: Vec<Session>, config: Config, today: Date) -> Self {
    crate::workdir::fold_sessions(&mut sessions);
    let events = metrics::billable_events(&sessions);
    let mut cache: HashMap<(String, String), (String, bool)> = HashMap::new();
    let names = sessions
      .iter()
      .map(|x| {
        let key = (x.cwd.clone(), x.workdir.clone());
        let mapped = || config.find(&x.cwd, &x.workdir).is_some();
        cache
          .entry(key)
          .or_insert_with(|| (config.map(&x.cwd, &x.workdir, By::Project), mapped()))
          .clone()
      })
      .collect();
    Self {
      sessions,
      events,
      config,
      names,
      today,
      times: RefCell::default(),
      buckets: RefCell::default(),
    }
  }

  fn events(&self, prov: Option<Provider>) -> Vec<(usize, usize)> {
    let keep = |si: usize| prov.is_none_or(|p| self.sessions[si].provider == p);
    self.events.iter().copied().filter(|(si, _)| keep(*si)).collect()
  }

  /// Projects as the text report shows them, `other` included, and the total.
  pub fn projects(&self, span: Span, prov: Option<Provider>, all: bool) -> (Vec<Row>, Row) {
    let range = span.range(self.today);
    let events = self.events(prov);
    let stats = metrics::stats_by(&self.sessions, &events, &range, &self.config, By::Project);
    let total = metrics::combine_stats("total", &stats);
    let small = output::into_other(&stats, total.active, By::Project, all);
    let row = |x: &metrics::Stat, dim: bool| Row {
      key: x.name.clone(),
      label: if x.mapped { x.name.clone() } else { workdir_label(&x.name) },
      dim,
      active: Some(x.active),
      tokens: x.usage.total(),
      cost: x.cost,
      sessions: Some(x.sessions),
      ..Default::default()
    };
    let mut rows: Vec<Row> =
      stats.iter().zip(&small).filter(|x| !x.1).map(|(x, _)| row(x, !x.mapped)).collect();
    let other: Vec<&metrics::Stat> =
      stats.iter().zip(&small).filter(|x| *x.1).map(|x| x.0).collect();
    if !other.is_empty() {
      let dirs = if other.len() == 1 { "dir" } else { "dirs" };
      let label = format!("other ({} {dirs})", other.len());
      rows.push(Row { key: String::new(), ..row(&metrics::combine_stats(&label, other), true) });
      rows.last_mut().unwrap().label = label;
    }
    (rows, row(&total, false))
  }

  /// Models, fast and standard together, with their time; within a project or of everything.
  pub fn models(&self, span: Span, prov: Option<Provider>, project: Option<&str>) -> Vec<Row> {
    let keep = |si: usize, _: &Event| project.is_none_or(|p| self.names[si].0 == p);
    let rows = self.usage(span, prov, keep, |s, e| model_key(s.provider, model(e)));
    let times = self.times(span, prov);
    let time = |k: &Part, row: &Row| k.1 == row.key && project.is_none_or(|x| x == k.0);
    self.timed(rows, |row| times.iter().filter(|x| time(x.0, row)).map(|x| *x.1).sum())
  }

  /// A model's use by speed (standard, fast) and by effort (as logged, `–` where it isn't).
  pub fn model_usage(&self, span: Span, prov: Option<Provider>, key: &str) -> [Vec<Row>; 2] {
    let keep = |si: usize, e: &Event| model_key(self.sessions[si].provider, model(e)) == key;
    let speed = |e: &Event| if is_fast(e) { "fast" } else { "standard" }.to_string();
    let effort = |e: &Event| e.effort.clone().unwrap_or(UNKNOWN.into());
    let times = self.times(span, prov);
    let by = |part: &dyn Fn(&Part) -> String, rows: Vec<Row>| {
      let of = |k: &Part, row: &Row| k.1 == key && part(k) == row.key;
      self.timed(rows, |row| times.iter().filter(|x| of(x.0, row)).map(|x| *x.1).sum())
    };
    let speeds = self.usage(span, prov, keep, |_, e| speed(e));
    let efforts = self.usage(span, prov, keep, |_, e| effort(e));
    let fast = |k: &Part| if k.2 { "fast" } else { "standard" }.to_string();
    [by(&fast, speeds), by(&|k: &Part| k.3.clone(), efforts)]
  }

  /// Tokens and cost of the events `keep` takes in the span, per `key`; `UNKNOWN` reads `–`.
  fn usage(
    &self,
    span: Span,
    prov: Option<Provider>,
    keep: impl Fn(usize, &Event) -> bool,
    key: impl Fn(&Session, &Event) -> String,
  ) -> Vec<Row> {
    let range = span.range(self.today);
    let mut rows: HashMap<String, Row> = HashMap::new();
    for (si, ei) in self.events(prov) {
      let (s, e) = (&self.sessions[si], &self.sessions[si].events[ei]);
      if !in_span(&range, e) || !keep(si, e) {
        continue;
      }
      let key = key(s, e);
      let row = rows.entry(key.clone()).or_insert_with(|| Row {
        label: if key == UNKNOWN { "–".into() } else { key.clone() },
        key,
        ..Default::default()
      });
      let usage = e.usage.unwrap_or_default();
      row.tokens = row.tokens.saturating_add(usage.total());
      row.cost += pricing::cost(s.provider, e.model.as_deref(), e.ts, &usage, e.speed);
      row.unpriced |= pricing::price(s.provider, e.model.as_deref(), e.ts).is_none();
    }
    rows.into_values().collect()
  }

  /// Rows with their time.
  fn timed(&self, mut rows: Vec<Row>, time: impl Fn(&Row) -> i64) -> Vec<Row> {
    for row in &mut rows {
      row.active = Some(time(row));
    }
    rows
  }

  /// Active time split within each project between its models, speeds and efforts, summing to
  /// the project's time: each activity stamp belongs to the session's last response (its first
  /// one before any), simultaneous time is divided as between projects, and each project's parts
  /// are scaled to its time. Kept for the last span and provider.
  fn times(&self, span: Span, prov: Option<Provider>) -> std::cell::Ref<'_, Times> {
    let fresh = self.times.borrow().as_ref().is_some_and(|x| x.0 == (span, prov));
    if !fresh {
      *self.times.borrow_mut() = Some(((span, prov), self.model_times(span, prov)));
    }
    std::cell::Ref::map(self.times.borrow(), |x| &x.as_ref().unwrap().1)
  }

  fn model_times(&self, span: Span, prov: Option<Provider>) -> Times {
    let range = span.range(self.today);
    let (projects, _) = self.projects(span, prov, true);
    let totals: HashMap<String, i64> =
      projects.into_iter().map(|x| (x.key, x.active.unwrap_or(0) / MICROS)).collect();
    let mut stamps: HashMap<String, Vec<Ts>> = HashMap::new();
    for (si, s) in self.sessions.iter().enumerate() {
      if prov.is_some_and(|p| s.provider != p) {
        continue;
      }
      let part = |e: &Event| {
        let effort = e.effort.as_deref().unwrap_or(UNKNOWN);
        e.model
          .as_ref()
          .map(|_| format!("{}\0{}\0{effort}", model_key(s.provider, model(e)), is_fast(e)))
      };
      let Some(mut last) = s.events.iter().find_map(part) else { continue };
      let project = &self.names[si].0;
      for e in &s.events {
        last = part(e).unwrap_or(last);
        if let Some(ts) = e.ts.filter(|x| !e.failed && (!range.bounded() || range.contains(*x))) {
          stamps.entry(format!("{project}\0{last}")).or_default().push(ts);
        }
      }
    }
    let by: HashMap<&str, Vec<Ts>> = stamps.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    let mut per: HashMap<&str, Vec<(String, i64)>> = HashMap::new();
    for (k, v) in metrics::active_by_name(&by, &range) {
      let (project, part) = k.split_once('\0').unwrap();
      per.entry(project).or_default().push((part.to_string(), v));
    }
    let mut out = Times::new();
    for (project, parts) in per {
      let total = totals.get(project).copied().unwrap_or(0);
      for (part, v) in scale(parts, total) {
        let mut x = part.splitn(3, '\0');
        let (model, fast, effort) = (x.next().unwrap(), x.next().unwrap(), x.next().unwrap());
        let key = (project.to_string(), model.to_string(), fast == "true", effort.to_string());
        out.insert(key, v * MICROS);
      }
    }
    out
  }

  /// Time, tokens and cost of a project, or of everything, per bucket of the span, oldest first;
  /// a bounded span lists its empty days too.
  pub fn timeline(&self, span: Span, prov: Option<Provider>, project: Option<&str>) -> Vec<Row> {
    let range = span.range(self.today);
    let gran = span.gran();
    let events: Vec<_> = self
      .events(prov)
      .into_iter()
      .filter(|(si, _)| project.is_none_or(|p| self.names[*si].0 == p))
      .collect();
    let buckets = metrics::usage_by_period(&self.sessions, &events, gran, &range);
    let row = |day: Date| Row {
      key: day.to_string(),
      label: label(day, gran),
      day: Some(day),
      active: Some(0),
      ..Default::default()
    };
    let mut rows: Vec<Row> = match (range.since, range.until.map(|x| x.min(self.today))) {
      (Some(from), Some(to)) => {
        from.series(jiff::Span::new().days(1)).take_while(|x| *x <= to).map(row).collect()
      }
      _ => vec![],
    };
    // Sessions per bucket: those with usage in it.
    let mut sessions: HashMap<Date, std::collections::HashSet<(Provider, &str)>> = HashMap::new();
    for &(si, ei) in &events {
      if let Some(ts) = self.sessions[si].events[ei].ts.filter(|x| range.contains(*x)) {
        let day = gran.bucket(metrics::local_date(ts));
        sessions.entry(day).or_default().insert(self.sessions[si].root());
      }
    }
    for (day, x) in buckets {
      let i = match rows.iter().position(|r| r.day == Some(day)) {
        Some(i) => i,
        None => {
          rows.push(row(day));
          rows.len() - 1
        }
      };
      rows[i].active = Some(x.active);
      rows[i].tokens = x.usage.total();
      rows[i].cost = x.cost;
      rows[i].sessions = Some(sessions.get(&day).map_or(0, |x| x.len()));
    }
    for row in rows.iter_mut().filter(|x| x.sessions.is_none()) {
      row.sessions = Some(0);
    }
    rows.sort_by_key(|x| x.day);
    rows
  }

  /// Each timeline bucket's models with their time, tokens and cost; the time as in `times`, each
  /// bucket's parts scaled to its time. Kept for the last span and provider.
  pub fn bucket_models(&self, span: Span, prov: Option<Provider>) -> std::cell::Ref<'_, Buckets> {
    let fresh = self.buckets.borrow().as_ref().is_some_and(|x| x.0 == (span, prov));
    if !fresh {
      *self.buckets.borrow_mut() = Some(((span, prov), self.models_by_bucket(span, prov)));
    }
    std::cell::Ref::map(self.buckets.borrow(), |x| &x.as_ref().unwrap().1)
  }

  fn models_by_bucket(&self, span: Span, prov: Option<Provider>) -> Buckets {
    let range = span.range(self.today);
    let gran = span.gran();
    let bucket = |ts: Ts| gran.bucket(metrics::local_date(ts));
    let mut out: HashMap<Date, HashMap<String, Row>> = HashMap::new();
    for (si, ei) in self.events(prov) {
      let (s, e) = (&self.sessions[si], &self.sessions[si].events[ei]);
      let Some(ts) = e.ts.filter(|x| range.contains(*x)) else { continue };
      let key = model_key(s.provider, model(e));
      let row = out.entry(bucket(ts)).or_default().entry(key.clone()).or_insert_with(|| Row {
        label: key.clone(),
        key,
        ..Default::default()
      });
      let usage = e.usage.unwrap_or_default();
      row.tokens = row.tokens.saturating_add(usage.total());
      row.cost += pricing::cost(s.provider, e.model.as_deref(), e.ts, &usage, e.speed);
    }
    // Activity stamps to the model of the session's last response, as in `model_times`.
    let mut stamps: HashMap<String, Vec<Ts>> = HashMap::new();
    for s in self.sessions.iter().filter(|s| prov.is_none_or(|p| s.provider == p)) {
      let key = |e: &Event| e.model.as_ref().map(|_| model_key(s.provider, model(e)));
      let Some(mut last) = s.events.iter().find_map(key) else { continue };
      for e in &s.events {
        last = key(e).unwrap_or(last);
        if let Some(ts) = e.ts.filter(|x| !e.failed && range.contains(*x)) {
          stamps.entry(format!("{}\0{last}", bucket(ts))).or_default().push(ts);
        }
      }
    }
    let by: HashMap<&str, Vec<Ts>> = stamps.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    let mut per: HashMap<Date, Vec<(String, i64)>> = HashMap::new();
    for (k, v) in metrics::active_by_name(&by, &range) {
      let (day, model) = k.split_once('\0').unwrap();
      per.entry(day.parse().unwrap()).or_default().push((model.to_string(), v));
    }
    let totals: HashMap<Date, i64> = self
      .timeline(span, prov, None)
      .iter()
      .filter_map(|x| Some((x.day?, x.active.unwrap_or(0) / MICROS)))
      .collect();
    for (day, parts) in per {
      for (model, v) in scale(parts, totals.get(&day).copied().unwrap_or(0)) {
        let row = out.entry(day).or_default().entry(model.clone()).or_insert_with(|| Row {
          label: model.clone(),
          key: model,
          ..Default::default()
        });
        row.active = Some(v * MICROS);
      }
    }
    out.into_iter().map(|(day, rows)| (day, rows.into_values().collect())).collect()
  }

  /// The projects of one timeline bucket.
  pub fn bucket_projects(&self, span: Span, prov: Option<Provider>, day: Date) -> Vec<Row> {
    let range = span.range(self.today);
    let gran = span.gran();
    let (from, to) = gran.bounds(day, &range);
    let range = DateRange { since: Some(from), until: Some(to) };
    let events = self.events(prov);
    let stats = metrics::stats_by(&self.sessions, &events, &range, &self.config, By::Project);
    stats
      .iter()
      .map(|x| Row {
        key: x.name.clone(),
        label: if x.mapped { x.name.clone() } else { workdir_label(&x.name) },
        dim: !x.mapped,
        active: Some(x.active),
        tokens: x.usage.total(),
        cost: x.cost,
        sessions: Some(x.sessions),
        ..Default::default()
      })
      .collect()
  }

  /// Tokens on models without a price in the span, as the text report lists them.
  pub fn unpriced(&self, span: Span, prov: Option<Provider>) -> Vec<(String, u64)> {
    metrics::unpriced(&self.sessions, &self.events(prov), &span.range(self.today))
  }

  /// The logged directories a project folds together.
  pub fn paths(&self, project: &str) -> Vec<String> {
    let mut paths: Vec<String> = self
      .sessions
      .iter()
      .zip(&self.names)
      .filter(|(_, n)| n.0 == project)
      .map(|(s, _)| workdir_label(&s.cwd))
      .collect();
    paths.sort();
    paths.dedup();
    paths
  }
}

/// Seconds per name scaled to sum to `total`; the largest remainders get the rest.
fn scale(parts: Vec<(String, i64)>, total: i64) -> HashMap<String, i64> {
  let sum: i64 = parts.iter().map(|x| x.1).sum();
  if sum == 0 {
    return parts.into_iter().map(|(k, _)| (k, 0)).collect();
  }
  let mut out: Vec<(String, i64, i64)> = parts
    .into_iter()
    .map(|(k, v)| {
      let x = v as i128 * total as i128;
      (k, (x / sum as i128) as i64, (x % sum as i128) as i64)
    })
    .collect();
  let rest = total - out.iter().map(|x| x.1).sum::<i64>();
  out.sort_by(|a, b| (b.2, &a.0).cmp(&(a.2, &b.0)));
  for x in out.iter_mut().take(rest.max(0) as usize) {
    x.1 += 1;
  }
  out.into_iter().map(|(k, v, _)| (k, v)).collect()
}

const MICROS: i64 = 1_000_000;

/// The effort of turns that log none; it reads `–`.
const UNKNOWN: &str = "unknown";

/// An event counts in a span as `stats_by` counts it: by its day, and always for all time, even
/// without a timestamp.
fn in_span(range: &DateRange, e: &Event) -> bool {
  !range.bounded() || e.ts.is_some_and(|x| range.contains(x))
}

fn is_fast(e: &Event) -> bool {
  e.speed == Some(Speed::Fast)
}

fn model(e: &Event) -> &str {
  e.model.as_deref().unwrap_or("unknown")
}

pub fn model_key(provider: Provider, model: &str) -> String {
  format!("{}/{model}", provider.name())
}

fn label(day: Date, gran: Granularity) -> String {
  match gran {
    Granularity::Day => format!("{day} {}", &format!("{:?}", day.weekday())[..2]),
    _ => day.to_string(),
  }
}

#[cfg(test)]
pub mod tests {
  use std::path::{Path, PathBuf};

  use super::*;

  /// The fixtures' `/repo`, absolute on this platform: Windows needs a drive.
  pub const REPO: &str = if cfg!(windows) { "C:/repo" } else { "/repo" };

  fn fixtures(dir: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(dir)
  }

  fn sessions() -> Vec<Session> {
    let mut sessions = crate::parser::claude::parse(&[fixtures("claude/projects")]);
    sessions.extend(crate::parser::codex::parse(&[fixtures("codex")]));
    for x in &mut sessions {
      if let Some(rest) = x.workdir.strip_prefix("/repo") {
        x.workdir = format!("{REPO}{rest}");
      }
    }
    sessions
  }

  fn data_of(sessions: Vec<Session>) -> Data {
    let config = Config::parse(&format!("[projects]\n\"oss/demo\" = [{REPO:?}]\n"), None).unwrap();
    Data::new(sessions, config, jiff::civil::date(2026, 1, 10))
  }

  pub fn data() -> Data {
    data_of(sessions())
  }

  /// Projects of every size, a model without a price, one used fast some days, efforts logged
  /// for some turns, and two unconfigured dirs small enough to fold into `other` in the text
  /// report, over the three days before 2026-01-10.
  pub fn many() -> Data {
    let long = "/w/a-project-with-a-name-too-long-for-a-narrow-terminal";
    let dirs = [("/w/api", 40), ("/w/web", 20), (long, 10), ("/tmp/a", 1), ("/tmp/b", 1)];
    let models = [
      (Provider::Claude, "claude-sonnet-5"),
      (Provider::Claude, "claude-unknown-1"),
      (Provider::Codex, "gpt-5.4"),
    ];
    let tz = jiff::tz::TimeZone::system();
    let mut sessions = vec![];
    for d in 1..=3 {
      let day = jiff::civil::date(2026, 1, 6 + d);
      for (dir, weight) in dirs {
        for (i, (prov, model)) in models.iter().enumerate() {
          let mut s = Session::new(&format!("{dir}-{d}-{i}"), true, dir, *prov);
          let start = day.at(9 + 3 * i as i8, 0, 0, 0).to_zoned(tz.clone()).unwrap();
          for k in 0..weight * (i as i64 + d as i64) {
            let input = 1000 * weight as u64 * (i as u64 + 1);
            s.events.push(Event {
              ts: Some(start.timestamp().as_microsecond() + k * 60_000_000),
              usage: Some(crate::model::Usage { input, ..Default::default() }),
              model: Some(model.to_string()),
              speed: (i == 2 && d == 2).then_some(Speed::Fast),
              effort: (i != 1).then(|| ["low", "high"][d as usize % 2].to_string()),
              ..Default::default()
            });
          }
          sessions.push(s);
        }
      }
    }
    let config = format!(
      "[projects]\n\"w/api\" = [\"/w/api\"]\n\"w/web\" = [\"/w/web\"]\n\"w/{}\" = [{long:?}]\n",
      &long[3..]
    );
    Data::new(sessions, Config::parse(&config, None).unwrap(), jiff::civil::date(2026, 1, 10))
  }

  #[test]
  fn totals_match_the_report() {
    let data = data();
    let (rows, total) = data.projects(Span::All, None, false);
    assert_eq!(rows.iter().map(|x| x.tokens).sum::<u64>(), total.tokens);
    assert_eq!(rows[0].label, "oss/demo");

    let models: u64 = data.models(Span::All, None, None).iter().map(|x| x.tokens).sum();
    assert_eq!(models, total.tokens);
    let days: u64 = data.timeline(Span::All, None, None).iter().map(|x| x.tokens).sum();
    assert_eq!(days, total.tokens);

    let (_, claude) = data.projects(Span::All, Some(Provider::Claude), false);
    let (_, codex) = data.projects(Span::All, Some(Provider::Codex), false);
    assert_eq!(claude.tokens + codex.tokens, total.tokens);
  }

  #[test]
  fn drills_into_projects_models_and_days() {
    let data = data();
    let models = data.models(Span::All, None, Some("oss/demo"));
    assert!(models.iter().any(|x| x.key == "claude/claude-sonnet-5"));
    let [speeds, _] = data.model_usage(Span::All, None, "claude/claude-sonnet-5");
    assert_eq!(speeds.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["standard"]);

    let days = data.timeline(Span::All, None, Some("oss/demo"));
    let busy = days.iter().find(|x| x.tokens > 0).unwrap();
    let projects = data.bucket_projects(Span::All, None, busy.day.unwrap());
    assert!(projects.iter().any(|x| x.key == "oss/demo"));
    assert!(data.paths("oss/demo").contains(&REPO.to_string()));
  }

  #[test]
  fn counts_undated_usage_for_all_time() {
    let mut sessions = sessions();
    let model = Some("claude-sonnet-5".to_string());
    let usage = Some(crate::model::Usage { input: 123, ..Default::default() });
    sessions[0].events.push(Event { usage, model, ..Default::default() });
    let data = data_of(sessions);
    let (_, total) = data.projects(Span::All, None, false);
    let models = data.models(Span::All, None, None);
    assert_eq!(models.iter().map(|x| x.tokens).sum::<u64>(), total.tokens);
    let model = models.iter().find(|x| x.key == "claude/claude-sonnet-5").unwrap();
    for part in data.model_usage(Span::All, None, &model.key) {
      assert_eq!(part.iter().map(|x| x.tokens).sum::<u64>(), model.tokens);
    }
  }

  #[test]
  fn bounded_spans_list_empty_days() {
    let days = data().timeline(Span::Days30, None, None);
    assert_eq!(days.len(), 30);
    assert!(days.windows(2).all(|x| x[0].day < x[1].day));
  }

  #[test]
  fn model_times_add_up_everywhere() {
    let data = many();
    let (projects, total) = data.projects(Span::All, None, false);
    assert!(projects.iter().any(|x| x.label == "other (2 dirs)"));
    let models = data.models(Span::All, None, None);
    let time = |rows: &[Row]| rows.iter().map(|x| x.active.unwrap()).sum::<i64>();
    assert_eq!(time(&models), total.active.unwrap());
    assert!(models.iter().any(|x| x.unpriced) && models.iter().any(|x| !x.unpriced));
    for p in projects.iter().filter(|x| !x.key.is_empty()) {
      assert_eq!(time(&data.models(Span::All, None, Some(&p.key))), p.active.unwrap(), "{}", p.key);
    }
    // One row per model, fast and standard together; its speeds and efforts add up to it.
    let gpt = models.iter().filter(|x| x.key.starts_with("codex/gpt-5.4")).count();
    assert_eq!(gpt, 1);
    for m in &models {
      for part in data.model_usage(Span::All, None, &m.key) {
        assert_eq!(time(&part), m.active.unwrap(), "{}", m.key);
        let cost: f64 = part.iter().map(|x| x.cost).sum();
        assert!((cost - m.cost).abs() < 1e-9, "{}", m.key);
      }
    }
    let [speeds, efforts] = data.model_usage(Span::All, None, "codex/gpt-5.4");
    let mut keys: Vec<_> = speeds.iter().map(|x| x.key.as_str()).collect();
    keys.sort();
    assert_eq!(keys, ["fast", "standard"]);
    let mut keys: Vec<_> = efforts.iter().map(|x| x.label.as_str()).collect();
    keys.sort();
    assert_eq!(keys, ["high", "low"]);
    let [_, efforts] = data.model_usage(Span::All, None, "claude/claude-unknown-1");
    let unknown: Vec<_> = efforts.iter().map(|x| (x.key.as_str(), x.label.as_str())).collect();
    assert_eq!(unknown, [("unknown", "–")]);
  }
}
