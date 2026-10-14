// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::Path;

use jiff::civil::Date;
use serde_json::{Value, json};

use crate::config::{By, UNMAPPED, home_dir, split_project};
use crate::metrics::{
  DateRange, Granularity, ModelSummary, Stat, UsageSummary, combine_stats, local_date,
};
use crate::model::{Ts, Usage};

pub const CONFIG_GUIDE: &str = "https://github.com/vladkens/cctrail#projects";

// MARK: Terminal

const BOLD: &str = "1";
const DIM: &str = "2";
const CYAN: &str = "36";
const GREEN: &str = "32";
const YELLOW: &str = "33";

/// Colors and width of stdout: color only on a terminal without `NO_COLOR`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Term {
  pub color: bool,
  pub width: Option<usize>,
}

impl Term {
  pub fn detect() -> Self {
    let tty = std::io::stdout().is_terminal();
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|x| !x.is_empty());
    let columns = std::env::var("COLUMNS").ok().and_then(|x| x.parse().ok()).filter(|x| *x > 0);
    Self {
      color: tty && !no_color,
      width: columns.or_else(|| if tty { tty_width() } else { None }),
    }
  }

  pub fn paint(&self, text: &str, styles: &[&str]) -> String {
    match self.color && !styles.is_empty() && !text.is_empty() {
      true => format!("\x1b[{}m{text}\x1b[0m", styles.join(";")),
      false => text.to_string(),
    }
  }
}

#[cfg(unix)]
fn tty_width() -> Option<usize> {
  let mut size: libc::winsize = unsafe { std::mem::zeroed() };
  // SAFETY: TIOCGWINSZ only writes a `winsize` into the pointer it gets.
  let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
  (ok && size.ws_col > 0).then_some(size.ws_col as usize)
}

#[cfg(not(unix))]
fn tty_width() -> Option<usize> {
  None
}

/// Writes to stdout, ignoring a closed pipe (`cctrail | head`).
pub fn emit(text: &str) {
  let _ = std::io::stdout().lock().write_all(text.as_bytes());
}

// MARK: Tables

pub struct Table {
  pub headers: Vec<&'static str>,
  pub rows: Vec<Vec<String>>,
  /// A column printed dim.
  pub dim: Option<usize>,
  /// The widest the first column may get, in characters.
  pub max_first: Option<usize>,
}

impl Table {
  pub fn new(headers: Vec<&'static str>, rows: Vec<Vec<String>>) -> Self {
    Self { headers, rows, dim: None, max_first: None }
  }

  /// Columns one space apart, the first left-aligned and truncated with `…` to fit the width.
  pub fn render(&self, term: &Term) -> String {
    let n = self.headers.len();
    let len = |x: &str| x.chars().count();
    let mut widths: Vec<usize> = self.headers.iter().map(|x| len(x)).collect();
    for row in &self.rows {
      for (i, x) in row.iter().enumerate() {
        widths[i] = widths[i].max(len(x));
      }
    }
    if let Some(max) = self.max_first {
      widths[0] = widths[0].min(max);
    }
    if let Some(width) = term.width {
      let rest: usize = widths[1..].iter().sum::<usize>() + n - 1;
      widths[0] = widths[0].min(width.saturating_sub(rest).max(self.headers[0].len()));
    }

    let left = |i: usize| i == 0 || self.headers[i] == "Last active";
    let mut out = String::new();
    let mut line = |cells: Vec<String>, styles: &dyn Fn(usize, &str) -> Vec<&'static str>| {
      let mut parts = vec![];
      for (i, x) in cells.iter().enumerate() {
        let x = truncate(x, widths[i]);
        let pad = " ".repeat(widths[i] - len(&x));
        let x_painted = term.paint(&x, &styles(i, &x));
        parts.push(match left(i) {
          true if i == n - 1 => x_painted,
          true => format!("{x_painted}{pad}"),
          false => format!("{pad}{x_painted}"),
        });
      }
      out.push_str(&parts.join(" "));
      out.push('\n');
    };

    let headers = self.headers.iter().map(|x| x.to_string()).collect();
    line(headers, &|_, _| vec![BOLD]);
    for row in &self.rows {
      let bold = row[0] == "total" || row[0] == UNMAPPED;
      line(row.clone(), &|i, x| {
        let mut styles = vec![];
        if bold {
          styles.push(BOLD);
        }
        if self.dim == Some(i) {
          styles.push(DIM);
        }
        match self.headers[i] {
          _ if i == 0 && !x.starts_with("  ") => styles.push(CYAN),
          "Cost" => styles.push(GREEN),
          "Time" => styles.push(YELLOW),
          _ => {}
        }
        styles
      });
    }
    out
  }
}

