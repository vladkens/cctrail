// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use jiff::Timestamp;
use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;

use crate::config::{By, Config};
use crate::model::{Event, Provider, Session, Speed, Ts, Usage};
use crate::pricing;

/// Each event counts as this much activity; overlapping windows merge.
const WINDOW: Ts = 5 * 60 * MICROS;
const MICROS: i64 = 1_000_000;

static TZ: LazyLock<TimeZone> = LazyLock::new(TimeZone::system);

// MARK: Calendar

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
  Today,
  Yesterday,
  ThisWeek,
  LastWeek,
  ThisMonth,
  LastMonth,
  All,
}

impl Period {
  pub const ALL: [Period; 7] = [
    Self::Today,
    Self::Yesterday,
    Self::ThisWeek,
    Self::LastWeek,
    Self::ThisMonth,
    Self::LastMonth,
    Self::All,
  ];

  pub fn name(self) -> &'static str {
    match self {
      Self::Today => "today",
      Self::Yesterday => "yesterday",
      Self::ThisWeek => "this-week",
      Self::LastWeek => "last-week",
      Self::ThisMonth => "this-month",
      Self::LastMonth => "last-month",
      Self::All => "all",
    }
  }

  pub fn parse(s: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|x| x.name() == s)
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Granularity {
  Day,
  Week,
  Month,
}

impl Granularity {
  pub fn name(self) -> &'static str {
    match self {
      Self::Day => "day",
      Self::Week => "week",
      Self::Month => "month",
    }
  }

  /// The first day of the bucket holding a day; weeks start on Monday.
  pub fn bucket(self, day: Date) -> Date {
    match self {
      Self::Day => day,
      Self::Week => day - jiff::Span::new().days(day.weekday().to_monday_zero_offset()),
      Self::Month => day.first_of_month(),
    }
  }

  fn bucket_end(self, start: Date) -> Date {
    match self {
      Self::Day => start,
      Self::Week => start + jiff::Span::new().days(6),
      Self::Month => start.last_of_month(),
    }
  }

  /// The bucket's first and last day, clipped to the query.
  pub fn bounds(self, start: Date, range: &DateRange) -> (Date, Date) {
    let end = self.bucket_end(start);
    (range.since.map_or(start, |x| x.max(start)), range.until.map_or(end, |x| x.min(end)))
  }

  pub fn label(self, start: Date, range: &DateRange) -> String {
    if self == Self::Day {
      return start.to_string();
    }
    let (first, last) = self.bounds(start, range);
    if self == Self::Month && first == start && last == self.bucket_end(start) {
      return start.strftime("%Y-%m").to_string();
    }
    match first == last {
      true => first.to_string(),
      false => format!("{first} – {}", last.strftime("%m-%d")),
    }
  }
}

/// Inclusive local days; `None` is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DateRange {
  pub since: Option<Date>,
  pub until: Option<Date>,
}

impl DateRange {
  pub fn resolve(period: Option<Period>, since: Option<Date>, until: Option<Date>) -> Self {
    let Some(period) = period else { return Self { since, until } };
    let today = Timestamp::now().to_zoned(TZ.clone()).date();
    Self::of(period, today)
  }

  fn of(period: Period, today: Date) -> Self {
    let range = |a: Date, b: Date| Self { since: Some(a), until: Some(b) };
    let yesterday = today.yesterday().unwrap();
    let week = Granularity::Week.bucket(today);
    let month = today.first_of_month();
    match period {
      Period::All => Self::default(),
      Period::Today => range(today, today),
      Period::Yesterday => range(yesterday, yesterday),
      Period::ThisWeek => range(week, Granularity::Week.bucket_end(week)),
      Period::LastWeek => {
        let end = week.yesterday().unwrap();
        range(Granularity::Week.bucket(end), end)
      }
      Period::ThisMonth => range(month, month.last_of_month()),
      Period::LastMonth => {
        let end = month.yesterday().unwrap();
        range(end.first_of_month(), end)
      }
    }
  }

  pub fn bounded(&self) -> bool {
    self.since.is_some() || self.until.is_some()
  }

  pub fn contains(&self, ts: Ts) -> bool {
    self.contains_day(local_date(ts))
  }

  pub fn contains_day(&self, day: Date) -> bool {
    self.since.is_none_or(|x| day >= x) && self.until.is_none_or(|x| day <= x)
  }
}

