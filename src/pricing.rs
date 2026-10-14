// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use jiff::civil::Date;
use serde::Deserialize;

use crate::config::home_dir;
use crate::metrics::local_date;
use crate::model::{Provider, Speed, Ts, Usage};

const BUNDLED: &str = include_str!("pricing.json");

/// Dollars per million tokens from a day on: the models.dev price set on that day.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
  pub since: String,
  /// `since` parsed, filled in by `parse`.
  #[serde(skip)]
  day: Date,
  #[serde(default)]
  pub cache_read: Option<f64>,
  #[serde(default)]
  pub cache_write: Option<f64>,
  #[serde(default)]
  pub fast_multiplier: Option<f64>,
  pub input: f64,
  #[serde(default)]
  pub long_context_cache_read: Option<f64>,
  #[serde(default)]
  pub long_context_cache_write: Option<f64>,
  #[serde(default)]
  pub long_context_input: Option<f64>,
  #[serde(default)]
  pub long_context_output: Option<f64>,
  #[serde(default)]
  pub long_context_threshold: Option<u64>,
  pub output: f64,
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
const STALE: Duration = Duration::from_secs(7 * 24 * 3600);

/// Where the downloaded catalog lives; it wins over the bundled one.
pub fn cache_path() -> Result<PathBuf> {
  if let Some(x) = env::var_os("XDG_CACHE_HOME").filter(|x| !x.is_empty()) {
    return Ok(PathBuf::from(x).join("cctrail/pricing.json"));
  }
  let Some(home) = home_dir() else { bail!("cannot find pricing cache without HOME") };
  Ok(home.join(".cache/cctrail/pricing.json"))
}

/// The cached catalog with its mtime when it exists and parses, else the bundled one.
fn load(cache: Option<&Path>) -> Loaded {
  if let Some(path) = cache.filter(|x| x.is_file())
    && let Ok(x) = std::fs::read(path).map_err(anyhow::Error::from).and_then(|x| parse(&x))
  {
    let mtime = std::fs::metadata(path).and_then(|x| x.modified()).ok();
    return (x, Some(mtime.unwrap_or(SystemTime::UNIX_EPOCH)));
  }
  (parse(BUNDLED.as_bytes()).expect("bundled pricing.json"), None)
}

/// A catalog file: both providers present, every model with dated prices.
fn parse(data: &[u8]) -> Result<Catalog> {
  let mut catalog: Catalog = serde_json::from_slice(data)?;
  for provider in ["anthropic", "openai"] {
    if catalog.get(provider).is_none_or(|x| x.is_empty()) {
      bail!("pricing catalog has no {provider} prices");
    }
  }
  for (model, prices) in catalog.values_mut().flatten() {
    if prices.is_empty() {
      bail!("pricing catalog has no prices for {model}");
    }
    for x in prices.iter_mut() {
      x.day = x.since.parse().with_context(|| format!("{model}: since"))?;
    }
    prices.sort_by_key(|x| x.day);
  }
  Ok(catalog)
}

/// Whether `update-pricing` may bring new prices: the bundled catalog or an old download is in use.
pub fn stale() -> bool {
  is_stale(catalog().1, SystemTime::now())
}

fn is_stale(mtime: Option<SystemTime>, now: SystemTime) -> bool {
  mtime.is_none_or(|x| now.duration_since(x).is_ok_and(|x| x > STALE))
}

/// The price of a model at a moment: the latest set by its local day (the day the reports show
/// it under), the first one before that, the latest for an event without a time.
pub fn price(provider: Provider, model: Option<&str>, ts: Option<Ts>) -> Option<&'static Price> {
  pick(catalog().0.get(provider.pricing_key())?.get(model.unwrap_or(""))?, ts.map(local_date))
}

fn pick(prices: &[Price], day: Option<Date>) -> Option<&Price> {
  let i = day.map_or(prices.len(), |day| prices.partition_point(|x| x.day <= day));
  prices.get(i.saturating_sub(1))
}

