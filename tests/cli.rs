// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use serde_json::{Value, json};

/// The working directory in the fixtures; `/repo` is not absolute on Windows, where a project
/// can't hold it.
const REPO: &str = if cfg!(windows) { "C:/repo" } else { "/repo" };

/// The fixtures, on Windows a copy with `REPO` for `/repo`.
fn fixtures() -> PathBuf {
  static DIR: OnceLock<PathBuf> = OnceLock::new();
  let dir = DIR.get_or_init(|| {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    if !cfg!(windows) {
      return src;
    }
    let dst = std::env::temp_dir().join(format!("cctrail-fixtures-{}", std::process::id()));
    copy_tree(&src, &dst);
    dst
  });
  dir.clone()
}

fn copy_tree(src: &Path, dst: &Path) {
  std::fs::create_dir_all(dst).unwrap();
  for entry in std::fs::read_dir(src).unwrap().map(Result::unwrap) {
    let to = dst.join(entry.file_name());
    if entry.file_type().unwrap().is_dir() {
      copy_tree(&entry.path(), &to);
      continue;
    }
    let text = std::fs::read_to_string(entry.path()).unwrap();
    std::fs::write(to, text.replace("\"/repo", &format!("\"{REPO}"))).unwrap();
  }
}

/// A line with this start and end: paths in between are `~/…` or absolute, native separators.
fn has_line(out: &str, start: &str, end: &str) -> bool {
  out.lines().any(|x| x.starts_with(start) && x.ends_with(end))
}

/// A price catalog from `provider model since input output` rows, the other cells empty.
fn catalog(rows: &[&str]) -> String {
  let header = "provider\tmodel\tsince\tinput\toutput\tcache_read\tcache_write\t\
    long_context_threshold\tlong_context_input\tlong_context_output\tlong_context_cache_read\t\
    long_context_cache_write\tfast_multiplier";
  let rows = rows.iter().map(|x| format!("{x}\t\t\t\t\t\t\t\t\n"));
  format!("{header}\n{}", rows.collect::<String>())
}

