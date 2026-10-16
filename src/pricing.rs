// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use jiff::civil::Date;

use crate::config::home_dir;
use crate::metrics::local_date;
use crate::model::{Event, Provider, Speed, Usage};

const BUNDLED: &str = include_str!("pricing.tsv");

/// Dollars per million tokens from a day on: the models.dev price set on that day.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Price {
  pub since: Date,
  pub input: f64,
  pub output: f64,
  pub cache_read: Option<f64>,
  pub cache_write: Option<f64>,
  pub long_context_threshold: Option<u64>,
  pub long_context_input: Option<f64>,
  pub long_context_output: Option<f64>,
  pub long_context_cache_read: Option<f64>,
  pub long_context_cache_write: Option<f64>,
  pub fast_multiplier: Option<f64>,
}

/// Provider (`anthropic`, `openai`) to model id to its prices, oldest first.
pub type Catalog = BTreeMap<String, BTreeMap<String, Vec<Price>>>;

/// The catalog in use and, when it is the downloaded one, when it was written.
type Loaded = (Catalog, Option<SystemTime>);

static CATALOG: OnceLock<Loaded> = OnceLock::new();

fn catalog() -> &'static Loaded {
  CATALOG.get_or_init(|| load(cache_path().ok().as_deref()))
}

/// A download older than this may lack recent models or price changes.
const STALE: Duration = Duration::from_secs(24 * 3600);

/// Where the downloaded catalog lives; it wins over the bundled one.
pub fn cache_path() -> Result<PathBuf> {
  if let Some(x) = env::var_os("XDG_CACHE_HOME").filter(|x| !x.is_empty()) {
    return Ok(PathBuf::from(x).join("cctrail/pricing.tsv"));
  }
  let Some(home) = home_dir() else { bail!("cannot find pricing cache without HOME") };
  Ok(home.join(".cache/cctrail/pricing.tsv"))
}

/// The cached catalog with its mtime when it exists and parses, else the bundled one.
fn load(cache: Option<&Path>) -> Loaded {
  if let Some(path) = cache.filter(|x| x.is_file())
    && let Ok(x) =
      std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|x| parse(&x))
  {
    let mtime = std::fs::metadata(path).and_then(|x| x.modified()).ok();
    return (x, Some(mtime.unwrap_or(SystemTime::UNIX_EPOCH)));
  }
  (parse(BUNDLED).expect("bundled pricing.tsv"), None)
}

/// The catalog's columns, in order (`scripts/pricing-history.py` writes them).
const COLUMNS: [&str; 13] = [
  "provider",
  "model",
  "since",
  "input",
  "output",
  "cache_read",
  "cache_write",
  "long_context_threshold",
  "long_context_input",
  "long_context_output",
  "long_context_cache_read",
  "long_context_cache_write",
  "fast_multiplier",
];

/// A catalog file: the header, then one row per model and date; both providers present.
fn parse(text: &str) -> Result<Catalog> {
  let mut lines = text.lines().enumerate().filter(|(_, x)| !x.is_empty());
  match lines.next() {
    Some((_, x)) if x.split('\t').eq(COLUMNS) => {}
    _ => bail!("pricing catalog: line 1 must be the header {}", COLUMNS.join(" ")),
  }

  let mut catalog = Catalog::new();
  for (i, line) in lines {
    let row = parse_row(line).with_context(|| format!("pricing catalog: line {}", i + 1))?;
    let (provider, model, price) = row;
    catalog.entry(provider.into()).or_default().entry(model.into()).or_default().push(price);
  }
  for provider in ["anthropic", "openai"] {
    if catalog.get(provider).is_none_or(|x| x.is_empty()) {
      bail!("pricing catalog has no {provider} prices");
    }
  }
  catalog.values_mut().flat_map(|x| x.values_mut()).for_each(|x| x.sort_by_key(|x| x.since));
  Ok(catalog)
}