pub fn local_date(ts: Ts) -> Date {
  TZ.to_datetime(timestamp(ts)).date()
}

pub fn timestamp(ts: Ts) -> Timestamp {
  Timestamp::from_microsecond(ts).unwrap_or(Timestamp::UNIX_EPOCH)
}

/// The first moment of a local day; a midnight skipped by DST resolves later, as in Python.
fn day_start(day: Date) -> Ts {
  let dt = day.to_datetime(Time::midnight());
  TZ.to_ambiguous_timestamp(dt).compatible().map_or(0, |x| x.as_microsecond())
}

// MARK: Reports

#[derive(Clone, Debug, PartialEq)]
pub struct ModelSummary {
  pub provider: Provider,
  pub model: String,
  pub fast: bool,
  pub usage: Usage,
  pub cost: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageSummary {
  pub usage: Usage,
  pub cost: f64,
  /// Microseconds.
  pub active: i64,
  pub models: Vec<ModelSummary>,
}

impl UsageSummary {
  pub fn add(&mut self, x: &UsageSummary) {
    self.usage += x.usage;
    self.cost += x.cost;
    self.active += x.active;
  }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stat {
  pub name: String,
  /// The name is a configured project or group, not a working directory or `@unmapped`.
  pub mapped: bool,
  pub sessions: usize,
  pub usage: Usage,
  pub cost: f64,
  /// Microseconds, whole seconds.
  pub active: i64,
  pub first: Option<Ts>,
  pub last: Option<Ts>,
}

/// Billable events as `(session, event)` indexes in history order: not failed, with usage, and
/// one copy per Claude response (the largest; a sidechain copy only without a direct one).
pub fn billable_events(sessions: &[Session]) -> Vec<(usize, usize)> {
  type Key<'a> = (Provider, &'a str);
  let mut anonymous = vec![];
  let mut direct: HashMap<(Key, Option<&str>), (usize, usize)> = HashMap::new();
  let mut sidechains: HashMap<Key, (usize, usize)> = HashMap::new();
  let rank = |(si, ei): (usize, usize)| {
    let e: &Event = &sessions[si].events[ei];
    (e.usage.map_or(0, |x| x.total()), e.speed.is_some())
  };

  for (si, session) in sessions.iter().enumerate() {
    for (ei, event) in session.events.iter().enumerate() {
      if event.failed || event.usage.is_none_or(|x| x.total() == 0) {
        continue;
      }
      let Some((id, request)) = &event.identity else {
        anonymous.push((si, ei));
        continue;
      };
      let key = (session.provider, id.as_str());
      let best = match event.sidechain {
        true => sidechains.entry(key).or_insert((si, ei)),
        false => direct.entry((key, request.as_deref())).or_insert((si, ei)),
      };
      if rank((si, ei)) > rank(*best) {
        *best = (si, ei);
      }
    }
  }

