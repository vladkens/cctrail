// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::HashMap;

use anyhow::{Result, bail};
use jiff::civil::Date;
use serde_json::{Map, Value, json};

use crate::config::{self, By, Config};
use crate::metrics::{self, DateRange, Granularity, Period, UsageSummary};
use crate::model::{Provider, Session};
use crate::output::{self, Term};
use crate::parser::parse_history;
use crate::{pricing, workdir};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
  Stats,
  Here,
  Days,
  Weeks,
  Months,
  UpdatePricing,
}

impl Cmd {
  fn parse(s: &str) -> Option<Self> {
    match s {
      "here" => Some(Self::Here),
      "days" => Some(Self::Days),
      "weeks" => Some(Self::Weeks),
      "months" => Some(Self::Months),
      "update-pricing" => Some(Self::UpdatePricing),
      _ => None,
    }
  }

  pub fn name(self) -> &'static str {
    match self {
      Self::Stats => "stats",
      Self::Here => "here",
      Self::Days => "days",
      Self::Weeks => "weeks",
      Self::Months => "months",
      Self::UpdatePricing => "update-pricing",
    }
  }

  fn has_period(self) -> bool {
    matches!(self, Self::Stats | Self::Days | Self::Weeks | Self::Months)
  }
}

#[derive(Debug)]
pub struct Args {
  pub cmd: Cmd,
  pub period: Option<Period>,
  pub since: Option<Date>,
  pub until: Option<Date>,
  pub provider: Option<Provider>,
  pub by: By,
  pub models: bool,
  pub json: bool,
  /// Every row, no `other` collapse.
  pub all: bool,
}

#[derive(Debug)]
pub enum Parsed {
  Run(Args),
  Help,
  Version,
}

pub fn parse(argv: &[String]) -> Result<Parsed> {
  let mut args = Args {
    cmd: Cmd::Stats,
    period: None,
    since: None,
    until: None,
    provider: None,
    by: By::Project,
    models: false,
    json: false,
    all: false,
  };
  let mut cmd = None;
  let mut flags = vec![];
  let mut it = argv.iter();
  while let Some(arg) = it.next() {
    let (name, inline) = match arg.split_once('=') {
      Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
      _ => (arg.as_str(), None),
    };
    let mut value = || match inline.clone().or_else(|| it.next().cloned()) {
      Some(v) => Ok(v),
      None => bail!("{name} needs a value"),
    };

    match name {
      "-h" | "--help" => return Ok(Parsed::Help),
      "-V" | "--version" => return Ok(Parsed::Version),
      "--since" => args.since = Some(parse_date(name, &value()?)?),
      "--until" => args.until = Some(parse_date(name, &value()?)?),
      "--provider" => {
        let v = value()?;
        let Some(x) = Provider::parse(&v) else { bail!("--provider must be claude or codex: {v}") };
        args.provider = Some(x);
      }
      "--by" => {
        let v = value()?;
        let Some(x) = By::parse(&v) else { bail!("--by must be project, group or dir: {v}") };
        args.by = x;
      }
      "--models" => args.models = true,
      "--json" => args.json = true,
      "--all" => args.all = true,
      x if x.starts_with('-') => bail!("unknown option: {x}"),
      x => {
        if cmd.is_none()
          && args.period.is_none()
          && let Some(c) = Cmd::parse(x)
        {
          cmd = Some(c);
          continue;
        }
        let Some(period) = Period::parse(x) else { bail!("unknown command or period: {x}") };
        if args.period.is_some() {
          bail!("unexpected argument: {x}");
        }
        args.period = Some(period);
      }
    }
    if name.starts_with("--") {
      flags.push(name.to_string());
    }
  }

  args.cmd = cmd.unwrap_or(Cmd::Stats);
  let allowed: &[&str] = match args.cmd {
    Cmd::Stats => &["--since", "--until", "--provider", "--json", "--by", "--all"],
    Cmd::Days | Cmd::Weeks | Cmd::Months => {
      &["--since", "--until", "--provider", "--json", "--models"]
    }
    Cmd::Here => &["--provider", "--json"],
    Cmd::UpdatePricing => &[],
  };
  if let Some(x) = flags.iter().find(|x| !allowed.contains(&x.as_str())) {
    bail!("{x} is not an option of `cctrail {}`", args.cmd.name());
  }
  if args.period.is_some() && !args.cmd.has_period() {
    bail!("`cctrail {}` takes no period", args.cmd.name());
  }
  if args.period.is_some() && (args.since.is_some() || args.until.is_some()) {
    bail!("a period cannot be combined with --since or --until");
  }
  if let (Some(since), Some(until)) = (args.since, args.until)
    && since > until
  {
    bail!("--since must not be later than --until");
  }
  Ok(Parsed::Run(args))
}