fn parse_row(line: &str) -> Result<(&str, &str, Price)> {
  let cells: Vec<&str> = line.split('\t').collect();
  if cells.len() != COLUMNS.len() {
    bail!("{} cells, expected {}", cells.len(), COLUMNS.len());
  }
  let num = |i: usize| -> Result<Option<f64>> {
    match cells[i] {
      "" => Ok(None),
      x => x.parse().map(Some).with_context(|| format!("{}: not a number: {x}", COLUMNS[i])),
    }
  };
  let required = |i: usize| num(i)?.with_context(|| format!("{} is required", COLUMNS[i]));
  let (provider, model) = (cells[0], cells[1]);
  if provider.is_empty() || model.is_empty() {
    bail!("provider and model are required");
  }
  let threshold = match cells[7] {
    "" => None,
    x => Some(x.parse().with_context(|| format!("long_context_threshold: not a count: {x}"))?),
  };
  let price = Price {
    since: cells[2].parse().with_context(|| format!("since: not a date: {}", cells[2]))?,
    input: required(3)?,
    output: required(4)?,
    cache_read: num(5)?,
    cache_write: num(6)?,
    long_context_threshold: threshold,
    long_context_input: num(8)?,
    long_context_output: num(9)?,
    long_context_cache_read: num(10)?,
    long_context_cache_write: num(11)?,
    fast_multiplier: num(12)?,
  };
  Ok((provider, model, price))
}

/// Whether `update-pricing` may bring new prices: the bundled catalog or an old download is in use.
pub fn stale() -> bool {
  is_stale(catalog().1, SystemTime::now())
}

fn is_stale(mtime: Option<SystemTime>, now: SystemTime) -> bool {
  mtime.is_none_or(|x| now.duration_since(x).is_ok_and(|x| x > STALE))
}

/// Models without a price of their own, billed as another model at its price on the same day.
const ALIASES: &[(&str, &str)] = &[
  // Codex's approval reviewer: not on models.dev, it runs on the small model.
  ("codex-auto-review", "gpt-6-luna"),
];

/// Estimated dollars of an event at the price of its day; for a model the catalog lacks, the
/// harness's own estimate, else 0.
pub fn cost(provider: Provider, event: &Event) -> f64 {
  estimate(&catalog().0, provider, event).unwrap_or(0.0)
}

/// Whether an event has a cost: its model is in the catalog or the harness estimated it.
pub fn priced(provider: Provider, event: &Event) -> bool {
  estimate(&catalog().0, provider, event).is_some()
}

fn estimate(catalog: &Catalog, provider: Provider, e: &Event) -> Option<f64> {
  if e.summed && e.cost.is_some() {
    return e.cost;
  }
  let day = e.ts.map(local_date);
  match lookup(catalog, provider, e.model.as_deref().unwrap_or(""), day) {
    Some(x) => Some(x.cost(&e.usage.unwrap_or_default(), e.speed)),
    None => e.cost,
  }
}

/// The price of a model at a moment: the latest set by its local day (the day the reports show
/// it under), the first one before that, the latest for an event without a time.
fn lookup<'a>(
  catalog: &'a Catalog,
  p: Provider,
  model: &str,
  day: Option<Date>,
) -> Option<&'a Price> {
  let model = ALIASES.iter().find(|x| x.0 == model).map_or(model, |x| x.1);
  p.catalogs().iter().find_map(|x| pick(catalog.get(*x)?.get(model)?, day))
}

fn pick(prices: &[Price], day: Option<Date>) -> Option<&Price> {
  let i = day.map_or(prices.len(), |day| prices.partition_point(|x| x.since <= day));
  prices.get(i.saturating_sub(1))
}

impl Price {
  pub fn cost(&self, usage: &Usage, speed: Option<Speed>) -> f64 {
    let long = self.long_context_threshold.is_some_and(|x| {
      usage.input.saturating_add(usage.cache_write()).saturating_add(usage.cache_read) > x
    });
    let pick = |long_price: Option<f64>| if long { long_price } else { None };
    let input = pick(self.long_context_input).unwrap_or(self.input);
    let output = pick(self.long_context_output).unwrap_or(self.output);
    let read = pick(self.long_context_cache_read).or(self.cache_read).unwrap_or(input);
    let write = pick(self.long_context_cache_write).or(self.cache_write).unwrap_or(input * 1.25);

    // The order of the terms is the Python version's: costs match it to the last bit.
    let dollars = usage.input as f64 * input
      + usage.cache_write_5m as f64 * write
      + usage.cache_write_1h as f64 * input * 2.0
      + usage.cache_read as f64 * read
      + usage.output.saturating_add(usage.reasoning) as f64 * output;
    let multiplier = match speed {
      Some(Speed::Fast) => self.fast_multiplier.unwrap_or(1.0),
      _ => 1.0,
    };
    dollars * multiplier / 1_000_000.0
  }
}

