// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::home_dir;
use crate::model::{Provider, Speed, Usage};

const BUNDLED: &str = include_str!("pricing.json");

/// Dollars per million tokens. Fields in alphabetical order: the catalog file's key order.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub cache_read: Option<f64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub cache_write: Option<f64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub fast_multiplier: Option<f64>,
  pub input: f64,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub long_context_cache_read: Option<f64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub long_context_cache_write: Option<f64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub long_context_input: Option<f64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub long_context_output: Option<f64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub long_context_threshold: Option<u64>,
  pub output: f64,
}

/// Provider (`anthropic`, `openai`) to model id to price.
pub type Catalog = BTreeMap<String, BTreeMap<String, Price>>;

/// The catalog in use and, when it is the downloaded one, when it was written.
static CATALOG: LazyLock<(Catalog, Option<SystemTime>)> =
  LazyLock::new(|| load(cache_path().ok().as_deref()));

/// A download older than this may lack recent models.
const STALE: Duration = Duration::from_secs(7 * 24 * 3600);

/// Where `update-pricing` keeps the downloaded catalog, which wins over the bundled one.
pub fn cache_path() -> Result<PathBuf> {
  if let Some(x) = env::var_os("XDG_CACHE_HOME").filter(|x| !x.is_empty()) {
    return Ok(PathBuf::from(x).join("cctrail/pricing.json"));
  }
  let Some(home) = home_dir() else { bail!("cannot find pricing cache without HOME") };
  Ok(home.join(".cache/cctrail/pricing.json"))
}

/// The cached catalog with its mtime when it exists and parses, else the bundled one.
fn load(cache: Option<&Path>) -> (Catalog, Option<SystemTime>) {
  if let Some(path) = cache.filter(|x| x.is_file())
    && let Ok(x) = read(path)
  {
    let mtime = std::fs::metadata(path).and_then(|x| x.modified()).ok();
    return (x, Some(mtime.unwrap_or(SystemTime::UNIX_EPOCH)));
  }
  (serde_json::from_str(BUNDLED).expect("bundled pricing.json"), None)
}

/// Whether `update-pricing` may bring new prices: the bundled catalog or an old download is in use.
pub fn stale() -> bool {
  is_stale(CATALOG.1, SystemTime::now())
}

fn is_stale(mtime: Option<SystemTime>, now: SystemTime) -> bool {
  mtime.is_none_or(|x| now.duration_since(x).is_ok_and(|x| x > STALE))
}

fn read(path: &Path) -> Result<Catalog> {
  let data = std::fs::read(path)?;
  serde_json::from_slice(&data)
    .with_context(|| format!("invalid pricing catalog: {}", path.display()))
}

pub fn price(provider: Provider, model: Option<&str>) -> Option<&'static Price> {
  CATALOG.0.get(provider.pricing_key())?.get(model.unwrap_or(""))
}