fn truncate(x: &str, width: usize) -> String {
  if x.chars().count() <= width {
    return x.to_string();
  }
  let mut x: String = x.chars().take(width.saturating_sub(1)).collect();
  x.push('…');
  x
}

// MARK: Formats

/// `~/path` for paths under the home directory.
pub fn workdir_label(workdir: &str) -> String {
  let home = home_dir().unwrap_or_else(|| ".".into());
  match Path::new(workdir).strip_prefix(&home) {
    Ok(x) if x.as_os_str().is_empty() => "~".to_string(),
    Ok(x) => format!("~/{}", x.display()),
    Err(_) => workdir.to_string(),
  }
}

pub fn tokens(x: u64) -> String {
  match x {
    1_000_000_000.. => format!("{:.2}B", x as f64 / 1e9),
    1_000_000.. => format!("{:.2}M", x as f64 / 1e6),
    1_000.. => format!("{:.1}K", x as f64 / 1e3),
    _ => x.to_string(),
  }
}

pub fn cost(x: f64) -> String {
  format!("${x:.2}")
}

/// Microseconds as `Hh MMm`, rounded to the minute.
pub fn duration(micros: i64) -> String {
  let minutes = (micros / 1_000_000 + 30) / 60;
  format!("{}h {:02}m", minutes / 60, minutes % 60)
}

pub fn day(ts: Option<Ts>) -> String {
  ts.map_or("-".to_string(), |x| local_date(x).to_string())
}

// MARK: Reports

pub fn stats_text(
  by: By,
  stats: &[Stat],
  total: &Stat,
  config: Option<&Path>,
  term: &Term,
) -> String {
  let label = match by {
    By::Project => "Project",
    By::Group => "Group",
    By::Dir => "Directory",
  };
  let row = |x: &Stat| {
    let name = match by {
      By::Group => x.name.clone(),
      _ if split_project(&x.name).is_some() => x.name.clone(),
      _ => workdir_label(&x.name),
    };
    vec![
      name,
      x.sessions.to_string(),
      tokens(x.usage.total()),
      day(x.last),
      cost(x.cost),
      duration(x.active),
    ]
  };

  let unmapped: Vec<&Stat> = stats.iter().filter(|x| split_project(&x.name).is_none()).collect();
  let mut rows: Vec<Vec<String>> = match by {
    By::Project => stats.iter().filter(|x| split_project(&x.name).is_some()).map(row).collect(),
    _ => stats.iter().map(row).collect(),
  };
  if by == By::Project && !unmapped.is_empty() {
    rows.push(row(&combine_stats(UNMAPPED, unmapped.iter().copied())));
  }
  rows.push(row(total));
  let headers = vec![label, "Sessions", "Total tokens", "Last active", "Cost", "Time"];
  let table = Table { headers, rows, dim: Some(3), max_first: Some(60) };
  let mut out = table.render(term);
  if by != By::Project || unmapped.is_empty() {
    return out;
  }

  let dirs = if unmapped.len() == 1 { "dir" } else { "dirs" };
  out += &format!(
    "\n{} {}\n",
    term.paint(&format!("{} unmapped {dirs} · run", unmapped.len()), &[DIM]),
    term.paint("cctrail --by dir", &[CYAN])
  );
  let Some(path) = config else { return out };
  let path_label = term.paint(&workdir_label(&path.to_string_lossy()), &[CYAN]);
  if path.is_file() {
    out += &format!("{} {path_label}\n", term.paint("Project config:", &[DIM]));
    return out;
  }
  out += &format!("\n{}\n", term.paint("No project config found", &[DIM]));
  out += &format!("{} {path_label}\n", term.paint("Create:", &[DIM]));
  out += &format!("{} {}\n", term.paint("Setup guide:", &[DIM]), term.paint(CONFIG_GUIDE, &[CYAN]));
  out
}