fn parse_date(name: &str, value: &str) -> Result<Date> {
  match value.parse::<Date>() {
    Ok(x) if value.len() == 10 => Ok(x),
    _ => bail!("{name} must be a date as YYYY-MM-DD: {value}"),
  }
}

// MARK: Commands

pub fn run(args: &Args) -> Result<()> {
  match args.cmd {
    Cmd::Stats => stats(args),
    Cmd::Here => here(args),
    Cmd::Days => usage(args, Granularity::Day),
    Cmd::Weeks => usage(args, Granularity::Week),
    Cmd::Months => usage(args, Granularity::Month),
    Cmd::UpdatePricing => update_pricing(),
  }
}

fn query(args: &Args, range: &DateRange) -> Map<String, Value> {
  let date = |x: Option<Date>| x.map(|x| x.to_string());
  let value = json!({
    "period": args.period.map(Period::name),
    "since": date(range.since),
    "until": date(range.until),
    "provider": args.provider.map(Provider::name),
  });
  match value {
    Value::Object(x) => x,
    _ => unreachable!(),
  }
}

fn print_json(value: &Value) -> Result<()> {
  output::emit(&format!("{value}\n"))
}

fn stats(args: &Args) -> Result<()> {
  let range = DateRange::resolve(args.period, args.since, args.until);
  let (config, path) = match args.by {
    By::Dir => (Config::default(), None),
    _ => (Config::load()?, config::config_path()),
  };
  let mut sessions = sessions(args.provider);
  workdir::fold_sessions(&mut sessions);
  let events = metrics::billable_events(&sessions);
  let stats = metrics::stats_by(&sessions, &events, &range, &config, args.by);
  let total = metrics::combine_stats("total", &stats);
  let unpriced = metrics::unpriced(&sessions, &events, &range);

  if args.json {
    let mut query = query(args, &range);
    query.insert("by".into(), json!(args.by.name()));
    return print_json(&json!({
      "schema_version": 1,
      "command": "stats",
      "query": query,
      "groups": stats.iter().map(|x| output::group_json(x, args.by)).collect::<Vec<_>>(),
      "total": output::total_json(&total, stats.len(), args.by),
      "unpriced_models": output::unpriced_json(&unpriced),
    }));
  }

  let term = Term::detect();
  let mut out = output::stats_text(args.by, &stats, &total, path.as_deref(), args.all, &term);
  out += &output::unpriced_text(&unpriced, pricing::stale(), &term);
  output::emit(&out)
}