/// Estimated dollars; 0 for a model the catalog lacks.
pub fn cost(provider: Provider, model: Option<&str>, usage: &Usage, speed: Option<Speed>) -> f64 {
  price(provider, model).map_or(0.0, |x| x.cost(usage, speed))
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

const CATALOG_URL: &str = "https://models.dev/api.json";

/// Downloads the models.dev catalog and writes its prices to `path`, replacing it atomically.
pub fn update(path: &Path) -> Result<()> {
  let mut rep = ureq::get(CATALOG_URL)
    .header("User-Agent", "cctrail")
    .config()
    .timeout_global(Some(std::time::Duration::from_secs(30)))
    .build()
    .call()
    .with_context(|| format!("failed to download {CATALOG_URL}"))?;
  let data = rep.body_mut().with_config().limit(64 << 20).read_to_vec()?;
  let raw: serde_json::Value = serde_json::from_slice(&data)?;
  let catalog = from_models_dev(&raw)?;

  let mut data = serde_json::to_vec_pretty(&catalog)?;
  data.push(b'\n');
  if let Some(dir) = path.parent().filter(|x| !x.as_os_str().is_empty()) {
    std::fs::create_dir_all(dir)?;
  }
  let name = path.file_name().map(|x| x.to_string_lossy()).unwrap_or_default();
  let tmp = path.with_file_name(format!(".{name}.tmp"));
  let res = std::fs::write(&tmp, &data).and_then(|_| std::fs::rename(&tmp, path));
  let _ = std::fs::remove_file(&tmp);
  res.with_context(|| format!("failed to write {}", path.display()))
}

/// Anthropic and OpenAI prices from a models.dev `api.json`.
fn from_models_dev(raw: &serde_json::Value) -> Result<Catalog> {
  let Some(raw) = raw.as_object() else { bail!("models.dev response must be an object") };
  let mut catalog = Catalog::new();
  for provider in ["anthropic", "openai"] {
    let models = raw.get(provider).and_then(|x| x.get("models")).and_then(|x| x.as_object());
    let Some(models) = models else { bail!("models.dev response has no {provider} models") };
    let prices = catalog.entry(provider.to_string()).or_default();
    for (key, model) in models {
      let Some(price) = price_of(model) else { continue };
      let id = model.get("id").and_then(|x| x.as_str()).unwrap_or(key);
      prices.insert(id.to_string(), price);
    }
  }
  Ok(catalog)
}

fn price_of(model: &serde_json::Value) -> Option<Price> {
  let num = |x: Option<&serde_json::Value>| x.and_then(|x| x.as_f64());
  let cost = model.get("cost")?.as_object()?;
  let input = num(cost.get("input"))?;
  let mut price = Price {
    input,
    output: num(cost.get("output"))?,
    cache_read: num(cost.get("cache_read")),
    cache_write: num(cost.get("cache_write")),
    ..Default::default()
  };

  let tiers = cost.get("tiers").and_then(|x| x.as_array()).map_or(&[][..], |x| x.as_slice());
  for tier in tiers {
    let Some(rule) = tier.get("tier").filter(|x| x.is_object()) else { continue };
    if rule.get("type").and_then(|x| x.as_str()) != Some("context") {
      continue;
    }
    let Some(size) = rule.get("size").and_then(|x| x.as_u64()) else { continue };
    price.long_context_threshold = Some(size);
    price.long_context_input = num(tier.get("input"));
    price.long_context_output = num(tier.get("output"));
    price.long_context_cache_read = num(tier.get("cache_read"));
    price.long_context_cache_write = num(tier.get("cache_write"));
    break;
  }

  let fast = model.pointer("/experimental/modes/fast/cost/input");
  if let Some(x) = num(fast).filter(|_| input != 0.0) {
    price.fast_multiplier = Some(x / input);
  }
  Some(price)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reads_models_dev_catalog() {
    let raw = serde_json::json!({
      "anthropic": {"models": {
        "key": {
          "id": "model",
          "cost": {
            "input": 2, "output": 10, "cache_read": 0.2,
            "tiers": [
              {"input": 1, "tier": {"type": "other", "size": 1}},
              {"input": 4, "output": 15, "cache_read": 0.4, "tier": {"type": "context", "size": 200_000}},
            ],
          },
          "experimental": {"modes": {"fast": {"cost": {"input": 6}}}},
        },
        "free": {"cost": {"input": 0, "output": 0, "cache_read": true}},
        "unpriced": {"cost": {"output": 1}},
        "bare": {},
      }},
      "openai": {"models": {}},
    });
    let catalog = from_models_dev(&raw).unwrap();
    let expected = Price {
      input: 2.0,
      output: 10.0,
      cache_read: Some(0.2),
      long_context_threshold: Some(200_000),
      long_context_input: Some(4.0),
      long_context_output: Some(15.0),
      long_context_cache_read: Some(0.4),
      fast_multiplier: Some(3.0),
      ..Default::default()
    };
    assert_eq!(catalog["anthropic"]["model"], expected);
    assert_eq!(catalog["anthropic"]["free"], Price::default());
    assert_eq!(catalog["anthropic"].len(), 2);
    assert!(catalog["openai"].is_empty());

    let json = serde_json::to_string(&catalog["anthropic"]["free"]).unwrap();
    assert_eq!(json, r#"{"input":0.0,"output":0.0}"#);
    assert!(from_models_dev(&serde_json::json!(null)).is_err());
    assert!(from_models_dev(&serde_json::json!({"anthropic": {"models": {}}})).is_err());
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
    let catalog: Catalog = serde_json::from_str(BUNDLED).unwrap();
    assert!(!catalog["anthropic"].is_empty());
    assert!(!catalog["openai"].is_empty());
  }

  #[test]
  fn prefers_valid_cache() {
    let dir = std::env::temp_dir().join(format!("cctrail-pricing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pricing.json");

    std::fs::write(&path, r#"{"openai": {"model": {"input": 2, "output": 10}}}"#).unwrap();
    let (catalog, mtime) = load(Some(&path));
    assert_eq!(
      catalog["openai"]["model"],
      Price { input: 2.0, output: 10.0, ..Default::default() }
    );
    assert!(!catalog.contains_key("anthropic"));
    assert!(!is_stale(mtime, SystemTime::now()));

    let bundled = |path: &Path| {
      let (catalog, mtime) = load(Some(path));
      catalog.contains_key("anthropic") && mtime.is_none()
    };
    std::fs::write(&path, "invalid").unwrap();
    assert!(bundled(&path));
    std::fs::write(&path, r#"{"openai": {"model": {"input": 2, "output": 10, "x": 1}}}"#).unwrap();
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
