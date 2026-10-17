// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::cell::RefCell;
use std::collections::HashMap;

use jiff::civil::Date;

use crate::config::{By, Config};
use crate::metrics::{self, DateRange, Granularity, Period};
use crate::model::{Event, Prompt, Provider, Session, Speed, Ts};
use crate::output::{self, workdir_label};
use crate::pricing;
use crate::swear::MIN_WORDS;

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
  /// The swears tab's: words typed to a model, the swear words among them, the prompts and those
  /// with swearing, and the score.
  pub words: u64,
  pub swears: u64,
  pub prompts: u64,
  pub swearing: u64,
  pub score: Option<f64>,
}

impl Row {
  /// Swear words per 1,000 words typed, from enough words.
  pub fn per_1k(&self) -> Option<f64> {
    (self.words >= MIN_WORDS).then(|| self.swears as f64 * 1000.0 / self.words as f64)
  }

  /// The percent of the prompts with swearing, from enough words.
  pub fn msgs(&self) -> Option<f64> {
    (self.words >= MIN_WORDS).then(|| self.swearing as f64 * 100.0 / self.prompts.max(1) as f64)
  }
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
  /// The main sessions' prompts, each once.
  prompts: Vec<(Provider, Prompt)>,
  events: Vec<(usize, usize)>,
  config: Config,
  /// The project of each session and whether it is a configured one.
  names: Vec<(String, bool)>,
  pub today: Date,
  /// `model_times` of the last span and provider asked for.
  times: RefCell<Kept<Times>>,
  /// `bucket_models` of the last span and provider asked for.
  buckets: RefCell<Kept<Buckets>>,
  /// `model_rank`, once worked out.
  rank: RefCell<Option<Vec<String>>>,
}