fn usage(args: &Args, gran: Granularity) -> Result<()> {
  let range = DateRange::resolve(args.period, args.since, args.until);
  let sessions = sessions(args.provider);
  let events = metrics::billable_events(&sessions);
  let buckets = metrics::usage_by_period(&sessions, &events, gran, &range);
  let mut total = UsageSummary::default();
  buckets.values().for_each(|x| total.add(x));
  let unpriced = metrics::unpriced(&sessions, &events, &range);

  if args.json {
    let periods: Vec<Value> = buckets
      .iter()
      .map(|(bucket, x)| {
        let (start, end) = gran.bounds(*bucket, &range);
        let mut value = Map::new();
        value.insert("start".into(), json!(start.to_string()));
        value.insert("end".into(), json!(end.to_string()));
        value.extend(output::summary_json(x));
        let models = x.models.iter().filter(|_| args.models).map(output::model_json);
        value.insert("models".into(), json!(models.collect::<Vec<_>>()));
        Value::Object(value)
      })
      .collect();
    let mut query = query(args, &range);
    query.insert("granularity".into(), json!(gran.name()));
    query.insert("models".into(), json!(args.models));
    return print_json(&json!({
      "schema_version": 1,
      "command": args.cmd.name(),
      "query": query,
      "periods": periods,
      "total": output::summary_json(&total),
      "unpriced_models": output::unpriced_json(&unpriced),
    }));
  }

  let term = Term::detect();
  let mut out = output::usage_text(gran, &buckets, &range, args.models, &total, &term);
  out += &output::unpriced_text(&unpriced, pricing::stale(), &term);
  output::emit(&out)
}

fn here(args: &Args) -> Result<()> {
  const PERIODS: [(Period, &str); 6] = [
    (Period::Today, "today"),
    (Period::Yesterday, "yesterday"),
    (Period::ThisWeek, "this week"),
    (Period::LastWeek, "last week"),
    (Period::ThisMonth, "this month"),
    (Period::All, "all time"),
  ];

  let config = Config::load()?;
  let cwd = std::env::current_dir()?.to_string_lossy().to_string();
  let mut sessions = sessions(args.provider);
  let folds = workdir::fold_sessions(&mut sessions);
  let workdir = folds.fold(&cwd);
  let project = config.find(&cwd, &workdir);
  let name = config.map(&cwd, &workdir, By::Project);
  let mut cache: HashMap<(String, String), bool> = HashMap::new();
  sessions.retain(|x| {
    let key = (x.cwd.clone(), x.workdir.clone());
    *cache.entry(key).or_insert_with(|| config.map(&x.cwd, &x.workdir, By::Project) == name)
  });
  let events = metrics::billable_events(&sessions);
  let days = metrics::usage_by_period(&sessions, &events, Granularity::Day, &DateRange::default());
  let unpriced = metrics::unpriced(&sessions, &events, &DateRange::default());

  let rows: Vec<(Period, &str, UsageSummary)> = PERIODS
    .iter()
    .map(|(period, label)| {
      let range = DateRange::resolve(Some(*period), None, None);
      let mut summary = UsageSummary::default();
      days.iter().filter(|(day, _)| range.contains_day(**day)).for_each(|(_, x)| summary.add(x));
      (*period, *label, summary)
    })
    .collect();

  if args.json {
    let periods: Vec<Value> = rows
      .iter()
      .map(|(period, _, x)| {
        let mut value = Map::new();
        value.insert("period".into(), json!(period.name()));
        value.extend(output::summary_json(x));
        Value::Object(value)
      })
      .collect();
    return print_json(&json!({
      "schema_version": 1,
      "command": "here",
      "query": {"provider": args.provider.map(Provider::name), "workdir": cwd},
      "project": {
        "group": project.map_or(config::UNMAPPED, |x| &x.group),
        "project": project.map_or(&workdir, |x| &x.name),
      },
      "periods": periods,
      "unpriced_models": output::unpriced_json(&unpriced),
    }));
  }

  let term = Term::detect();
  let title = if project.is_some() { name.as_str() } else { config::UNMAPPED };
  let rows: Vec<(&str, UsageSummary)> = rows.into_iter().map(|(_, label, x)| (label, x)).collect();
  let mut out = output::here_text(title, &workdir, &rows, &term);
  out += &output::unpriced_text(&unpriced, pricing::stale(), &term);
  output::emit(&out)
}

fn update_pricing() -> Result<()> {
  let path = pricing::update()?;
  output::emit(&format!("Updated {}\n", path.display()))
}