  let direct_ids: HashSet<Key> = direct.keys().map(|(x, _)| *x).collect();
  let mut all = anonymous;
  all.extend(direct.into_values());
  all.extend(sidechains.into_iter().filter(|(k, _)| !direct_ids.contains(k)).map(|(_, x)| x));
  all.sort();
  all
}

pub fn usage_by_period(
  sessions: &[Session],
  gran: Granularity,
  range: &DateRange,
) -> BTreeMap<Date, UsageSummary> {
  let mut summaries: BTreeMap<Date, UsageSummary> = BTreeMap::new();
  let mut models: HashMap<Date, HashMap<(Provider, &str, bool), ModelSummary>> = HashMap::new();
  let mut buckets: HashSet<((Provider, &str), Date)> = HashSet::new();

  for (si, ei) in billable_events(sessions) {
    let (session, event) = (&sessions[si], &sessions[si].events[ei]);
    let Some(ts) = event.ts.filter(|x| range.contains(*x)) else { continue };
    let bucket = gran.bucket(local_date(ts));
    let usage = event.usage.unwrap_or_default();
    let cost =
      pricing::cost(session.provider, event.model.as_deref(), Some(ts), &usage, event.speed);
    let summary = summaries.entry(bucket).or_default();
    summary.usage += usage;
    summary.cost += cost;
    buckets.insert((session.root(), bucket));

    let fast = event.speed == Some(Speed::Fast);
    let model = event.model.as_deref().unwrap_or("unknown");
    let x =
      models.entry(bucket).or_default().entry((session.provider, model, fast)).or_insert_with(
        || ModelSummary {
          provider: session.provider,
          model: model.to_string(),
          fast,
          usage: Usage::default(),
          cost: 0.0,
        },
      );
    x.usage += usage;
    x.cost += cost;
  }

  let mut stamps = vec![];
  for session in sessions {
    for e in &session.events {
      let Some(ts) = e.ts.filter(|_| !e.failed) else { continue };
      let day = local_date(ts);
      if range.contains_day(day) && buckets.contains(&(session.root(), gran.bucket(day))) {
        stamps.push(ts);
      }
    }
  }
  for (day, active) in active_by_day(stamps) {
    let bucket = gran.bucket(day);
    if let Some(x) = summaries.get_mut(&bucket).filter(|_| range.contains_day(day)) {
      x.active += active;
    }
  }

  for (bucket, values) in models {
    let mut values: Vec<ModelSummary> = values.into_values().collect();
    values.sort_by(|a, b| {
      (b.usage.total(), a.provider, &a.model, a.fast).cmp(&(
        a.usage.total(),
        b.provider,
        &b.model,
        b.fast,
      ))
    });
    summaries.get_mut(&bucket).unwrap().models = values;
  }
  summaries
}

pub fn stats_by(sessions: &[Session], range: &DateRange, config: &Config, by: By) -> Vec<Stat> {
  let events = billable_events(sessions);
  let bounded = range.bounded();
  let in_range = |e: &Event| !bounded || e.ts.is_some_and(|x| range.contains(x));
  let billable: HashSet<(Provider, &str)> = events
    .iter()
    .filter(|(si, ei)| in_range(&sessions[*si].events[*ei]))
    .map(|(si, _)| sessions[*si].root())
    .collect();
  let mains: HashSet<(Provider, &str)> =
    sessions.iter().filter(|x| x.is_main).map(|x| x.root()).collect();

  // Whether a name is a configured project or group comes from the config, never from its text:
  // a working directory such as `C:/repo` looks like a `<group>/<project>` label.
  let mut cache: HashMap<&str, (String, bool)> = HashMap::new();
  let keys: Vec<(String, bool)> = sessions
    .iter()
    .map(|x| {
      let mapped = || by != By::Dir && config.find(&x.workdir).is_some();
      cache.entry(&x.workdir).or_insert_with(|| (config.map(&x.workdir, by), mapped())).clone()
    })
    .collect();
  let names: Vec<&str> = keys.iter().map(|x| x.0.as_str()).collect();
  let new_stat =
    |si: usize| Stat { name: keys[si].0.clone(), mapped: keys[si].1, ..Default::default() };

  let mut stats: HashMap<&str, Stat> = HashMap::new();
  let mut roots: HashMap<&str, HashSet<(Provider, &str)>> = HashMap::new();
  let mut stamps: HashMap<&str, Vec<Ts>> = HashMap::new();
  for (si, session) in sessions.iter().enumerate() {
    if !billable.contains(&session.root()) {
      continue;
    }
    let own: Vec<Ts> =
      session.events.iter().filter(|e| !e.failed && in_range(e)).filter_map(|e| e.ts).collect();
    let (Some(first), Some(last)) = (own.iter().min(), own.iter().max()) else { continue };
    let name = names[si];
    let stat = stats.entry(name).or_insert_with(|| new_stat(si));
    stat.first = Some(stat.first.map_or(*first, |x| x.min(*first)));
    stat.last = Some(stat.last.map_or(*last, |x| x.max(*last)));
    stamps.entry(name).or_default().extend(own);
    if mains.contains(&session.root()) {
      roots.entry(name).or_default().insert(session.root());
    }
  }

  for (si, ei) in events {
    let (session, event) = (&sessions[si], &sessions[si].events[ei]);
    if !in_range(event) {
      continue;
    }
    let name = names[si];
    let stat = stats.entry(name).or_insert_with(|| new_stat(si));
    let usage = event.usage.unwrap_or_default();
    stat.usage += usage;
    let model = event.model.as_deref();
    stat.cost += pricing::cost(session.provider, model, event.ts, &usage, event.speed);
  }

  for (name, seconds) in active_by_name(&stamps, range) {
    if let Some(x) = stats.get_mut(name) {
      x.active = seconds * MICROS;
    }
  }
  for (name, x) in roots {
    if let Some(stat) = stats.get_mut(name) {
      stat.sessions = x.len();
    }
  }

  let mut stats: Vec<Stat> = stats.into_values().collect();
  stats.sort_by(|a, b| {
    (b.active, b.usage.total(), &a.name).cmp(&(a.active, a.usage.total(), &b.name))
  });
  stats
}

pub fn combine_stats<'a>(name: &str, stats: impl IntoIterator<Item = &'a Stat>) -> Stat {
  let mut total = Stat { name: name.to_string(), ..Default::default() };
  for x in stats {
    total.sessions += x.sessions;
    total.usage += x.usage;
    total.cost += x.cost;
    total.active += x.active;
    if let Some(first) = x.first {
      total.first = Some(total.first.map_or(first, |t| t.min(first)));
    }
    if let Some(last) = x.last {
      total.last = Some(total.last.map_or(last, |t| t.max(last)));
    }
  }
  total
}