pub fn usage_text(
  gran: Granularity,
  buckets: &BTreeMap<Date, UsageSummary>,
  range: &DateRange,
  models: bool,
  total: &UsageSummary,
  term: &Term,
) -> String {
  let row = |label: String, x: &Usage, dollars: f64, active: Option<i64>| {
    let time = active.map(duration).unwrap_or_default();
    vec![label, tokens(x.input), tokens(x.output), tokens(x.total()), cost(dollars), time]
  };
  let mut rows = vec![];
  for (start, x) in buckets {
    rows.push(row(gran.label(*start, range), &x.usage, x.cost, Some(x.active)));
    for m in x.models.iter().filter(|_| models) {
      let fast = if m.fast { " (fast)" } else { "" };
      let label = format!("  {}/{}{fast}", m.provider.name(), m.model);
      rows.push(row(label, &m.usage, m.cost, None));
    }
  }
  rows.push(row("total".into(), &total.usage, total.cost, Some(total.active)));
  let label = match (gran, models) {
    (Granularity::Day, false) => "Date",
    (Granularity::Week, false) => "Week",
    (Granularity::Month, false) => "Month",
    (Granularity::Day, true) => "Date / Model",
    (Granularity::Week, true) => "Week / Model",
    (Granularity::Month, true) => "Month / Model",
  };
  Table::new(vec![label, "Input", "Output", "Total", "Cost", "Time"], rows).render(term)
}

pub fn here_text(title: &str, workdir: &str, rows: &[(&str, UsageSummary)], term: &Term) -> String {
  let title = format!(
    "{}{}{}\n",
    term.paint(title, &[BOLD, CYAN]),
    term.paint(" · ", &[DIM]),
    term.paint(&workdir_label(workdir), &[DIM])
  );
  let rows = rows
    .iter()
    .map(|(label, x)| {
      vec![label.to_string(), tokens(x.usage.total()), cost(x.cost), duration(x.active)]
    })
    .collect();
  title + &Table::new(vec!["Period", "Total tokens", "Cost", "Time"], rows).render(term)
}

/// The tokens left out of the cost; suggests `update-pricing` only when it may help.
pub fn unpriced_text(models: &[(String, u64)], stale: bool, term: &Term) -> String {
  if models.is_empty() {
    return String::new();
  }
  let total = models.iter().map(|x| x.1).sum();
  let names: Vec<&str> = models.iter().map(|x| x.0.as_str()).collect();
  let text = format!("Cost excludes {} on unpriced models: {}", tokens(total), names.join(", "));
  match stale {
    true => format!(
      "\n{} {}\n",
      term.paint(&format!("{text} · run"), &[DIM]),
      term.paint("cctrail update-pricing", &[CYAN])
    ),
    false => format!("\n{}\n", term.paint(&text, &[DIM])),
  }
}

// MARK: JSON

pub fn group_json(x: &Stat, by: By) -> Value {
  let mut value = stat_json(x);
  match by {
    By::Project => {
      let (group, project) = split_project(&x.name).unwrap_or((UNMAPPED, &x.name));
      value.insert("group".into(), json!(group));
      value.insert("project".into(), json!(project));
    }
    By::Group => _ = value.insert("group".into(), json!(x.name)),
    By::Dir => _ = value.insert("workdir".into(), json!(x.name)),
  }
  Value::Object(value)
}

pub fn total_json(x: &Stat, count: usize, by: By) -> Value {
  let mut value = stat_json(x);
  let key = match by {
    By::Project => "projects",
    By::Group => "groups",
    By::Dir => "workdirs",
  };
  value.insert(key.into(), json!(count));
  Value::Object(value)
}

pub fn summary_json(x: &UsageSummary) -> serde_json::Map<String, Value> {
  let value = json!({
    "usage": usage_json(&x.usage),
    "cost_usd": cost_json(x.cost),
    "active_seconds": seconds_json(x.active),
  });
  match value {
    Value::Object(x) => x,
    _ => unreachable!(),
  }
}

pub fn unpriced_json(models: &[(String, u64)]) -> Value {
  json!(models.iter().map(|x| &x.0).collect::<Vec<_>>())
}

pub fn usage_json(x: &Usage) -> Value {
  json!({
    "input": x.input,
    "cache_write_5m": x.cache_write_5m,
    "cache_write_1h": x.cache_write_1h,
    "cache_write": x.cache_write(),
    "cache_read": x.cache_read,
    "output": x.output,
    "reasoning": x.reasoning,
    "total": x.total(),
  })
}

/// Dollars rounded to 1e-12, as the Python version.
pub fn cost_json(x: f64) -> f64 {
  (x * 1e12 + 0.5).floor() / 1e12
}

pub fn seconds_json(micros: i64) -> i64 {
  micros / 1_000_000
}