// MARK: Updates
//
// A daily workflow rebuilds the catalog from the models.dev git history
// (`scripts/pricing-history.py`) and publishes it as the `pricing` release's asset.

const CATALOG_URL: &str =
  "https://github.com/vladkens/cctrail/releases/download/pricing/pricing.tsv";

/// A failed automatic download waits this long before the next try.
const RETRY: Duration = Duration::from_secs(24 * 3600);

fn download(timeout: Duration) -> Result<String> {
  let mut rep = ureq::get(CATALOG_URL)
    .header("User-Agent", concat!("cctrail/", env!("CARGO_PKG_VERSION")))
    .config()
    .timeout_global(Some(timeout))
    .build()
    .call()
    .with_context(|| format!("failed to download {CATALOG_URL}"))?;
  let data = rep.body_mut().with_config().limit(64 << 20).read_to_string()?;
  parse(&data).context("downloaded pricing catalog")?;
  Ok(data)
}

/// Writes through a temporary file, so a reader never sees half a catalog.
fn write(path: &Path, data: &str) -> Result<()> {
  if let Some(dir) = path.parent().filter(|x| !x.as_os_str().is_empty()) {
    std::fs::create_dir_all(dir)?;
  }
  let name = path.file_name().map(|x| x.to_string_lossy()).unwrap_or_default();
  let tmp = path.with_file_name(format!(".{name}.tmp"));
  let res = std::fs::write(&tmp, data).and_then(|_| std::fs::rename(&tmp, path));
  let _ = std::fs::remove_file(&tmp);
  res.with_context(|| format!("failed to write {}", path.display()))
}

/// `cctrail update-pricing`: downloads the catalog into the cache now.
pub fn update() -> Result<PathBuf> {
  let path = cache_path()?;
  write(&path, &download(Duration::from_secs(30))?)?;
  Ok(path)
}

/// A download running beside the history parsing, started by `refresh`.
pub struct Refresh(Receiver<Catalog>);

/// Starts downloading the catalog when the cached one is missing or a day old and no attempt
/// failed in the last day. `CCTRAIL_NO_UPDATE=1` turns it off. Never fails: the bundled or cached
/// prices stay in use.
pub fn refresh() -> Option<Refresh> {
  if env::var_os("CCTRAIL_NO_UPDATE").is_some_and(|x| !x.is_empty() && x != "0") {
    return None;
  }
  let path = cache_path().ok()?;
  let now = SystemTime::now();
  let mtime = |x: &Path| std::fs::metadata(x).and_then(|x| x.modified()).ok();
  let attempt = path.with_file_name(".pricing.attempt");
  let tried = mtime(&attempt).is_some_and(|x| now.duration_since(x).is_ok_and(|x| x < RETRY));
  // A cached file that doesn't parse counts as missing, so a broken download gets replaced.
  let valid = std::fs::read_to_string(&path).is_ok_and(|x| parse(&x).is_ok());
  if tried || (valid && !is_stale(mtime(&path), now)) {
    return None;
  }
  write(&attempt, "").ok()?;

  let (tx, rx) = mpsc::channel();
  std::thread::spawn(move || {
    let Ok(data) = download(Duration::from_secs(10)) else { return };
    if write(&path, &data).is_ok() {
      let _ = std::fs::remove_file(&attempt);
    }
    if let Ok(x) = parse(&data) {
      let _ = tx.send(x);
    }
  });
  Some(Refresh(rx))
}

