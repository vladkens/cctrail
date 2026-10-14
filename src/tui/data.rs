// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

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
  /// Microseconds; none where time can't be split (models).
  pub active: Option<i64>,
  pub tokens: u64,
  pub cost: f64,
  pub sessions: Option<usize>,
  pub day: Option<Date>,
}

/// The parsed history and what each session maps to.
pub struct Data {
  sessions: Vec<Session>,
  events: Vec<(usize, usize)>,
  config: Config,
  /// The project of each session and whether it is a configured one.
  names: Vec<(String, bool)>,
  pub today: Date,
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
    Self { sessions, events, config, names, today }
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
      day: None,
    };
    let mut rows: Vec<Row> =
      stats.iter().zip(&small).filter(|x| !x.1).map(|(x, _)| row(x, !x.mapped)).collect();
    let other: Vec<&metrics::Stat> =
      stats.iter().zip(&small).filter(|x| *x.1).map(|x| x.0).collect();
    if !other.is_empty() {
      let label = format!("other ({} dirs)", other.len());
      rows.push(Row { key: String::new(), ..row(&metrics::combine_stats(&label, other), true) });
      rows.last_mut().unwrap().label = label;
    }
    (rows, row(&total, false))
  }

  /// Models by tokens and cost; within a project also by time (see `model_times`).
  pub fn models(&self, span: Span, prov: Option<Provider>, project: Option<&str>) -> Vec<Row> {
    let range = span.range(self.today);
    let mut rows: HashMap<String, Row> = HashMap::new();
    for (si, ei) in self.events(prov) {
      let (s, e) = (&self.sessions[si], &self.sessions[si].events[ei]);
      if !in_span(&range, e) || project.is_some_and(|p| self.names[si].0 != p) {
        continue;
      }
      let key = model_key(s.provider, e.model.as_deref().unwrap_or("unknown"), is_fast(e));
      let row = rows.entry(key.clone()).or_insert_with(|| Row {
        label: key.clone(),
        key,
        ..Default::default()
      });
      let usage = e.usage.unwrap_or_default();
      row.tokens = row.tokens.saturating_add(usage.total());
      row.cost += pricing::cost(s.provider, e.model.as_deref(), e.ts, &usage, e.speed);
    }
    if let Some(project) = project {
      let times = self.model_times(span, prov, project);
      for row in rows.values_mut() {
        row.active = Some(times.get(&row.key).copied().unwrap_or(0));
      }
    }
    rows.into_values().collect()
  }

  /// A project's active time split between its models, summing to the project's time: each
  /// activity stamp belongs to the model of the session's last response (its first one before
  /// any), simultaneous time is divided as between projects, and the parts are scaled to the
  /// project's share of the time.
  fn model_times(&self, span: Span, prov: Option<Provider>, project: &str) -> HashMap<String, i64> {
    let range = span.range(self.today);
    let total = self.projects(span, prov, true).0.into_iter().find(|x| x.key == project);
    let total = total.and_then(|x| x.active).unwrap_or(0) / MICROS;
    let mut stamps: HashMap<String, Vec<Ts>> = HashMap::new();
    for (si, s) in self.sessions.iter().enumerate() {
      if self.names[si].0 != project || prov.is_some_and(|p| s.provider != p) {
        continue;
      }
      let key = |e: &Event| e.model.as_deref().map(|m| model_key(s.provider, m, is_fast(e)));
      let Some(mut model) = s.events.iter().find_map(key) else { continue };
      for e in &s.events {
        model = key(e).unwrap_or(model);
        if let Some(ts) = e.ts.filter(|x| !e.failed && (!range.bounded() || range.contains(*x))) {
          stamps.entry(model.clone()).or_default().push(ts);
        }
      }
    }
    let by: HashMap<&str, Vec<Ts>> = stamps.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    let parts = metrics::active_by_name(&by, &range);
    scale(parts.into_iter().map(|(k, v)| (k.to_string(), v)).collect(), total)
      .into_iter()
      .map(|(k, v)| (k, v * MICROS))
      .collect()
  }

  /// The projects that used a model, by its tokens and cost there.
  pub fn model_projects(&self, span: Span, prov: Option<Provider>, model: &str) -> Vec<Row> {
    let range = span.range(self.today);
    let mut rows: HashMap<&str, Row> = HashMap::new();
    for (si, ei) in self.events(prov) {
      let (s, e) = (&self.sessions[si], &self.sessions[si].events[ei]);
      if !in_span(&range, e)
        || model_key(s.provider, e.model.as_deref().unwrap_or("unknown"), is_fast(e)) != model
      {
        continue;
      }
      let (name, mapped) = &self.names[si];
      let row = rows.entry(name).or_insert_with(|| Row {
        key: name.clone(),
        label: if *mapped { name.clone() } else { workdir_label(name) },
        dim: !mapped,
        ..Default::default()
      });
      let usage = e.usage.unwrap_or_default();
      row.tokens = row.tokens.saturating_add(usage.total());
      row.cost += pricing::cost(s.provider, e.model.as_deref(), e.ts, &usage, e.speed);
    }
    rows.into_values().collect()
  }

  /// The metric of a project, or of everything, per bucket of the span, oldest first; a bounded
  /// span lists its empty days too.
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
    }
    rows.sort_by_key(|x| x.day);
    rows
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
        day: None,
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

/// An event counts in a span as `stats_by` counts it: by its day, and always for all time, even
/// without a timestamp.
fn in_span(range: &DateRange, e: &Event) -> bool {
  !range.bounded() || e.ts.is_some_and(|x| range.contains(x))
}

fn is_fast(e: &Event) -> bool {
  e.speed == Some(Speed::Fast)
}

pub fn model_key(provider: Provider, model: &str, fast: bool) -> String {
  format!("{}/{model}{}", provider.name(), if fast { " (fast)" } else { "" })
}

fn label(day: Date, gran: Granularity) -> String {
  match gran {
    Granularity::Day => format!("{day} {}", &format!("{:?}", day.weekday())[..2]),
    _ => format!("{day} week"),
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
    let used = data.model_projects(Span::All, None, "claude/claude-sonnet-5");
    assert_eq!(used.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["oss/demo"]);

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
    let used: u64 = data.model_projects(Span::All, None, &model.key).iter().map(|x| x.tokens).sum();
    assert_eq!(used, model.tokens);
  }

  #[test]
  fn bounded_spans_list_empty_days() {
    let days = data().timeline(Span::Days30, None, None);
    assert_eq!(days.len(), 30);
    assert!(days.windows(2).all(|x| x[0].day < x[1].day));
  }
}