/// Estimated dollars at the price of the event's day; 0 for a model the catalog lacks.
pub fn cost(
  provider: Provider,
  model: Option<&str>,
  ts: Option<Ts>,
  usage: &Usage,
  speed: Option<Speed>,
) -> f64 {
  price(provider, model, ts).map_or(0.0, |x| x.cost(usage, speed))
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
// A weekly workflow rebuilds the catalog from the models.dev git history
// (`scripts/pricing-history.py`) and publishes it as the `pricing` release's asset.

const CATALOG_URL: &str =
  "https://github.com/vladkens/cctrail/releases/download/pricing/pricing.json";

/// A failed automatic download waits this long before the next try.
const RETRY: Duration = Duration::from_secs(24 * 3600);

fn download(timeout: Duration) -> Result<Vec<u8>> {
  let mut rep = ureq::get(CATALOG_URL)
    .header("User-Agent", concat!("cctrail/", env!("CARGO_PKG_VERSION")))
    .config()
    .timeout_global(Some(timeout))
    .build()
    .call()
    .with_context(|| format!("failed to download {CATALOG_URL}"))?;
  let data = rep.body_mut().with_config().limit(64 << 20).read_to_vec()?;
  parse(&data).context("downloaded pricing catalog")?;
  Ok(data)
}

/// Writes through a temporary file, so a reader never sees half a catalog.
fn write(path: &Path, data: &[u8]) -> Result<()> {
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

/// Starts downloading the catalog when the cached one is missing or a week old and no attempt
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
  if tried || !is_stale(mtime(&path), now) {
    return None;
  }
  write(&attempt, b"").ok()?;

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

  #[test]
  fn picks_the_price_of_the_day() {
    let data = br#"{"anthropic": {"m": [
      {"since": "2026-03-01", "input": 3, "output": 10},
      {"since": "2026-01-01", "input": 5, "output": 10}
    ]}, "openai": {"n": [{"since": "2026-01-01", "input": 1, "output": 2}]}}"#;
    let catalog = parse(data).unwrap();
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
  fn rejects_incomplete_catalogs() {
    let one = r#"[{"since": "2026-01-01", "input": 1, "output": 2}]"#;
    let ok = format!(r#"{{"anthropic": {{"m": {one}}}, "openai": {{"n": {one}}}}}"#);
    assert!(parse(ok.as_bytes()).is_ok());
    for bad in [
      format!(r#"{{"anthropic": {{"m": {one}}}}}"#),
      format!(r#"{{"anthropic": {{"m": {one}}}, "openai": {{"n": []}}}}"#),
      format!(
        r#"{{"anthropic": {{"m": {one}}}, "openai": {{"n": {}}}}}"#,
        one.replace("2026-01-01", "soon")
      ),
      format!(
        r#"{{"anthropic": {{"m": {one}}}, "openai": {{"n": {}}}}}"#,
        one.replace("\"output\"", "\"x\"")
      ),
      // The flat format of cctrail 0.3.0's first builds.
      r#"{"anthropic": {"m": {"input": 1, "output": 2}}, "openai": {}}"#.to_string(),
    ] {
      assert!(parse(bad.as_bytes()).is_err(), "{bad}");
    }
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
    let catalog = parse(BUNDLED.as_bytes()).unwrap();
    assert!(catalog["anthropic"].contains_key("claude-opus-5-5"));
    assert!(catalog["openai"].contains_key("gpt-6-astra"));
  }

  #[test]
  fn prefers_valid_cache() {
    let dir = std::env::temp_dir().join(format!("cctrail-pricing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pricing.json");

    let one = r#"[{"since": "2026-01-01", "input": 2, "output": 10}]"#;
    std::fs::write(&path, format!(r#"{{"anthropic": {{"a": {one}}}, "openai": {{"m": {one}}}}}"#))
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
    assert!(bundled(&dir.join("missing.json")));
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn suggests_updates_for_bundled_or_old_catalogs() {
    let now = SystemTime::now();
    let day = Duration::from_secs(24 * 3600);
    assert!(is_stale(None, now));
    assert!(is_stale(Some(now - 8 * day), now));
    assert!(!is_stale(Some(now - 6 * day), now));
    assert!(!is_stale(Some(now + day), now));
  }
}