/// Uses the refreshed catalog when it arrives within a moment; a slower one is still saved for
/// the next run if the process lives long enough.
pub fn finish(refresh: Option<Refresh>) {
  let Some(Refresh(rx)) = refresh else { return };
  if let Ok(x) = rx.recv_timeout(Duration::from_millis(1500)) {
    let _ = CATALOG.set((x, Some(SystemTime::now())));
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const HEADER: &str = "provider\tmodel\tsince\tinput\toutput\tcache_read\tcache_write\t\
    long_context_threshold\tlong_context_input\tlong_context_output\tlong_context_cache_read\t\
    long_context_cache_write\tfast_multiplier";

  /// A catalog from rows of `provider model since input output`, the other cells empty.
  fn tsv(rows: &[&str]) -> String {
    let rows = rows.iter().map(|x| format!("{}\t\t\t\t\t\t\t\t", x.replace(' ', "\t")));
    std::iter::once(HEADER.to_string()).chain(rows).collect::<Vec<_>>().join("\n")
  }

  #[test]
  fn bills_aliases_as_their_model() {
    // The bundled catalog, not the cached one a test run may find.
    let catalog = parse(BUNDLED).unwrap();
    let day = Some(jiff::civil::date(2026, 10, 1));
    let luna = lookup(&catalog, Provider::Codex, "gpt-6-luna", day).unwrap();
    assert_eq!(lookup(&catalog, Provider::Codex, "codex-auto-review", day), Some(luna));
    assert_eq!(lookup(&catalog, Provider::Claude, "codex-auto-review", day), None);
  }

  #[test]
  fn finds_the_models_pi_calls() {
    let catalog = parse(BUNDLED).unwrap();
    let day = Some(jiff::civil::date(2026, 10, 1));
    // pi's in either catalog, Claude Code's and Codex's only in their own.
    let opus = lookup(&catalog, Provider::Claude, "claude-opus-4-6", day).unwrap();
    assert_eq!(lookup(&catalog, Provider::Pi, "claude-opus-4-6", day), Some(opus));
    assert_eq!(lookup(&catalog, Provider::Codex, "claude-opus-4-6", day), None);
    let gpt = lookup(&catalog, Provider::Codex, "gpt-5.4", day).unwrap();
    assert_eq!(lookup(&catalog, Provider::Pi, "gpt-5.4", day), Some(gpt));
    assert_eq!(lookup(&catalog, Provider::Claude, "gpt-5.4", day), None);
  }

  #[test]
  fn falls_back_to_the_harness_estimate() {
    let catalog = parse(&tsv(&["anthropic claude-m 2026-01-01 1 2", "openai n 2026-01-01 1 2"]));
    let catalog = catalog.unwrap();
    let event = |model: &str, cost| Event {
      model: Some(model.into()),
      usage: Some(Usage { input: 1_000_000, ..Default::default() }),
      cost,
      ..Default::default()
    };
    // The catalog's price wins over pi's; without either the event has no price.
    assert_eq!(estimate(&catalog, Provider::Pi, &event("claude-m", Some(9.0))), Some(1.0));
    assert_eq!(estimate(&catalog, Provider::Pi, &event("glm-5", Some(0.25))), Some(0.25));
    assert_eq!(estimate(&catalog, Provider::Pi, &event("glm-5", None)), None);
    // Usage summed over several requests at the harness's estimate, else the catalog's.
    let summed = |cost| Event { summed: true, ..event("claude-m", cost) };
    assert_eq!(estimate(&catalog, Provider::Pi, &summed(Some(0.5))), Some(0.5));
    assert_eq!(estimate(&catalog, Provider::Pi, &summed(None)), Some(1.0));
  }

  #[test]
  fn picks_the_price_of_the_day() {
    let text = tsv(&[
      "anthropic m 2026-03-01 3 10",
      "anthropic m 2026-01-01 5 10",
      "openai n 2026-01-01 1 2",
    ]);
    let catalog = parse(&text).unwrap();
    let prices = &catalog["anthropic"]["m"];
    let input = |x: Option<&str>| pick(prices, x.map(|x| x.parse().unwrap())).unwrap().input;

    assert_eq!(input(Some("2025-06-01")), 5.0); // before the first: the first
    assert_eq!(input(Some("2026-02-28")), 5.0);
    assert_eq!(input(Some("2026-03-01")), 3.0);
    assert_eq!(input(Some("2027-01-01")), 3.0); // after the last: the last
    assert_eq!(input(None), 3.0);
    assert!(pick(&[], None).is_none());
  }

  #[test]
  fn reads_every_column() {
    let text = format!(
      "{HEADER}\nanthropic\tm\t2026-01-01\t2.5\t15\t0.25\t3.125\t272000\t5\t22.5\t0.5\t6.25\t\
       1.7499999999999998\nopenai\tn\t2026-01-01\t1\t2\t\t\t\t\t\t\t\t\n"
    );
    let catalog = parse(&text).unwrap();
    let expected = Price {
      since: "2026-01-01".parse().unwrap(),
      input: 2.5,
      output: 15.0,
      cache_read: Some(0.25),
      cache_write: Some(3.125),
      long_context_threshold: Some(272_000),
      long_context_input: Some(5.0),
      long_context_output: Some(22.5),
      long_context_cache_read: Some(0.5),
      long_context_cache_write: Some(6.25),
      fast_multiplier: Some(1.7499999999999998),
    };
    assert_eq!(catalog["anthropic"]["m"], [expected]);
    assert_eq!(catalog["openai"]["n"][0].cache_read, None);
  }

  #[test]
  fn rejects_incomplete_catalogs() {
    let ok = ["anthropic m 2026-01-01 1 2", "openai n 2026-01-01 1 2"];
    assert!(parse(&tsv(&ok)).is_ok());
    let err = |rows: &[&str]| parse(&tsv(rows)).unwrap_err().to_string();
    let chain = |rows: &[&str]| format!("{:#}", parse(&tsv(rows)).unwrap_err());

    assert!(err(&ok[..1]).contains("no openai prices"));
    assert!(chain(&[ok[0], "openai n soon 1 2"]).contains("line 3: since: not a date: soon"));
    assert!(chain(&[ok[0], "openai n 2026-01-01 x 2"]).contains("line 3: input: not a number"));
    assert!(chain(&[ok[0], "openai n 2026-01-01  2"]).contains("line 3: input is required"));
    let short = format!("{HEADER}\nanthropic\tm\t2026-01-01\t1\t2");
    assert!(format!("{:#}", parse(&short).unwrap_err()).contains("line 2: 5 cells, expected 13"));
    assert!(parse("provider\tmodel\n").unwrap_err().to_string().contains("line 1 must be"));
    // The JSON catalog of the first dated builds is not a catalog any more.
    assert!(parse(r#"{"anthropic": {}, "openai": {}}"#).is_err());
  }

  #[test]
  fn prices_reasoning_fast_and_long_context() {
    let price = Price {
      input: 2.5,
      output: 15.0,
      cache_read: Some(0.25),
      long_context_threshold: Some(272_000),
      long_context_input: Some(5.0),
      long_context_output: Some(22.5),
      long_context_cache_read: Some(0.5),
      fast_multiplier: Some(2.0),
      ..Default::default()
    };
    let usage = Usage { input: 272_001, output: 500, reasoning: 500, ..Default::default() };
    let standard = price.cost(&usage, Some(Speed::Standard));
    assert_eq!(standard, 1.382505);
    assert_eq!(price.cost(&usage, Some(Speed::Fast)), standard * 2.0);

    let usage =
      Usage { input: 1000, cache_write_5m: 100, cache_write_1h: 10, cache_read: 1, ..usage };
    assert_eq!(price.cost(&usage, None), (2500.0 + 312.5 + 50.0 + 0.25 + 15000.0) / 1e6);

    let huge = Usage { input: u64::MAX, cache_read: 1, output: u64::MAX, reasoning: 1, ..usage };
    assert!(price.cost(&huge, None).is_finite());
  }

  #[test]
  fn bundled_catalog_parses() {
    let catalog = parse(BUNDLED).unwrap();
    assert!(catalog["anthropic"].contains_key("claude-opus-5-5"));

    // GPT-5.6 Sol's price cut, as models.dev recorded it on 2026-08-25.
    let sol = &catalog["openai"]["gpt-5.6-sol"];
    let input = |day: &str| pick(sol, Some(day.parse().unwrap())).unwrap().input;
    assert_eq!((input("2026-08-01"), input("2026-09-01")), (5.0, 4.0));
  }

  #[test]
  fn prefers_valid_cache() {
    let dir = std::env::temp_dir().join(format!("cctrail-pricing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pricing.tsv");

    std::fs::write(&path, tsv(&["anthropic a 2026-01-01 2 10", "openai m 2026-01-01 2 10"]))
      .unwrap();
    let (catalog, mtime) = load(Some(&path));
    assert_eq!((catalog["openai"]["m"][0].input, catalog["anthropic"].len()), (2.0, 1));
    assert!(!is_stale(mtime, SystemTime::now()));

    let bundled = |path: &Path| {
      let (catalog, mtime) = load(Some(path));
      catalog["anthropic"].len() > 1 && mtime.is_none()
    };
    std::fs::write(&path, "invalid").unwrap();
    assert!(bundled(&path));
    assert!(bundled(&dir.join("missing.tsv")));
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn suggests_updates_for_bundled_or_old_catalogs() {
    let now = SystemTime::now();
    let day = Duration::from_secs(24 * 3600);
    assert!(is_stale(None, now));
    let hour = Duration::from_secs(3600);
    assert!(is_stale(Some(now - 25 * hour), now));
    assert!(!is_stale(Some(now - 23 * hour), now));
    assert!(!is_stale(Some(now + day), now));
  }
}