impl Data {
  pub fn new(mut sessions: Vec<Session>, config: Config, today: Date) -> Self {
    let prompts = crate::swear::take_prompts(&mut sessions);
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
      prompts,
      events,
      config,
      names,
      today,
      times: RefCell::default(),
      buckets: RefCell::default(),
      rank: RefCell::default(),
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

  /// The words typed to each model in the span and the swear words among them, their rates and
  /// score, and the total, scored at the overall rate.
  pub fn swears(&self, span: Span, prov: Option<Provider>) -> (Vec<Row>, Row) {
    let range = span.range(self.today);
    let mut rows: HashMap<String, Row> = HashMap::new();
    let mut total = Row { key: "total".into(), label: "total".into(), ..Default::default() };
    for (_, x) in self.prompts.iter().filter(|x| prov.is_none_or(|p| p == x.0)) {
      if !range.contains(x.ts) {
        continue;
      }
      let key = x.model.as_deref().unwrap_or(UNKNOWN);
      for row in [rows.entry(key.to_string()).or_insert_with_key(|k| row_of(k)), &mut total] {
        row.words += u64::from(x.words);
        row.swears += u64::from(x.swears);
        row.prompts += 1;
        row.swearing += u64::from(x.swears > 0);
      }
    }
    let overall = total.swears as f64 / total.words.max(1) as f64;
    let mut rows: Vec<Row> = rows.into_values().collect();
    for x in &mut rows {
      x.score = x.per_1k().map(|_| crate::swear::score(x.swears, x.words, overall));
    }
    total.score = total.per_1k();
    (rows, total)
  }

  /// Models, fast and standard together, with their time; within a project or of everything.
  pub fn models(&self, span: Span, prov: Option<Provider>, project: Option<&str>) -> Vec<Row> {
    let keep = |si: usize, _: &Event| project.is_none_or(|p| self.names[si].0 == p);
    let rows = self.usage(span, prov, keep, |s, e| model_key(s.provider, model(e)));
    let times = self.times(span, prov);
    let mine = times.iter().filter(|x| project.is_none_or(|p| p == x.0.0));
    timed(rows, mine.map(|(k, v)| (k.1.clone(), *v)))
  }

  /// A model's use by speed (standard, fast) and by effort (as logged, `–` where it isn't).
  pub fn model_usage(&self, span: Span, prov: Option<Provider>, key: &str) -> [Vec<Row>; 2] {
    let keep = |si: usize, e: &Event| model_key(self.sessions[si].provider, model(e)) == key;
    let speed = |e: &Event| if is_fast(e) { "fast" } else { "standard" }.to_string();
    let effort = |e: &Event| e.effort.clone().unwrap_or(UNKNOWN.into());
    let times = self.times(span, prov);
    let by = |part: &dyn Fn(&Part) -> String, rows: Vec<Row>| {
      let mine = times.iter().filter(|x| x.0.1 == key);
      timed(rows, mine.map(|(k, v)| (part(k), *v)))
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
      let row = rows.entry(key(s, e)).or_insert_with_key(|key| row_of(key));
      let usage = e.usage.unwrap_or_default();
      row.tokens = row.tokens.saturating_add(usage.total());
      row.cost += pricing::cost(s.provider, e);
      row.unpriced |= !pricing::priced(s.provider, e);
    }
    rows.into_values().collect()
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
    // The sessions the projects count: those with usage in the span.
    let billed: std::collections::HashSet<(Provider, &str)> = self
      .events(prov)
      .into_iter()
      .filter(|(si, ei)| in_span(&range, &self.sessions[*si].events[*ei]))
      .map(|(si, _)| self.sessions[si].root())
      .collect();
    let mut stamps: HashMap<String, Vec<Ts>> = HashMap::new();
    for (si, s) in self.sessions.iter().enumerate() {
      if prov.is_some_and(|p| s.provider != p) || !billed.contains(&s.root()) {
        continue;
      }
      let part = |e: &Event| {
        let effort = e.effort.as_deref().unwrap_or(UNKNOWN);
        // A failed response answered nothing, cache warming ran unattended: the time stays with
        // the model before it.
        e.model
          .as_ref()
          .filter(|_| e.active())
          .map(|_| format!("{}\0{}\0{effort}", model_key(s.provider, model(e)), is_fast(e)))
      };
      // A session that names no model is the unknown one, as its usage is.
      let unknown = || format!("{}\0false\0{UNKNOWN}", model_key(s.provider, "unknown"));
      let mut last = s.events.iter().find_map(part).unwrap_or_else(unknown);
      let project = &self.names[si].0;
      for e in &s.events {
        last = part(e).unwrap_or(last);
        if let Some(ts) = e.ts.filter(|x| e.active() && (!range.bounded() || range.contains(*x))) {
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
    // Sessions per bucket: the main ones with usage in it, as the projects count them.
    let mains = self.mains();
    let mut sessions: HashMap<Date, std::collections::HashSet<(Provider, &str)>> = HashMap::new();
    for &(si, ei) in &events {
      let root = self.sessions[si].root();
      let ts = self.sessions[si].events[ei].ts.filter(|x| range.contains(*x));
      if let Some(ts) = ts.filter(|_| mains.contains(&root)) {
        sessions.entry(gran.bucket(metrics::local_date(ts))).or_default().insert(root);
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

  /// The models by their tokens over all of the history and both providers, most first: an order
  /// that doesn't change with the period, the provider or what is shown, for their colors.
  pub fn model_rank(&self) -> std::cell::Ref<'_, Vec<String>> {
    if self.rank.borrow().is_none() {
      let mut tokens: HashMap<String, u64> = HashMap::new();
      for (si, ei) in self.events(None) {
        let (s, e) = (&self.sessions[si], &self.sessions[si].events[ei]);
        let t = tokens.entry(model_key(s.provider, model(e))).or_default();
        *t = t.saturating_add(e.usage.unwrap_or_default().total());
      }
      let mut rank: Vec<(String, u64)> = tokens.into_iter().collect();
      rank.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
      *self.rank.borrow_mut() = Some(rank.into_iter().map(|x| x.0).collect());
    }
    std::cell::Ref::map(self.rank.borrow(), |x| x.as_ref().unwrap())
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
      let row = out.entry(bucket(ts)).or_default().entry(key).or_insert_with_key(|k| row_of(k));
      let usage = e.usage.unwrap_or_default();
      row.tokens = row.tokens.saturating_add(usage.total());
      row.cost += pricing::cost(s.provider, e);
    }
    // Activity stamps, as the timeline counts them, to the model of the session's last response,
    // as in `model_times`; the windows cut at midnight, so a day gets the part of a window from
    // the day before.
    let counted = self.counted(span, prov);
    let mut stamps: HashMap<String, Vec<Ts>> = HashMap::new();
    for s in self.sessions.iter().filter(|s| prov.is_none_or(|p| s.provider == p)) {
      let key = |e: &Event| {
        e.model.as_ref().filter(|_| e.active()).map(|_| model_key(s.provider, model(e)))
      };
      let mut last = s.events.iter().find_map(key).unwrap_or(model_key(s.provider, "unknown"));
      for e in &s.events {
        last = key(e).unwrap_or(last);
        if let Some(ts) = e.ts.filter(|x| e.active() && counted(s, *x)) {
          stamps.entry(last.clone()).or_default().push(ts);
        }
      }
    }
    let mut per: HashMap<Date, HashMap<String, i64>> = HashMap::new();
    for ((model, day), micros) in by_name_and_day(&stamps) {
      if range.contains_day(day) {
        *per.entry(gran.bucket(day)).or_default().entry(model).or_default() += micros;
      }
    }
    let per = per
      .into_iter()
      .map(|(day, x)| (day, x.into_iter().map(|(k, v)| (k, v / MICROS)).collect::<Vec<_>>()));
    let totals: HashMap<Date, i64> = self
      .timeline(span, prov, None)
      .iter()
      .filter_map(|x| Some((x.day?, x.active.unwrap_or(0) / MICROS)))
      .collect();
    for (day, parts) in per {
      for (model, v) in scale(parts, totals.get(&day).copied().unwrap_or(0)) {
        out.entry(day).or_default().entry(model).or_insert_with_key(|k| row_of(k)).active =
          Some(v * MICROS);
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
    // The time as the timeline counts it, a window from the day before reaching into it included,
    // then cut to the bucket's days.
    let counted = self.counted(span, prov);
    let mut stamps: HashMap<&str, Vec<Ts>> = HashMap::new();
    for (si, s) in self.sessions.iter().enumerate() {
      if prov.is_none_or(|p| s.provider == p) {
        let mine = s.events.iter().filter(|e| e.active()).filter_map(|e| e.ts);
        stamps
          .entry(self.names[si].0.as_str())
          .or_default()
          .extend(mine.filter(|x| counted(s, *x)));
      }
    }
    let active = metrics::active_by_name(&stamps, &range);
    let mut rows: Vec<Row> = stats
      .iter()
      .map(|x| Row {
        key: x.name.clone(),
        label: if x.mapped { x.name.clone() } else { workdir_label(&x.name) },
        dim: !x.mapped,
        active: Some(active.get(x.name.as_str()).copied().unwrap_or(0) * MICROS),
        tokens: x.usage.total(),
        cost: x.cost,
        sessions: Some(x.sessions),
        ..Default::default()
      })
      .collect();
    for (name, seconds) in active.into_iter().filter(|x| x.1 > 0) {
      if !rows.iter().any(|x| x.key == name) {
        let mapped = self.names.iter().any(|x| x.0 == name && x.1);
        let label = if mapped { name.to_string() } else { workdir_label(name) };
        let (key, active) = (name.to_string(), Some(seconds * MICROS));
        rows.push(Row {
          key,
          label,
          dim: !mapped,
          active,
          sessions: Some(0),
          ..Default::default()
        });
      }
    }
    rows
  }

  /// Whether the timeline counts a session's activity stamp (`usage_by_period`): on a day of a
  /// bucket the session has usage in, within the span.
  fn counted(&self, span: Span, prov: Option<Provider>) -> impl Fn(&Session, Ts) -> bool + '_ {
    let range = span.range(self.today);
    let gran = span.gran();
    let used: std::collections::HashSet<((Provider, &str), Date)> = self
      .events(prov)
      .into_iter()
      .filter_map(|(si, ei)| {
        let ts = self.sessions[si].events[ei].ts.filter(|x| range.contains(*x))?;
        Some((self.sessions[si].root(), gran.bucket(metrics::local_date(ts))))
      })
      .collect();
    move |s: &Session, ts: Ts| {
      let day = metrics::local_date(ts);
      range.contains_day(day) && used.contains(&(s.root(), gran.bucket(day)))
    }
  }

  /// The main sessions' roots: sessions the projects count.
  fn mains(&self) -> std::collections::HashSet<(Provider, &str)> {
    self.sessions.iter().filter(|x| x.is_main).map(|x| x.root()).collect()
  }

  /// The main sessions with usage in the timeline's buckets of `days`, each once.
  pub fn bucket_sessions(&self, span: Span, prov: Option<Provider>, days: &[Date]) -> usize {
    let range = span.range(self.today);
    let gran = span.gran();
    let mains = self.mains();
    let roots: std::collections::HashSet<(Provider, &str)> = self
      .events(prov)
      .into_iter()
      .filter(|(si, ei)| {
        let ts = self.sessions[*si].events[*ei].ts.filter(|x| range.contains(*x));
        ts.is_some_and(|x| days.contains(&gran.bucket(metrics::local_date(x))))
      })
      .map(|(si, _)| self.sessions[si].root())
      .filter(|x| mains.contains(x))
      .collect();
    roots.len()
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

/// Microseconds per name and local day: time when several names are active at once is divided
/// between them, as `metrics::active_by_name` does, and each window is cut at midnight.
fn by_name_and_day(stamps: &HashMap<String, Vec<Ts>>) -> HashMap<(String, Date), i64> {
  let mut changes: std::collections::BTreeMap<Ts, Vec<(&str, bool)>> = Default::default();
  for (name, x) in stamps {
    for (start, end) in metrics::activity_windows(x) {
      changes.entry(start).or_default().push((name, true));
      changes.entry(end).or_default().push((name, false));
    }
  }
  let mut active: std::collections::BTreeSet<&str> = Default::default();
  let mut out: HashMap<(String, Date), i64> = HashMap::new();
  let mut prev: Option<Ts> = None;
  for (ts, items) in changes {
    if let Some(prev) = prev.filter(|_| !active.is_empty()) {
      for (day, x) in metrics::split_by_day(prev, ts) {
        let n = active.len() as i64;
        for (i, name) in active.iter().enumerate() {
          *out.entry((name.to_string(), day)).or_default() += x / n + i64::from((i as i64) < x % n);
        }
      }
    }
    for (name, begins) in items {
      match begins {
        true => active.insert(name),
        false => active.remove(name),
      };
    }
    prev = Some(ts);
  }
  out
}

/// An empty row for `key`; `UNKNOWN` reads `–`.
fn row_of(key: &str) -> Row {
  let label = if key == UNKNOWN { "–".into() } else { key.to_string() };
  Row { key: key.to_string(), label, ..Default::default() }
}

/// `rows` with their time from `times` (key, microseconds); a key with time and no row (time
/// after its last usage left the span) gets one.
fn timed(mut rows: Vec<Row>, times: impl Iterator<Item = (String, i64)>) -> Vec<Row> {
  let mut sums: HashMap<String, i64> = HashMap::new();
  for (key, v) in times {
    *sums.entry(key).or_default() += v;
  }
  for (key, v) in &sums {
    if *v > 0 && !rows.iter().any(|x| &x.key == key) {
      rows.push(row_of(key));
    }
  }
  for row in &mut rows {
    row.active = Some(sums.get(&row.key).copied().unwrap_or(0));
  }
  rows
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
  /// What makes the synthetic fixtures' paths absolute on this platform.
  pub const ROOT: &str = if cfg!(windows) { "C:" } else { "" };

  /// The config of `w/api` at `{ROOT}/w/api`.
  pub fn api_config() -> Config {
    Config::parse(&format!("[projects]\n\"w/api\" = [\"{ROOT}/w/api\"]\n"), None).unwrap()
  }

  fn fixtures(dir: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(dir)
  }

  fn sessions() -> Vec<Session> {
    let mut sessions = crate::parser::claude::parse(&[fixtures("claude/projects")], None);
    sessions.extend(crate::parser::codex::parse(&[fixtures("codex")], None));
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

  /// One project with a few minutes every day for the 150 days up to 2026-01-10.
  pub fn long() -> Data {
    let tz = jiff::tz::TimeZone::system();
    let today = jiff::civil::date(2026, 1, 10);
    let mut sessions = vec![];
    for d in 0..150 {
      let day = today.checked_sub(jiff::Span::new().days(d)).unwrap();
      let mut s = Session::new(&format!("s{d}"), true, &format!("{ROOT}/w/api"), Provider::Claude);
      let start = day.at(10, 0, 0, 0).to_zoned(tz.clone()).unwrap().timestamp().as_microsecond();
      for k in 0..(d % 7 + 2) {
        s.events.push(Event {
          ts: Some(start + k * 60_000_000),
          usage: Some(crate::model::Usage { input: 1000 * (d as u64 + 1), ..Default::default() }),
          model: Some("claude-sonnet-5".into()),
          ..Default::default()
        });
      }
      sessions.push(s);
    }
    Data::new(sessions, api_config(), today)
  }

  /// Projects of every size, a model without a price, one used fast some days, efforts logged
  /// for some turns, and two unconfigured dirs small enough to fold into `other` in the text
  /// report, over the three days before 2026-01-10.
  pub fn many() -> Data {
    let name = "a-project-with-a-name-too-long-for-a-narrow-terminal";
    let dirs = [("/w/api", 40), ("/w/web", 20), ("/w/a-project", 10), ("/tmp/a", 1), ("/tmp/b", 1)];
    let dirs = dirs.map(|(dir, weight)| {
      let dir = if dir == "/w/a-project" { format!("/w/{name}") } else { dir.to_string() };
      (format!("{ROOT}{dir}"), weight)
    });
    let models = [
      (Provider::Claude, "claude-sonnet-5"),
      (Provider::Claude, "claude-unknown-1"),
      (Provider::Codex, "gpt-5.4"),
    ];
    let tz = jiff::tz::TimeZone::system();
    let mut sessions = vec![];
    for d in 1..=3 {
      let day = jiff::civil::date(2026, 1, 6 + d);
      for (dir, weight) in &dirs {
        let weight = *weight;
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
      "[projects]\n\"w/api\" = [\"{ROOT}/w/api\"]\n\"w/web\" = [\"{ROOT}/w/web\"]\n\"w/{name}\" = [\"{ROOT}/w/{name}\"]\n"
    );
    Data::new(sessions, Config::parse(&config, None).unwrap(), jiff::civil::date(2026, 1, 10))
  }

  /// Prompts to four models on 2026-01-09: `sonnet` swears at 50 per 1,000 words in 6,000,
  /// `small` at 80 in 300, `gpt` (Codex) at 10 in 3,000, `tiny` at 500 in 20.
  pub fn swearing() -> Data {
    let tz = jiff::tz::TimeZone::system();
    let at = jiff::civil::date(2026, 1, 9).at(10, 0, 0, 0).to_zoned(tz).unwrap();
    let at = at.timestamp().as_microsecond();
    // A model's words over its prompts, its swear words over the first `swearing` of them.
    let models = [
      ("sonnet", Provider::Claude, 6000, 300, 10, 6),
      ("small", Provider::Claude, 300, 24, 3, 3),
      ("gpt", Provider::Codex, 3000, 30, 10, 2),
      ("tiny", Provider::Claude, 20, 10, 1, 1),
    ];
    let mut sessions = vec![];
    for (i, (model, prov, words, swears, prompts, swearing)) in models.into_iter().enumerate() {
      let mut s = Session::new(model, true, &format!("{ROOT}/w/api"), prov);
      for k in 0..prompts {
        let ts = at + (i as i64 * 100 + k as i64) * 1_000_000;
        let swears = if k < swearing { swears / swearing } else { 0 };
        let model = Some(model.into());
        let prompt = Prompt { ts, model, words: words / prompts, swears, hash: k as u64 };
        s.events.push(Event { ts: Some(ts), prompt: Some(Box::new(prompt)), ..Default::default() });
      }
      sessions.push(s);
    }
    Data::new(sessions, api_config(), jiff::civil::date(2026, 1, 10))
  }

  #[test]
  fn scores_swears_per_model() {
    let data = swearing();
    let (rows, total) = data.swears(Span::Week, None);
    let get = |key: &str| rows.iter().find(|x| x.key == key).unwrap();
    let (sonnet, small, gpt, tiny) = (get("sonnet"), get("small"), get("gpt"), get("tiny"));
    assert_eq!((sonnet.words, sonnet.swears, sonnet.prompts, sonnet.swearing), (6000, 300, 10, 6));
    assert_eq!((total.words, total.swears, total.prompts, total.swearing), (9320, 364, 24, 12));
    // The raw rates: `small` swears more often than `sonnet`.
    assert_eq!(
      (small.per_1k(), sonnet.per_1k(), small.msgs()),
      (Some(80.0), Some(50.0), Some(100.0))
    );
    // Shrunk toward the overall 364 / 9,320, `small`'s 300 words score below `sonnet`'s 6,000.
    let score = |x: &Row| x.score.unwrap();
    let overall = 364.0 / 9320.0;
    assert!((score(small) - (24.0 + 2000.0 * overall) / 2300.0 * 1000.0).abs() < 1e-9);
    assert!(score(sonnet) > score(small) && score(small) > score(gpt), "{rows:?}");
    // Under 200 words no rate; the total's score is the overall rate.
    assert_eq!((tiny.per_1k(), tiny.msgs(), tiny.score), (None, None, None));
    assert!((total.score.unwrap() - overall * 1000.0).abs() < 1e-9);
    // One provider, its own overall rate; nothing in the period, no rows.
    let (rows, total) = data.swears(Span::Week, Some(Provider::Codex));
    assert_eq!((rows.len(), total.score), (1, Some(10.0)));
    assert_eq!(rows[0].score, Some(10.0));
    assert!(data.swears(Span::Today, None).0.is_empty());
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

  /// A session in `w/api` of `(minutes from 2026-01-07 00:00 local, model, tokens)`, an event
  /// without tokens being user activity.
  fn session(id: &str, events: &[(i64, Option<&str>, u64)]) -> Session {
    let tz = jiff::tz::TimeZone::system();
    let start = jiff::civil::date(2026, 1, 7).at(0, 0, 0, 0).to_zoned(tz).unwrap();
    let mut s = Session::new(id, true, &format!("{ROOT}/w/api"), Provider::Claude);
    for (min, model, tokens) in events {
      s.events.push(Event {
        ts: Some(start.timestamp().as_microsecond() + min * 60_000_000),
        usage: (*tokens > 0).then(|| crate::model::Usage { input: *tokens, ..Default::default() }),
        model: model.map(String::from),
        ..Default::default()
      });
    }
    s
  }

  fn data_with(sessions: Vec<Session>, today: Date) -> Data {
    Data::new(sessions, api_config(), today)
  }

  const DAY: i64 = 24 * 60;

  fn minutes(rows: &[Row]) -> i64 {
    rows.iter().map(|x| x.active.unwrap_or(0)).sum::<i64>() / 60_000_000
  }

  #[test]
  fn time_with_no_usage_still_belongs_to_the_last_model() {
    // Model A yesterday; user activity today, then model B: today's first minutes are A's.
    let s = session("s", &[(600, Some("a"), 10), (DAY + 600, None, 0), (DAY + 720, Some("b"), 10)]);
    let data = data_with(vec![s], jiff::civil::date(2026, 1, 8));
    let (_, total) = data.projects(Span::Today, None, true);
    assert_eq!(total.active, Some(10 * 60_000_000));
    let models = data.models(Span::Today, None, None);
    assert_eq!(minutes(&models), 10, "{models:?}");
    for part in data.model_usage(Span::Today, None, "claude/a") {
      assert_eq!(minutes(&part), 5, "{part:?}");
    }
  }

  #[test]
  fn sessions_without_a_model_are_the_unknown_one() {
    let s = session("s", &[(600, None, 10), (602, None, 10)]);
    let data = data_with(vec![s], jiff::civil::date(2026, 1, 8));
    let models = data.models(Span::All, None, None);
    assert_eq!(models.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["claude/unknown"]);
    assert_eq!(minutes(&models), 7);
    let days = data.bucket_models(Span::Days30, None);
    assert_eq!(minutes(days.values().next().unwrap()), 7);
  }

  #[test]
  fn a_day_keeps_the_time_from_before_midnight() {
    // Responses at 23:59 and at 09:00 the next day: that day has 4 + 5 minutes.
    let s = session("s", &[(DAY - 1, Some("a"), 10), (DAY + 540, Some("a"), 10)]);
    let data = data_with(vec![s], jiff::civil::date(2026, 1, 10));
    let day = jiff::civil::date(2026, 1, 8);
    let row = data.timeline(Span::Month, None, None).into_iter().find(|x| x.day == Some(day));
    assert_eq!(minutes(&[row.unwrap()]), 9);
    assert_eq!(minutes(&data.bucket_projects(Span::Month, None, day)), 9);
  }

  #[test]
  fn a_day_splits_its_time_between_the_models_across_midnight() {
    // Model A at 23:59, model B at 09:00 the next day: that day has 4 minutes of A, 5 of B.
    let s = session("s", &[(DAY - 1, Some("a"), 10), (DAY + 540, Some("b"), 10)]);
    let data = data_with(vec![s], jiff::civil::date(2026, 1, 10));
    let models = data.bucket_models(Span::Month, None);
    let day = &models[&jiff::civil::date(2026, 1, 8)];
    let of =
      |key: &str| day.iter().find(|x| x.key == key).map_or(0, |x| minutes(std::slice::from_ref(x)));
    assert_eq!((of("claude/a"), of("claude/b")), (4, 5), "{day:?}");
  }

  #[test]
  fn a_day_counts_main_sessions_only() {
    // An agent's log without its main session's: usage, but no session, as the projects count.
    let mut s = session("agent", &[(600, Some("a"), 10)]);
    s.is_main = false;
    let data = data_with(vec![s], jiff::civil::date(2026, 1, 10));
    let (_, total) = data.projects(Span::Month, None, true);
    let day = data.timeline(Span::Month, None, None).into_iter().find(|x| x.tokens > 0).unwrap();
    assert_eq!((total.sessions, day.sessions), (Some(0), Some(0)));
    assert_eq!(data.bucket_sessions(Span::Month, None, &[day.day.unwrap()]), 0);
  }

  #[test]
  fn cache_warming_owns_no_time() {
    // Model A at high effort, cache warming on B, then a prompt: its time stays with A, high.
    let mut s = session("s", &[(600, Some("a"), 10), (602, Some("b"), 10), (604, None, 0)]);
    s.events[0].effort = Some("high".into());
    s.events[1].background = true;
    let data = data_with(vec![s], jiff::civil::date(2026, 1, 10));
    let times = |rows: Vec<Row>| -> Vec<(String, i64)> {
      rows.into_iter().map(|x| (x.key, x.active.unwrap_or(0) / 60_000_000)).collect()
    };
    let models = times(data.models(Span::All, None, None));
    assert!(
      models.contains(&("claude/a".into(), 9))
        && !models.iter().any(|x| x.0 == "claude/b" && x.1 > 0),
      "{models:?}"
    );
    let [_, effort] = data.model_usage(Span::All, None, "claude/a");
    assert_eq!(times(effort).iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["high"]);
    let days = data.bucket_models(Span::Month, None);
    let keys: Vec<(&str, i64)> = days
      .values()
      .flatten()
      .map(|x| (x.key.as_str(), x.active.unwrap_or(0) / 60_000_000))
      .collect();
    assert!(
      keys.contains(&("claude/a", 9)) && !keys.iter().any(|x| x.0 == "claude/b" && x.1 > 0),
      "{keys:?}"
    );
  }

  #[test]
  fn a_failed_response_owns_no_time() {
    // User at 10:00, an API error naming a synthetic model, then the answer of model A.
    let mut s =
      session("s", &[(600, None, 0), (601, Some("<synthetic>"), 0), (605, Some("a"), 10)]);
    s.events[1].failed = true;
    let data = data_with(vec![s], jiff::civil::date(2026, 1, 10));
    let models = data.models(Span::All, None, None);
    assert_eq!(models.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["claude/a"]);
    let days = data.bucket_models(Span::Month, None);
    let keys: Vec<&str> = days.values().flatten().map(|x| x.key.as_str()).collect();
    assert_eq!(keys, ["claude/a"]);
  }
}