/// UTC ISO 8601 with `+00:00`, microseconds only when not zero.
pub fn ts_json(ts: Option<Ts>) -> Value {
  let Some(ts) = ts else { return Value::Null };
  let dt = jiff::tz::TimeZone::UTC.to_datetime(crate::metrics::timestamp(ts));
  let micros = ts.rem_euclid(1_000_000);
  let base = dt.strftime("%Y-%m-%dT%H:%M:%S");
  match micros {
    0 => json!(format!("{base}+00:00")),
    _ => json!(format!("{base}.{micros:06}+00:00")),
  }
}

pub fn model_json(x: &ModelSummary) -> Value {
  json!({
    "provider": x.provider.name(),
    "model": x.model,
    "speed": if x.fast { json!("fast") } else { Value::Null },
    "usage": usage_json(&x.usage),
    "cost_usd": cost_json(x.cost),
  })
}

pub fn stat_json(x: &Stat) -> serde_json::Map<String, Value> {
  let value = json!({
    "sessions": x.sessions,
    "usage": usage_json(&x.usage),
    "cost_usd": cost_json(x.cost),
    "active_seconds": seconds_json(x.active),
    "first_activity": ts_json(x.first),
    "last_activity": ts_json(x.last),
  });
  match value {
    Value::Object(x) => x,
    _ => unreachable!(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn formats_numbers() {
    assert_eq!(tokens(999), "999");
    assert_eq!(tokens(1_000), "1.0K");
    assert_eq!(tokens(65_449), "65.4K");
    assert_eq!(tokens(195_990_000), "195.99M");
    assert_eq!(tokens(10_280_000_000), "10.28B");
    assert_eq!(cost(0.125), "$0.12");
    assert_eq!(cost(0.375), "$0.38");
    assert_eq!(cost(87.4), "$87.40");
    assert_eq!(duration(0), "0h 00m");
    assert_eq!(duration(29 * 1_000_000), "0h 00m");
    assert_eq!(duration(30 * 1_000_000), "0h 01m");
    assert_eq!(duration((139 * 3600 + 35 * 60) * 1_000_000), "139h 35m");
  }

  #[test]
  fn formats_json_values() {
    assert_eq!(cost_json(0.000549500000000001), 0.0005495);
    let ts = "2026-10-11T23:59:20.742Z".parse::<jiff::Timestamp>().unwrap().as_microsecond();
    assert_eq!(ts_json(Some(ts)), json!("2026-10-11T23:59:20.742000+00:00"));
    assert_eq!(ts_json(Some(ts - 742_000)), json!("2026-10-11T23:59:20+00:00"));
    assert_eq!(ts_json(None), Value::Null);
  }

  fn table(rows: Vec<Vec<&str>>) -> Table {
    let rows = rows.into_iter().map(|x| x.into_iter().map(String::from).collect()).collect();
    Table::new(vec!["Project", "Sessions", "Last active", "Time"], rows)
  }

  #[test]
  fn aligns_columns() {
    let x = table(vec![
      vec!["oss/demo", "1", "2026-01-01", "0h 05m"],
      vec!["total", "12", "-", "10h 00m"],
    ]);
    let out = x.render(&Term::default());
    assert_eq!(
      out,
      "Project  Sessions Last active    Time\noss/demo        1 2026-01-01   0h 05m\ntotal          12 -           10h 00m\n"
    );
  }

  #[test]
  fn fits_terminal_width() {
    let name = "  ~/Documents/Codex/2026-08-06/a-very-long-project-name-that-goes-on";
    let mut x = table(vec![vec![name, "12", "2026-08-29", "12h 34m"]]);
    x.max_first = Some(60);
    let out = x.render(&Term { color: false, width: None });
    assert!(out.lines().nth(1).unwrap().starts_with(&name[..59]));
    assert!(out.lines().all(|x| x.chars().count() <= 60 + 31));

    let out = x.render(&Term { color: false, width: Some(50) });
    assert!(out.lines().all(|x| x.chars().count() <= 50), "{out}");
    assert!(out.lines().nth(1).unwrap().starts_with("  ~/Documents"));
    assert!(out.contains('…'));
  }

  #[test]
  fn colors_only_when_asked() {
    let x = table(vec![vec!["oss/demo", "1", "2026-01-01", "0h 05m"]]);
    assert!(!x.render(&Term::default()).contains('\x1b'));
    let out = x.render(&Term { color: true, width: None });
    assert!(out.contains("\x1b[36moss/demo\x1b[0m"));
    assert!(out.contains("\x1b[33m0h 05m\x1b[0m"));
  }
}
