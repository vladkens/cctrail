// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const UNMAPPED: &str = "@unmapped";

/// What statistics are grouped by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
  Project,
  Group,
  Dir,
}

impl By {
  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "project" => Some(Self::Project),
      "group" => Some(Self::Group),
      "dir" => Some(Self::Dir),
      _ => None,
    }
  }

  pub fn name(self) -> &'static str {
    match self {
      Self::Project => "project",
      Self::Group => "group",
      Self::Dir => "dir",
    }
  }
}

#[derive(Debug, PartialEq)]
pub struct Project {
  pub group: String,
  pub name: String,
  pub roots: Vec<PathBuf>,
}

impl Project {
  pub fn label(&self) -> String {
    format!("{}/{}", self.group, self.name)
  }
}

#[derive(Debug, Default, PartialEq)]
pub struct Config {
  /// Sorted by label.
  pub projects: Vec<Project>,
}

impl Config {
  /// Reads the config file; a missing file is an error only when `CCTRAIL_CONFIG` names it.
  pub fn load() -> Result<Self> {
    let home = home_dir();
    let Some(path) = config_path() else { return Ok(Self::default()) };
    if env::var_os("CCTRAIL_CONFIG").is_some() || path.is_file() {
      return Self::read(&path, home.as_deref());
    }
    Ok(Self::default())
  }

  pub fn read(path: &Path, home: Option<&Path>) -> Result<Self> {
    let text = std::fs::read_to_string(path)
      .with_context(|| format!("failed to read {}", path.display()))?;
    Self::parse(&text, home).with_context(|| format!("failed to parse {}", path.display()))
  }

  pub fn parse(text: &str, home: Option<&Path>) -> Result<Self> {
    let data: toml::Table = toml::from_str(text)?;
    if let Some(key) = data.keys().find(|x| *x != "projects") {
      bail!("unknown field `{key}`");
    }
    let raw = match data.get("projects") {
      None => return Ok(Self::default()),
      Some(toml::Value::Table(x)) => x,
      Some(_) => bail!("projects must be a table"),
    };

    let mut labels: Vec<&String> = raw.keys().collect();
    labels.sort();
    let mut seen: HashMap<PathBuf, &str> = HashMap::new();
    let mut projects = vec![];
    for label in labels {
      let Some((group, name)) = split_project(label) else {
        bail!("project key must have <group>/<project> form: {label}");
      };
      let values = match &raw[label.as_str()] {
        toml::Value::Array(x) if !x.is_empty() => x,
        _ => bail!("project has no paths: {label}"),
      };

      let mut roots = vec![];
      for value in values {
        let Some(value) = value.as_str() else { bail!("project path must be a string: {label}") };
        let root = fold_case(&expand_path(value, home)?);
        if let Some(other) = seen.get(&root) {
          bail!("path belongs to both {other} and {label}: {}", root.display());
        }
        seen.insert(root.clone(), label);
        roots.push(root);
      }
      projects.push(Project { group: group.to_string(), name: name.to_string(), roots });
    }
    Ok(Self { projects })
  }

  /// The project of a working directory: the one with the longest root containing it.
  pub fn find(&self, workdir: &str) -> Option<&Project> {
    let path = fold_case(Path::new(workdir));
    let mut best: Option<(usize, &Project)> = None;
    for project in &self.projects {
      for root in &project.roots {
        let depth = root.components().count();
        if path.starts_with(root) && best.is_none_or(|(x, _)| depth > x) {
          best = Some((depth, project));
        }
      }
    }
    best.map(|(_, x)| x)
  }

  /// The name statistics group a working directory under.
  pub fn map(&self, workdir: &str, by: By) -> String {
    if by == By::Dir {
      return workdir.to_string();
    }
    match (self.find(workdir), by) {
      (Some(x), By::Project) => x.label(),
      (None, By::Project) => workdir.to_string(),
      (Some(x), _) => x.group.clone(),
      (None, _) => UNMAPPED.to_string(),
    }
  }
}