/// Tokens on models the price catalog lacks, by model, most first: their cost counts as 0.
pub fn unpriced(sessions: &[Session], range: &DateRange) -> Vec<(String, u64)> {
  let mut models: HashMap<&str, u64> = HashMap::new();
  for (si, ei) in billable_events(sessions) {
    let (session, event) = (&sessions[si], &sessions[si].events[ei]);
    if range.bounded() && !event.ts.is_some_and(|x| range.contains(x)) {
      continue;
    }
    if pricing::price(session.provider, event.model.as_deref(), event.ts).is_none() {
      let model = event.model.as_deref().unwrap_or("unknown");
      let x = models.entry(model).or_default();
      *x = x.saturating_add(event.usage.map_or(0, |x| x.total()));
    }
  }
  let mut models: Vec<(String, u64)> =
    models.into_iter().filter(|x| x.1 > 0).map(|(k, v)| (k.to_string(), v)).collect();
  models.sort_by(|a, b| (b.1, &a.0).cmp(&(a.1, &b.0)));
  models
}

// MARK: Active time

/// Merged activity windows `[start, end)` of the stamps.
pub fn activity_windows(stamps: &[Ts]) -> Vec<(Ts, Ts)> {
  let mut stamps = stamps.to_vec();
  stamps.sort_unstable();
  stamps.dedup();
  let mut windows: Vec<(Ts, Ts)> = vec![];
  for x in stamps {
    match windows.last_mut() {
      Some(w) if x <= w.1 => w.1 = w.1.max(x + WINDOW),
      _ => windows.push((x, x + WINDOW)),
    }
  }
  windows
}

fn active_by_day(stamps: Vec<Ts>) -> BTreeMap<Date, i64> {
  let mut days = BTreeMap::new();
  for (start, end) in activity_windows(&stamps) {
    for (day, x) in split_by_day(start, end) {
      *days.entry(day).or_default() += x;
    }
  }
  days
}

