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

fn tmp(name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("cctrail-cli-{}-{name}", std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

struct Run {
  claude: PathBuf,
  codex: PathBuf,
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
      config,
      cwd: None,
      cache: fixtures().join("missing"),
    }
  }

  fn config(mut self, path: impl AsRef<Path>) -> Self {
    self.config = Some(fixtures().join(path));
    self
  }

  fn exec(&self, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cctrail"));
    cmd.args(args).env("CLAUDE_CONFIG_DIR", &self.claude).env("CODEX_HOME", &self.codex);
    // The user's downloaded prices must not change the numbers.
    cmd.env("XDG_CACHE_HOME", &self.cache).env_remove("COLUMNS");
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
fn reports_configured_projects() {
  let out =
    Run::new("claude/projects", "missing").config("config.toml").stdout(&["--provider", "claude"]);
  assert!(out.lines().any(|x| x.starts_with("oss/demo") && x.contains("427")), "{out}");
  assert!(!out.contains("unmapped"));
}

#[test]
fn reports_unmapped_section_total() {
  let out = Run::new("claude/projects", "missing").stdout(&["--provider", "claude"]);
  assert!(out.lines().any(|x| x.starts_with("@unmapped") && x.contains("427")), "{out}");
  assert!(out.contains("1 unmapped dir · run cctrail --by dir"));
  assert!(has_line(&out, "Project config: ", "empty.toml"), "{out}");
  assert!(!out.lines().any(|x| x.starts_with(REPO)));
}

#[test]
fn explains_missing_project_config() {
  let dir = tmp("xdg");
  let mut run = Run::new("claude/projects", "missing");
  run.config = None;
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_cctrail"));
  cmd.args(["--provider", "claude"]).env("XDG_CONFIG_HOME", &dir).env_remove("CCTRAIL_CONFIG");
  cmd.env("CLAUDE_CONFIG_DIR", &run.claude).env("XDG_CACHE_HOME", fixtures().join("missing"));
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
  std::fs::remove_dir_all(&dir).unwrap();
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

#[test]
fn stats_json_over_all_fixtures() {
  let value = Run::new("", "").config("config.toml").json(&["--json"]);
  assert_eq!(value["command"], "stats");
  assert_eq!(value["query"]["by"], "project");
  assert_eq!(value["groups"].as_array().unwrap().len(), 1);
  let group = &value["groups"][0];
  assert_eq!((&group["group"], &group["project"]), (&json!("oss"), &json!("demo")));
  assert_eq!(group["sessions"], 5);
  assert_eq!(group["usage"]["total"], 962);
  assert_eq!(group["active_seconds"], 1227);
  assert_eq!(value["total"]["projects"], 1);

  let value = Run::new("", "").config("config.toml").json(&["--by", "group", "--json"]);
  assert_eq!(value["groups"][0]["group"], "oss");
  assert_eq!(value["groups"][0]["usage"]["total"], 962);
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
  std::fs::write(run.cache.join("cctrail/pricing.json"), r#"{"anthropic": {}}"#).unwrap();
  let fresh = run.stdout(&["days"]);
  std::fs::remove_dir_all(&dir).unwrap();

  let warning = "Cost excludes 1.5K on unpriced models: claude-from-the-future";
  assert!(out.contains(&format!("{warning} · run cctrail update-pricing")), "{out}");
  assert_eq!(value["unpriced_models"], json!(["claude-from-the-future"]));
  assert!(fresh.contains(&format!("{warning}\n")), "{fresh}");
  assert!(!fresh.contains("update-pricing"));
}

#[test]
fn prints_version() {
  for flag in ["-V", "--version"] {
    let out = Run::new("missing", "missing").stdout(&[flag]);
    assert_eq!(out, format!("cctrail {}\n", env!("CARGO_PKG_VERSION")));
  }
}