/// The sessions, with the price catalog refreshed beside the parsing when it is due.
pub fn sessions(provider: Option<Provider>) -> Vec<Session> {
  let refresh = pricing::refresh();
  let sessions = parse_history(provider);
  pricing::finish(refresh);
  sessions
}

pub fn help() -> String {
  let config = match config::config_path() {
    Some(x) => format!(
      "\nProject mappings: {}. Setup guide: {}\n",
      output::workdir_label(&x.to_string_lossy()),
      output::CONFIG_GUIDE
    ),
    None => String::new(),
  };
  format!(
    "Time, tokens and cost per project from local Claude Code and Codex history.

Usage:
  cctrail                                     the interactive view (on a terminal)
  cctrail [period] [--by project|group|dir] [--all]
                                              statistics per project
  cctrail here                                the current directory's project
  cctrail days|weeks|months [period] [--models]
  cctrail update-pricing                      download the latest model prices

Periods: today, yesterday, this-week, last-week, this-month, last-month, all (default)

Options:
  --since YYYY-MM-DD   first day to include
  --until YYYY-MM-DD   last day to include
  --provider NAME      claude or codex only
  --by NAME            group statistics by project, group or dir
  --all                list every row instead of folding the small ones into `other`
  --models             break each period down by model
  --json               machine-readable output
  -h, --help           show this help
  -V, --version        show the version
{config}"
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  fn run(args: &[&str]) -> Result<Args> {
    let argv: Vec<String> = args.iter().map(|x| x.to_string()).collect();
    match parse(&argv)? {
      Parsed::Run(x) => Ok(x),
      x => panic!("unexpected {x:?}"),
    }
  }

  fn err(args: &[&str]) -> String {
    run(args).unwrap_err().to_string()
  }

  #[test]
  fn parses_commands_and_periods() {
    let x = run(&[]).unwrap();
    assert_eq!((x.cmd, x.period, x.by), (Cmd::Stats, None, By::Project));

    assert!(run(&["--all"]).unwrap().all && !run(&[]).unwrap().all);

    let x = run(&["this-month", "--by", "dir", "--json"]).unwrap();
    assert_eq!(
      (x.cmd, x.period, x.by, x.json),
      (Cmd::Stats, Some(Period::ThisMonth), By::Dir, true)
    );

    let x = run(&["weeks", "last-month", "--models", "--provider=codex"]).unwrap();
    assert_eq!((x.cmd, x.period, x.models), (Cmd::Weeks, Some(Period::LastMonth), true));
    assert_eq!(x.provider, Some(Provider::Codex));

    let x = run(&["--since", "2026-01-01", "days", "--until=2026-01-31"]).unwrap();
    assert_eq!(x.cmd, Cmd::Days);
    assert_eq!(x.since, Some(jiff::civil::date(2026, 1, 1)));
    assert_eq!(x.until, Some(jiff::civil::date(2026, 1, 31)));
  }

  #[test]
  fn rejects_invalid_arguments() {
    assert!(err(&["today", "--since", "2026-01-01"]).contains("cannot be combined"));
    assert!(err(&["--since", "2026-01-02", "--until", "2026-01-01"]).contains("--since must not"));
    assert!(err(&["here", "today"]).contains("takes no period"));
    assert!(err(&["days", "--by", "dir"]).contains("--by is not an option"));
    assert!(err(&["--models"]).contains("--models is not an option"));
    assert!(err(&["days", "--all"]).contains("--all is not an option"));
    assert!(err(&["nope"]).contains("unknown command or period"));
    assert!(err(&["--since", "2026-1-1"]).contains("YYYY-MM-DD"));
    assert!(err(&["--since"]).contains("needs a value"));
    assert!(err(&["--provider", "gemini"]).contains("claude or codex"));
  }

  #[test]
  fn help_and_version_win() {
    let argv = vec!["days".to_string(), "--help".to_string()];
    assert!(matches!(parse(&argv).unwrap(), Parsed::Help));
    let argv = vec!["-V".to_string()];
    assert!(matches!(parse(&argv).unwrap(), Parsed::Version));
  }
}