/// Seconds per name; time when several names are active at once is divided between them.
fn active_by_name<'a>(
  stamps: &HashMap<&'a str, Vec<Ts>>,
  range: &DateRange,
) -> HashMap<&'a str, i64> {
  let mut changes: BTreeMap<Ts, Vec<(&str, bool)>> = BTreeMap::new();
  for (name, x) in stamps {
    for (start, end) in activity_windows(x) {
      changes.entry(start).or_default().push((name, true));
      changes.entry(end).or_default().push((name, false));
    }
  }

  let mut active: BTreeSet<&str> = BTreeSet::new();
  let mut micros: HashMap<&str, i64> = HashMap::new();
  let mut prev: Option<Ts> = None;
  for (ts, items) in changes {
    if let Some(prev) = prev.filter(|_| !active.is_empty()) {
      match range.bounded() {
        true => split_by_day(prev, ts)
          .into_iter()
          .filter(|(day, _)| range.contains_day(*day))
          .for_each(|(_, x)| distribute(&mut micros, &active, x)),
        false => distribute(&mut micros, &active, ts - prev),
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
  whole_seconds(micros)
}

/// Splits `[start, end)` at local midnights.
fn split_by_day(start: Ts, end: Ts) -> Vec<(Date, i64)> {
  let mut parts = vec![];
  let mut cur = start;
  while cur < end {
    let day = local_date(cur);
    let next = day.tomorrow().map_or(end, day_start).min(end).max(cur + 1);
    parts.push((day, next - cur));
    cur = next;
  }
  parts
}

fn distribute<'a>(micros: &mut HashMap<&'a str, i64>, names: &BTreeSet<&'a str>, x: i64) {
  let n = names.len() as i64;
  let (share, rest) = (x / n, x % n);
  for (i, name) in names.iter().enumerate() {
    *micros.entry(name).or_default() += share + i64::from((i as i64) < rest);
  }
}

/// Rounds to whole seconds keeping the total: the largest fractions get the remainder.
fn whole_seconds(micros: HashMap<&str, i64>) -> HashMap<&str, i64> {
  let total = micros.values().sum::<i64>() / MICROS;
  let mut seconds: HashMap<&str, i64> = micros.iter().map(|(k, v)| (*k, v / MICROS)).collect();
  let rest = total - seconds.values().sum::<i64>();
  let mut fractions: Vec<(&str, i64)> = micros.into_iter().collect();
  fractions.sort_by(|a, b| (b.1 % MICROS, a.0).cmp(&(a.1 % MICROS, b.0)));
  for (name, _) in fractions.into_iter().take(rest as usize) {
    *seconds.get_mut(name).unwrap() += 1;
  }
  seconds
}

#[cfg(test)]
mod tests {
  use jiff::civil::date;

  use super::*;

  fn ts(s: &str) -> Ts {
    s.parse::<Timestamp>().unwrap().as_microsecond()
  }

  fn session(id: &str, at: Ts) -> Session {
    let mut x = Session::new(id, true, id, Provider::Codex);
    let usage = Usage { input: 1, ..Default::default() };
    x.events.push(Event {
      ts: Some(at),
      usage: Some(usage),
      speed: Some(Speed::Standard),
      ..Default::default()
    });
    x
  }

  #[test]
  fn divides_parallel_time_between_workdirs() {
    let at = ts("2026-01-01T10:00:00Z");
    let sessions = [session("/first", at), session("/second", at)];
    let stats = stats_by(&sessions, &DateRange::default(), &Config::default(), By::Dir);

    assert_eq!(stats.len(), 2);
    assert!(stats.iter().all(|x| x.active == 150 * MICROS));
  }

  #[test]
  fn rounds_divided_time_keeping_the_total() {
    let micros = HashMap::from([("a", 1_400_000), ("b", 1_300_000), ("c", 1_300_000)]);
    let seconds = whole_seconds(micros);
    assert_eq!(seconds.values().sum::<i64>(), 4);
    assert_eq!((seconds["a"], seconds["b"], seconds["c"]), (2, 1, 1));

    let mut micros = HashMap::new();
    distribute(&mut micros, &BTreeSet::from(["a", "b", "c"]), 5);
    assert_eq!((micros["a"], micros["b"], micros["c"]), (2, 2, 1));
  }

  #[test]
  fn weeks_start_on_monday() {
    assert_eq!(Granularity::Week.bucket(date(2026, 8, 5)), date(2026, 8, 3));
    assert_eq!(Granularity::Week.bucket(date(2026, 8, 3)), date(2026, 8, 3));
    assert_eq!(Granularity::Week.bucket(date(2026, 8, 9)), date(2026, 8, 3));
    assert_eq!(Granularity::Month.bucket(date(2026, 8, 31)), date(2026, 8, 1));
  }

  #[test]
  fn clips_calendar_buckets_to_query() {
    let range = DateRange { since: Some(date(2026, 8, 10)), until: Some(date(2026, 8, 20)) };
    let month = Granularity::Month;
    assert_eq!(month.bounds(date(2026, 8, 1), &range), (date(2026, 8, 10), date(2026, 8, 20)));
    assert_eq!(month.label(date(2026, 8, 1), &range), "2026-08-10 – 08-20");
    assert_eq!(month.label(date(2026, 8, 1), &DateRange::default()), "2026-08");
    assert_eq!(
      Granularity::Week.label(date(2026, 8, 3), &DateRange::default()),
      "2026-08-03 – 08-09"
    );
    let day = DateRange { since: Some(date(2026, 8, 9)), until: None };
    assert_eq!(Granularity::Week.label(date(2026, 8, 3), &day), "2026-08-09");
  }

  #[test]
  fn resolves_periods() {
    let today = date(2026, 3, 4); // Wednesday
    let of = |p| {
      let x = DateRange::of(p, today);
      (x.since.unwrap(), x.until.unwrap())
    };
    assert_eq!(of(Period::Today), (today, today));
    assert_eq!(of(Period::Yesterday), (date(2026, 3, 3), date(2026, 3, 3)));
    assert_eq!(of(Period::ThisWeek), (date(2026, 3, 2), date(2026, 3, 8)));
    assert_eq!(of(Period::LastWeek), (date(2026, 2, 23), date(2026, 3, 1)));
    assert_eq!(of(Period::ThisMonth), (date(2026, 3, 1), date(2026, 3, 31)));
    assert_eq!(of(Period::LastMonth), (date(2026, 2, 1), date(2026, 2, 28)));
    assert_eq!(DateRange::of(Period::All, today), DateRange::default());
  }

  #[test]
  fn merges_overlapping_activity_windows() {
    let at = ts("2026-01-01T10:00:00Z");
    let min = 60 * MICROS;
    assert_eq!(activity_windows(&[at, at + 2 * min, at]), vec![(at, at + 7 * min)]);
    assert_eq!(activity_windows(&[at, at + 5 * min]), vec![(at, at + 10 * min)]);
    assert_eq!(
      activity_windows(&[at, at + 6 * min]),
      vec![(at, at + 5 * min), (at + 6 * min, at + 11 * min)]
    );
  }

  #[test]
  fn splits_windows_at_local_midnight() {
    let start = day_start(date(2026, 1, 2)) - 60 * MICROS;
    let parts = split_by_day(start, start + 5 * 60 * MICROS);
    assert_eq!(parts, vec![(date(2026, 1, 1), 60 * MICROS), (date(2026, 1, 2), 4 * 60 * MICROS)]);
  }

  #[test]
  fn mapped_comes_from_config_not_the_name() {
    let at = ts("2026-01-01T10:00:00Z");
    let root = if cfg!(windows) { "C:/oss" } else { "/oss" };
    let config = Config::parse(&format!("[projects]\n'oss/demo' = ['{root}']"), None).unwrap();
    let sessions = [session("C:/repo", at), session("a/b", at), session(root, at)];

    let stats = stats_by(&sessions, &DateRange::default(), &config, By::Project);
    let mapped = |name: &str| stats.iter().find(|x| x.name == name).unwrap().mapped;
    assert!(!mapped("C:/repo") && !mapped("a/b") && mapped("oss/demo"));

    let stats = stats_by(&sessions, &DateRange::default(), &config, By::Group);
    let mapped = |name: &str| stats.iter().find(|x| x.name == name).unwrap().mapped;
    assert!(!mapped("@unmapped") && mapped("oss"));
  }

  #[test]
  fn failed_events_are_not_billed_or_counted_as_activity() {
    let at = ts("2026-01-01T10:00:00Z");
    let mut x = session("/repo", at);
    let usage = Usage { input: 100, ..Default::default() };
    x.events.push(Event {
      ts: Some(at + 3600 * MICROS),
      usage: Some(usage),
      failed: true,
      ..Default::default()
    });

    let stat = &stats_by(&[x], &DateRange::default(), &Config::default(), By::Dir)[0];
    assert_eq!(stat.usage.total(), 1);
    assert_eq!(stat.active, 300 * MICROS);
    assert_eq!(stat.last, Some(at));
  }

  #[test]
  fn deduplicates_claude_responses() {
    let usage = |input| Some(Usage { input, ..Default::default() });
    let event = |id: &str, input, sidechain| Event {
      ts: Some(0),
      usage: usage(input),
      identity: Some((id.to_string(), Some("r".to_string()))),
      sidechain,
      ..Default::default()
    };
    let mut a = Session::new("a", true, "/repo", Provider::Claude);
    a.events = vec![event("m1", 10, false), event("m2", 5, true), event("m3", 0, false)];
    let mut b = Session::new("b", false, "/repo", Provider::Claude);
    b.events = vec![event("m1", 20, false), event("m1", 30, true), event("m2", 7, true)];

    assert_eq!(billable_events(&[a, b]), vec![(1, 0), (1, 2)]);
  }
}