fn tmp(name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("cctrail-cli-{}-{name}", std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

struct Run {
  claude: PathBuf,
  codex: PathBuf,
  pi: PathBuf,
  fx: PathBuf,
  config: Option<PathBuf>,
  cwd: Option<PathBuf>,
  cache: PathBuf,
}

impl Run {
  fn new(claude: &str, codex: &str) -> Self {
    let config = Some(fixtures().join("empty.toml"));
    Self {
      claude: fixtures().join(claude),
      codex: fixtures().join(codex),
      pi: fixtures().join("missing"),
      fx: fixtures().join("missing"),
      config,
      cwd: None,
      cache: fixtures().join("missing"),
    }
  }

  fn pi(mut self, path: &str) -> Self {
    self.pi = fixtures().join(path);
    self
  }

  fn fx(mut self, path: &str) -> Self {
    self.fx = fixtures().join(path);
    self
  }

  fn config(mut self, path: impl AsRef<Path>) -> Self {
    self.config = Some(fixtures().join(path));
    self
  }

  fn exec(&self, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cctrail"));
    cmd.args(args).env("CLAUDE_CONFIG_DIR", &self.claude).env("CODEX_HOME", &self.codex);
    cmd.env("PI_CODING_AGENT_DIR", &self.pi).env_remove("PI_CODING_AGENT_SESSION_DIR");
    cmd.env("FX_HOME", &self.fx);
    // The user's downloaded prices must not change the numbers, and tests never download.
    cmd.env("XDG_CACHE_HOME", &self.cache).env("CCTRAIL_NO_UPDATE", "1").env_remove("COLUMNS");
    match &self.config {
      Some(x) => cmd.env("CCTRAIL_CONFIG", x),
      None => cmd.env_remove("CCTRAIL_CONFIG"),
    };
    if let Some(x) = &self.cwd {
      cmd.current_dir(x);
    }
    cmd.output().unwrap()
  }

  fn stdout(&self, args: &[&str]) -> String {
    let out = self.exec(args);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
  }

  fn json(&self, args: &[&str]) -> Value {
    serde_json::from_str(&self.stdout(args)).unwrap()
  }
}

fn total_tokens(stdout: &str) -> String {
  let line = stdout.lines().find(|x| x.starts_with("total")).unwrap();
  line.split_whitespace().nth(3).unwrap().to_string()
}

#[test]
fn reports_claude_usage_by_day() {
  let out = Run::new("claude/projects", "missing").stdout(&["days", "--provider", "claude"]);
  assert!(out.contains("2026-01-01"));
  assert!(!out.contains("2026-01-02"));
  assert_eq!(total_tokens(&out), "427");
}

#[test]
fn groups_by_dir_without_config() {
  let mut run = Run::new("claude/projects", "missing").config("missing.toml");
  let value = run.json(&["--by", "dir", "--provider", "claude", "--json"]);
  assert_eq!(value["query"]["by"], "dir");
  assert_eq!(value["groups"][0]["workdir"], REPO);
  assert_eq!(value["groups"][0]["usage"]["total"], 427);

  run.config = None;
  assert!(run.stdout(&["--by", "dir"]).contains(REPO));
}

#[test]
fn folds_worktrees_and_clones_into_the_repository() {
  let run = Run::new("", "").config("missing.toml");
  let value = run.json(&["--by", "dir", "--json"]);
  let groups = value["groups"].as_array().unwrap();
  assert_eq!(groups.len(), 1, "{value}");
  // The worktree folds by its path, the older clone `/old/repo` by its name.
  let worktree = format!("{REPO}/.claude/worktrees/flag");
  assert_eq!(groups[0]["workdir"], REPO);
  assert_eq!(groups[0]["paths"], json!(["/old/repo", REPO, worktree]));
  assert_eq!(groups[0]["usage"]["total"], 992);

  // `--all` lists every row; nothing is small here either way.
  let out = run.stdout(&["--by", "dir", "--all"]);
  assert!(out.contains(REPO) && !out.contains("other"), "{out}");
}

#[test]
fn maps_worktrees_with_the_repository_unless_configured() {
  let dir = tmp("folds");
  let config = dir.join("config.toml");
  let worktree = format!("{REPO}/.claude/worktrees/flag");
  let text = format!("[projects]\n\"oss/demo\" = [{REPO:?}]\n\"try/flag\" = [{worktree:?}]\n");
  std::fs::write(&config, text).unwrap();
  let value = Run::new("", "").config(&config).json(&["--json"]);
  std::fs::remove_dir_all(&dir).unwrap();
  let totals: Vec<(&Value, &Value)> = value["groups"]
    .as_array()
    .unwrap()
    .iter()
    .map(|x| (&x["project"], &x["usage"]["total"]))
    .collect();
  // The clone `/old/repo` maps with the repository it folds into.
  assert_eq!(totals, [(&json!("demo"), &json!(977)), (&json!("flag"), &json!(15))], "{value}");
}

#[test]
fn reports_configured_projects() {
  let out =
    Run::new("claude/projects", "missing").config("config.toml").stdout(&["--provider", "claude"]);
  assert!(out.lines().any(|x| x.starts_with("oss/demo") && x.contains("427")), "{out}");
  assert!(!out.contains("unmapped"));
}

#[test]
fn lists_unconfigured_dirs_beside_projects() {
  let out = Run::new("claude/projects", "missing").stdout(&["--provider", "claude"]);
  assert!(out.lines().any(|x| x.starts_with(REPO) && x.contains("427")), "{out}");
  assert!(!out.contains("@unmapped") && !out.contains("other ("), "{out}");
  assert!(has_line(&out, "Project config: ", "empty.toml"), "{out}");
}

#[test]
fn explains_missing_project_config() {
  let dir = tmp("xdg");
  let mut run = Run::new("claude/projects", "missing");
  run.config = None;
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_cctrail"));
  cmd.args(["--provider", "claude"]).env("XDG_CONFIG_HOME", &dir).env_remove("CCTRAIL_CONFIG");
  cmd.env("CLAUDE_CONFIG_DIR", &run.claude).env("XDG_CACHE_HOME", fixtures().join("missing"));
  cmd.env("CCTRAIL_NO_UPDATE", "1");
  let out = String::from_utf8(cmd.output().unwrap().stdout).unwrap();
  std::fs::remove_dir_all(&dir).unwrap();

  assert!(out.contains("No project config found"), "{out}");
  assert!(has_line(&out, "Create: ", "config.toml"), "{out}");
  assert!(out.contains("Setup guide: https://github.com/vladkens/cctrail#projects"));
}

#[test]
fn explicit_config_must_exist() {
  let out = Run::new("claude/projects", "missing").config("missing.toml").exec(&["here"]);
  assert_eq!(out.status.code(), Some(2));
  assert!(String::from_utf8_lossy(&out.stderr).contains("failed to read"));
}

#[test]
fn unmapped_json_is_always_complete() {
  let value = Run::new("claude/projects", "missing").json(&["--provider", "claude", "--json"]);
  let group = &value["groups"][0];
  assert_eq!((&group["group"], &group["project"]), (&json!("@unmapped"), &json!(REPO)));
  assert_eq!(group["usage"]["total"], 427);
  assert_eq!(group["active_seconds"], 310);
  assert_eq!(value["unpriced_models"], json!([]));
}

#[test]
fn help_explains_project_config() {
  let out = Run::new("missing", "missing").stdout(&["--help"]);
  assert!(
    has_line(
      &out,
      "Project mappings: ",
      "empty.toml. Setup guide: https://github.com/vladkens/cctrail#projects"
    ),
    "{out}"
  );
  assert!(out.contains("https://github.com/vladkens/cctrail#projects"));
  assert!(out.contains("update-pricing"));
}

#[test]
fn reports_current_project() {
  let dir = tmp("here");
  let config = dir.join("config.toml");
  // macOS: the temp dir is behind a symlink. Windows: `canonicalize` gives a `\\?\` path.
  let cwd = if cfg!(windows) { dir.clone() } else { dir.canonicalize().unwrap() };
  std::fs::write(&config, format!("[projects]\n\"oss/demo\" = [{REPO:?}, {cwd:?}]\n")).unwrap();
  let mut run = Run::new("claude/projects", "missing").config(&config);
  run.cwd = Some(cwd);

  let out = run.stdout(&["here", "--provider", "claude"]);
  assert!(out.contains("oss/demo · "), "{out}");
  let header: Vec<&str> = out.lines().nth(1).unwrap().split_whitespace().collect();
  assert_eq!(header, ["Period", "Total", "tokens", "Cost", "Time"]);
  assert!(
    out.lines().any(|x| x.starts_with("all time") && x.contains("0h 05m") && x.contains("427"))
  );

  let value = run.json(&["here", "--provider", "claude", "--json"]);
  // A worktree of the project reports the project.
  let worktree = run.cwd.as_ref().unwrap().join(".worktrees/x");
  std::fs::create_dir_all(&worktree).unwrap();
  run.cwd = Some(worktree);
  let inside = run.json(&["here", "--provider", "claude", "--json"]);
  std::fs::remove_dir_all(&dir).unwrap();
  assert_eq!(inside["project"], value["project"]);
  assert_eq!(value["project"], json!({"group": "oss", "project": "demo"}));
  let periods: Vec<&str> =
    value["periods"].as_array().unwrap().iter().map(|x| x["period"].as_str().unwrap()).collect();
  assert_eq!(periods, ["today", "yesterday", "this-week", "last-week", "this-month", "all"]);
  assert_eq!(value["periods"][5]["usage"]["total"], 427);
  assert_eq!(value["periods"][5]["active_seconds"], 310);
}

#[test]
fn rejects_reversed_date_range() {
  let out = Run::new("missing", "missing").exec(&[
    "days",
    "--since",
    "2026-01-02",
    "--until",
    "2026-01-01",
  ]);
  assert_eq!(out.status.code(), Some(2));
  assert!(String::from_utf8_lossy(&out.stderr).contains("--since must not be later"));
}

#[test]
fn usage_json_with_models() {
  let value = Run::new("claude/projects", "missing").json(&[
    "days",
    "--provider",
    "claude",
    "--models",
    "--json",
  ]);
  let query = json!({
    "period": null, "since": null, "until": null, "provider": "claude", "granularity": "day", "models": true,
  });
  assert_eq!(value["query"], query);
  let period = &value["periods"][0];
  assert_eq!(period["usage"]["total"], 427);
  assert_eq!(period["cost_usd"], 0.0005495);
  assert_eq!(period["active_seconds"], 310);
  let models: Vec<&Value> =
    period["models"].as_array().unwrap().iter().map(|x| &x["model"]).collect();
  assert_eq!(models, [&json!("claude-sonnet-5"), &json!("claude-haiku-4-5")]);
}

#[test]
fn codex_usage_json() {
  let value =
    Run::new("missing", "codex").json(&["days", "--provider", "codex", "--models", "--json"]);
  assert_eq!(value["total"]["usage"]["total"], 455);
  assert_eq!(value["total"]["active_seconds"], 615);
  assert_eq!(value["total"]["cost_usd"], 0.001401625);
  let totals: Vec<&Value> = value["periods"][0]["models"]
    .as_array()
    .unwrap()
    .iter()
    .map(|x| &x["usage"]["total"])
    .collect();
  assert_eq!(totals, [&json!(435), &json!(20)]);
}

fn total_of(run: &Run, args: &[&str]) -> Value {
  run.json(args)["total"]["usage"]["total"].clone()
}

#[test]
fn pi_usage_json() {
  let run = Run::new("claude/projects", "missing").pi("pi");
  let value = run.json(&["days", "--provider", "pi", "--models", "--json"]);
  assert_eq!(value["query"]["provider"], "pi");
  // A fork's copies of its source's entries count once.
  let total = &value["total"];
  assert_eq!(
    (&total["usage"]["total"], &total["usage"]["reasoning"]),
    (&json!(11_230), &json!(100))
  );
  assert_eq!(total["cost_usd"], 0.29532375);
  let mut models = vec![];
  for x in value["periods"].as_array().unwrap().iter().flat_map(|x| x["models"].as_array().unwrap())
  {
    models.push((x["provider"].clone(), x["model"].clone(), x["cost_usd"].clone()));
  }
  models.sort_by_key(|x| x.1.to_string());
  // Bedrock's Claude by its own name at the catalog's price, `openai/gpt-5` through a router as
  // `gpt-5`, a model outside the catalog at pi's own price.
  let expected = [
    ("claude-sonnet-4-5-20250929", 0.020775),
    ("glm-5", 0.25),
    ("gpt-5", 0.00454875),
    ("unknown", 0.02),
  ];
  let expected: Vec<_> = expected.map(|(m, c)| (json!("pi"), json!(m), json!(c))).into();
  assert_eq!(models, expected);

  // Every provider by default, one with --provider.
  assert_eq!(total_of(&run, &["--json"]), 11_230 + 427);
  assert_eq!(total_of(&run, &["--provider", "claude", "--json"]), 427);

  // An absolute `sessionDir` in pi's settings holds sessions too.
  let mut run = Run::new("missing", "missing");
  run.pi = tmp("pi-agent");
  let settings = json!({"sessionDir": fixtures().join("pi/sessions")});
  // Written with a byte order mark, as some Windows editors save.
  std::fs::write(run.pi.join("settings.json"), format!("\u{feff}{settings}")).unwrap();
  assert_eq!(total_of(&run, &["--json"]), 11_230);
  std::fs::remove_dir_all(&run.pi).unwrap();
}

#[test]
fn fx_usage_json() {
  let run = Run::new("claude/projects", "missing").fx("fx");
  let value = run.json(&["days", "--provider", "fx", "--models", "--json"]);
  assert_eq!(value["query"]["provider"], "fx");
  // Each session's totals, an agent's in its main session's, spread over the agents' activity.
  assert_eq!(value["total"]["usage"]["total"], 3475);
  let periods = value["periods"].as_array().unwrap();
  let days: Vec<_> =
    periods.iter().map(|x| (x["start"].clone(), x["usage"]["total"].clone())).collect();
  assert_eq!(days, [(json!("2026-10-05"), json!(2555)), (json!("2026-10-07"), json!(920))]);
  let mut models = vec![];
  for x in periods.iter().flat_map(|x| x["models"].as_array().unwrap()) {
    models.push((x["model"].clone(), x["speed"].clone(), x["cost_usd"].clone()));
  }
  models.sort_by_key(|x| x.0.to_string());
  // `codex/…` and a gateway's `anthropic/…` as the catalog names them, at Fast mode's rate when it
  // was on; a model outside the catalog at fx's own price.
  let expected = [
    ("claude-sonnet-4-5", Value::Null, 0.00675),
    ("gpt-5.6-luna", json!("fast"), 0.000032),
    ("gpt-5.6-luna", json!("fast"), 0.000032),
    ("gpt-6-astra", json!("fast"), 0.01318),
    ("gpt-6-astra", json!("fast"), 0.02602),
    ("grok-4", Value::Null, 0.5),
  ];
  let expected: Vec<_> = expected.map(|(m, s, c)| (json!(m), s, json!(c))).into();
  assert_eq!(models, expected);

  // Projects by the main session's directory, with its agent's activity elsewhere.
  let value = run.json(&["--provider", "fx", "--json"]);
  let rows = value["groups"].as_array().unwrap().iter().map(|x| {
    let name = x["project"].as_str().unwrap().to_string();
    (name, x["sessions"].clone(), x["active_seconds"].clone(), x["usage"]["total"].clone())
  });
  let rows: Vec<_> = rows.collect();
  assert_eq!(rows[0], (REPO.to_string(), json!(1), json!(1200), json!(1410)));
  assert!(rows[1].0.ends_with("other"));
  assert_eq!((&rows[1].1, &rows[1].2, &rows[1].3), (&json!(1), &json!(360), &json!(2065)));
  assert_eq!(total_of(&run, &["--json"]), 3475 + 427);
}

#[test]
fn fx_usage_on_every_active_day() {
  // Two requests over two agent moments on one day and ten on another: one on each day, so a
  // report of the first keeps the session and its time.
  let run = Run::new("missing", "missing").fx("fx_days");
  let value = run.json(&["--since", "2026-10-05", "--until", "2026-10-05", "--json"]);
  let row = &value["groups"][0];
  assert_eq!((&row["sessions"], &row["active_seconds"]), (&json!(1), &json!(360)));
  assert_eq!(row["usage"]["total"], 550);
  let value = run.json(&["days", "--json"]);
  let days: Vec<_> =
    value["periods"].as_array().unwrap().iter().map(|x| &x["usage"]["total"]).collect();
  assert_eq!(days, [&json!(550), &json!(550)]);
}

#[test]
fn stats_json_over_all_fixtures() {
  let value = Run::new("", "").config("config.toml").json(&["--json"]);
  assert_eq!(value["command"], "stats");
  assert_eq!(value["query"]["by"], "project");
  assert_eq!(value["groups"].as_array().unwrap().len(), 1);
  let group = &value["groups"][0];
  assert_eq!((&group["group"], &group["project"]), (&json!("oss"), &json!("demo")));
  assert_eq!(group["sessions"], 7);
  assert_eq!(group["usage"]["total"], 992);
  assert_eq!(group["active_seconds"], 1837);
  assert_eq!(value["total"]["projects"], 1);

  let value = Run::new("", "").config("config.toml").json(&["--by", "group", "--json"]);
  assert_eq!(value["groups"][0]["group"], "oss");
  assert_eq!(value["groups"][0]["usage"]["total"], 992);
  assert_eq!(value["total"]["groups"], 1);
}

#[test]
fn months_clip_to_custom_range_without_models() {
  let args =
    ["months", "--provider", "claude", "--since", "2026-01-01", "--until", "2026-01-20", "--json"];
  let value = Run::new("claude/projects", "missing").json(&args);
  assert_eq!(value["command"], "months");
  assert_eq!(value["query"]["granularity"], "month");
  assert_eq!(value["periods"][0]["start"], "2026-01-01");
  assert_eq!(value["periods"][0]["end"], "2026-01-20");
  assert_eq!(value["periods"][0]["models"], json!([]));
}

#[test]
fn warns_about_unpriced_models() {
  let dir = tmp("unpriced");
  let line = r#"{"type": "assistant", "timestamp": "2026-01-01T00:00:00Z", "cwd": "/repo", "message": {"id": "m", "model": "claude-from-the-future", "usage": {"input_tokens": 1500, "output_tokens": 10}}}"#;
  std::fs::write(dir.join("s.jsonl"), line).unwrap();
  let mut run = Run::new("missing", "missing");
  run.claude = dir.clone();

  let out = run.stdout(&["days"]);
  let value = run.json(&["--json"]);

  // A fresh download lacks the model too: updating won't help.
  run.cache = dir.join("cache");
  std::fs::create_dir_all(run.cache.join("cctrail")).unwrap();
  let catalog = catalog(&["anthropic\ta\t2026-01-01\t1\t2", "openai\tb\t2026-01-01\t1\t2"]);
  std::fs::write(run.cache.join("cctrail/pricing.tsv"), catalog).unwrap();
  let fresh = run.stdout(&["days"]);
  std::fs::remove_dir_all(&dir).unwrap();

  let warning = "Cost excludes 1.5K on unpriced models: claude-from-the-future";
  assert!(out.contains(&format!("{warning} · run cctrail update-pricing")), "{out}");
  assert_eq!(value["unpriced_models"], json!(["claude-from-the-future"]));
  assert!(fresh.contains(&format!("{warning}\n")), "{fresh}");
  assert!(!fresh.contains("update-pricing"));
}

#[test]
fn prices_each_day_at_its_rate() {
  let dir = tmp("dated");
  let line = |day: &str, id: &str| {
    format!(
      r#"{{"type": "assistant", "timestamp": "{day}T12:00:00Z", "cwd": "/repo", "message": {{"id": "{id}", "model": "m", "usage": {{"input_tokens": 1000000}}}}}}"#
    )
  };
  // 20:00 UTC on 2026-01-31 is 2026-02-01 in Tokyo: the day the report shows, and its price.
  let late = line("2026-01-31", "c").replace("T12:00", "T20:00");
  let lines = [line("2026-01-10", "a"), line("2026-02-10", "b"), late].join("\n");
  std::fs::write(dir.join("s.jsonl"), lines).unwrap();
  std::fs::create_dir_all(dir.join("cache/cctrail")).unwrap();
  let catalog = catalog(&[
    "anthropic\tm\t2026-01-01\t5\t25",
    "anthropic\tm\t2026-02-01\t3\t15",
    "openai\tn\t2026-01-01\t1\t2",
  ]);
  std::fs::write(dir.join("cache/cctrail/pricing.tsv"), catalog).unwrap();
  let mut run = Run::new("missing", "missing");
  run.claude = dir.clone();
  run.cache = dir.join("cache");

  // Days in a given time zone, with their costs.
  let days = |tz: &str| {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cctrail"));
    cmd.args(["days", "--json"]).env("TZ", tz).env("CCTRAIL_NO_UPDATE", "1");
    cmd.env("CLAUDE_CONFIG_DIR", &run.claude).env("CODEX_HOME", &run.codex);
    cmd.env("PI_CODING_AGENT_DIR", &run.pi).env_remove("PI_CODING_AGENT_SESSION_DIR");
    cmd
      .env("FX_HOME", &run.fx)
      .env("XDG_CACHE_HOME", &run.cache)
      .env("CCTRAIL_CONFIG", fixtures().join("empty.toml"));
    let value: Value = serde_json::from_slice(&cmd.output().unwrap().stdout).unwrap();
    value["periods"]
      .as_array()
      .unwrap()
      .iter()
      .map(|x| (x["start"].as_str().unwrap().to_string(), x["cost_usd"].as_f64().unwrap()))
      .collect::<Vec<_>>()
  };
  let utc = days("UTC");
  let tokyo = days("Asia/Tokyo");
  std::fs::remove_dir_all(&dir).unwrap();

  let day = |d: &str, cost: f64| (d.to_string(), cost);
  assert_eq!(utc, [day("2026-01-10", 5.0), day("2026-01-31", 5.0), day("2026-02-10", 3.0)]);
  assert_eq!(tokyo, [day("2026-01-10", 5.0), day("2026-02-01", 3.0), day("2026-02-10", 3.0)]);
}

#[test]
fn prints_version() {
  for flag in ["-V", "--version"] {
    let out = Run::new("missing", "missing").stdout(&[flag]);
    assert_eq!(out, format!("cctrail {}\n", env!("CARGO_PKG_VERSION")));
  }
}