pub fn split_project(value: &str) -> Option<(&str, &str)> {
  let (group, name) = value.split_once('/')?;
  match !group.is_empty() && !name.is_empty() && !name.contains('/') {
    true => Some((group, name)),
    false => None,
  }
}

fn expand_path(value: &str, home: Option<&Path>) -> Result<PathBuf> {
  let path = match value.strip_prefix('~') {
    Some(rest) if rest.is_empty() || rest.starts_with('/') => {
      let Some(home) = home else { bail!("cannot expand ~ without HOME") };
      home.join(rest.trim_start_matches('/'))
    }
    _ => PathBuf::from(value),
  };
  // `has_root`, not `is_absolute`: on Windows `\repo` is a root of the current drive.
  if !path.has_root() {
    bail!("project path must be absolute: {value}");
  }
  Ok(path)
}

/// Windows paths compare case-insensitively, as Python's `Path` does there.
fn fold_case(path: &Path) -> PathBuf {
  match cfg!(windows) {
    true => PathBuf::from(path.to_string_lossy().to_lowercase()),
    false => path.to_path_buf(),
  }
}

pub fn home_dir() -> Option<PathBuf> {
  ["HOME", "USERPROFILE"].iter().filter_map(env::var_os).find(|x| !x.is_empty()).map(PathBuf::from)
}

pub fn config_path() -> Option<PathBuf> {
  if let Some(x) = env::var_os("CCTRAIL_CONFIG") {
    return Some(PathBuf::from(x));
  }
  let root = match env::var_os("XDG_CONFIG_HOME") {
    Some(x) => PathBuf::from(x),
    None => home_dir()?.join(".config"),
  };
  Some(root.join("cctrail/config.toml"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn maps_descendants_by_longest_prefix() {
    let text = r#"
      [projects]
      "oss/tools" = ["~/Code/tools"]
      "try/sandbox" = ["~/Code/tools/sandbox"]
    "#;
    let config = Config::parse(text, Some(Path::new("/home/me"))).unwrap();

    assert_eq!(config.map("/home/me/Code/tools/cli", By::Project), "oss/tools");
    assert_eq!(config.map("/home/me/Code/tools", By::Project), "oss/tools");
    assert_eq!(config.map("/home/me/Code/tools/sandbox/app", By::Project), "try/sandbox");
    assert_eq!(config.map("/home/me/Code/toolsx", By::Project), "/home/me/Code/toolsx");
    assert_eq!(config.map("/home/me/Code/tools/cli", By::Group), "oss");
    assert_eq!(config.map("/other", By::Group), "@unmapped");
    assert_eq!(config.map("/other", By::Project), "/other");
    assert_eq!(config.map("/other", By::Dir), "/other");

    let other_case = config.map("/home/me/code/Tools/cli", By::Project);
    assert_eq!(other_case, if cfg!(windows) { "oss/tools" } else { "/home/me/code/Tools/cli" });
  }

  #[test]
  fn rejects_invalid_config() {
    let cases = [
      ("[projects]\ninvalid = ['/repo']", "<group>/<project>"),
      ("[projects]\n'a/b/c' = ['/repo']", "<group>/<project>"),
      ("[projects]\n'oss/demo' = []", "has no paths"),
      ("[projects]\n'oss/demo' = [1]", "must be a string"),
      ("[projects]\n'oss/demo' = ['relative']", "must be absolute"),
      ("[projects]\n'oss/one' = ['/repo']\n'try/two' = ['/repo/']", "belongs to both"),
      ("projects = 1", "must be a table"),
      ("other = 1", "unknown field `other`"),
      ("[projects", "TOML"),
    ];
    for (text, message) in cases {
      let e = format!("{:#}", Config::parse(text, None).unwrap_err());
      assert!(e.contains(message) || message == "TOML", "{text}: {e}");
    }
    assert!(Config::parse("[projects]\n'a/b' = ['~/x']", None).is_err());
  }

  #[test]
  fn splits_project_labels() {
    assert_eq!(split_project("oss/demo"), Some(("oss", "demo")));
    for x in ["oss", "/repo", "oss/", "/demo", "a/b/c", "/Users/me"] {
      assert_eq!(split_project(x), None, "{x}");
    }
  }
}
