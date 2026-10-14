// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::HashMap;

use crate::model::Session;

/// Folds agent worktrees into their repository by path alone, so it works for deleted ones too.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folds {
  /// The Codex and Claude homes (`same_path`): their `worktrees` name no repository.
  homes: Vec<String>,
}

impl Folds {
  pub fn new(homes: &[String]) -> Self {
    Self { homes: homes.iter().map(|x| same_path(x)).collect() }
  }

  /// The repository a directory belongs to: `<repo>` for `<repo>/.<tool>/worktrees/<name>`,
  /// `<repo>/.worktrees/<name>` and `<repo>-worktrees/<name>` (and below them); else itself.
  pub fn fold(&self, dir: &str) -> String {
    let is_sep = |c: char| c == '/' || c == '\\';
    let mut segs = vec![];
    let mut start = 0;
    for (i, c) in dir.char_indices().filter(|x| is_sep(x.1)) {
      segs.push((start, i));
      start = i + c.len_utf8();
    }
    segs.push((start, dir.len()));

    // The text before segment `k`, without its separator; a worktree needs a name after it.
    let before = |k: usize| &dir[..segs[k].0.saturating_sub(1)];
    let has_name = |x: &str| x.split(is_sep).any(|x| !x.is_empty());
    for (k, &(s, e)) in segs.iter().enumerate().skip(1) {
      if segs.get(k + 1).is_none_or(|x| x.1 == x.0) {
        continue;
      }
      let prev = &dir[segs[k - 1].0..segs[k - 1].1];
      let repo = match &dir[s..e] {
        ".worktrees" => before(k).to_string(),
        "worktrees" if prev.len() > 1 && prev.starts_with('.') => {
          if self.homes.contains(&same_path(&dir[..segs[k - 1].1])) {
            continue;
          }
          before(k - 1).to_string()
        }
        name => match name.strip_suffix("-worktrees") {
          Some(stem) if !stem.is_empty() => dir[..s].to_string() + stem,
          _ => continue,
        },
      };
      if has_name(&repo) {
        return repo;
      }
    }
    dir.to_string()
  }
}

/// Folds every session's directory into its repository, keeping the logged one in `cwd`.
pub fn fold_sessions(sessions: &mut [Session]) -> Folds {
  let folds = Folds::new(&homes());
  // Sessions share a few hundred directories: fold each directory once.
  let mut cache: HashMap<String, String> = HashMap::new();
  for x in sessions.iter_mut() {
    x.cwd = x.workdir.clone();
    x.workdir = cache.entry(x.cwd.clone()).or_insert_with(|| folds.fold(&x.cwd)).clone();
  }
  folds
}

/// The Codex and Claude homes, the configured and the default ones.
fn homes() -> Vec<String> {
  let home = crate::config::home_dir();
  let roots = [
    crate::parser::history_roots("CODEX_HOME", ".codex"),
    crate::parser::history_roots("CLAUDE_CONFIG_DIR", ".claude"),
  ];
  let defaults = [".codex", ".claude"].into_iter().filter_map(|x| Some(home.as_ref()?.join(x)));
  // Logged directories are absolute: a relative override (`CODEX_HOME=./.codex`) must be too.
  let absolute = |x: std::path::PathBuf| std::path::absolute(&x).unwrap_or(x);
  let homes = roots.into_iter().flatten().chain(defaults).map(absolute);
  homes.map(|x| x.to_string_lossy().to_string()).collect()
}

/// A path for comparing, without a trailing separator; on Windows with `/` separators and
/// lowercase, as paths compare there.
fn same_path(path: &str) -> String {
  match cfg!(windows) {
    true => path.replace('\\', "/").trim_end_matches('/').to_lowercase(),
    false => path.trim_end_matches('/').to_string(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn folds_worktrees_by_path() {
    let folds = Folds::new(&["/home/me/.codex".into(), "/home/me/.claude/".into()]);
    let cases = [
      ("/home/me/Code/app/.claude/worktrees/tui", "/home/me/Code/app"),
      ("/home/me/Code/app/.claude/worktrees/tui/src/ui", "/home/me/Code/app"),
      ("/home/me/Code/df/.ralphex/worktrees/market", "/home/me/Code/df"),
      ("/home/me/Code/lalo/.worktrees/natui", "/home/me/Code/lalo"),
      ("/home/me/Code/lalo-worktrees/natui", "/home/me/Code/lalo"),
      ("C:\\Code\\lalo\\.worktrees\\natui", "C:\\Code\\lalo"),
      // The harness homes' own worktrees name no repository.
      ("/home/me/.codex/worktrees/a1b2/app", "/home/me/.codex/worktrees/a1b2/app"),
      ("/home/me/.claude/worktrees/x", "/home/me/.claude/worktrees/x"),
      // No worktree name, no dot directory, no repository name.
      ("/home/me/Code/lalo/.worktrees", "/home/me/Code/lalo/.worktrees"),
      ("/home/me/Code/lalo-worktrees", "/home/me/Code/lalo-worktrees"),
      ("/home/me/Code/worktrees/x", "/home/me/Code/worktrees/x"),
      ("/home/me/Code/-worktrees/x", "/home/me/Code/-worktrees/x"),
      ("/.worktrees/x", "/.worktrees/x"),
      ("/home/me/Code/app", "/home/me/Code/app"),
      ("unknown", "unknown"),
    ];
    for (dir, repo) in cases {
      assert_eq!(folds.fold(dir), repo, "{dir}");
    }
  }

  #[test]
  fn compares_homes_as_paths() {
    let folds = Folds::new(&["C:/Users/Me/.codex/".into()]);
    let dir = "C:\\Users\\me\\.codex\\worktrees\\a1\\repo";
    // Windows compares with either separator and any case: the home's own worktree stays.
    let expected = if cfg!(windows) { dir } else { "C:\\Users\\me" };
    assert_eq!(folds.fold(dir), expected);
    let dir = "C:/Users/Me/.codex/worktrees/a1/repo";
    assert_eq!(folds.fold(dir), dir);
  }

  #[test]
  fn folds_sessions_keeping_the_logged_directory() {
    let mut sessions =
      [Session::new("s", true, "/repo/.worktrees/a", crate::model::Provider::Claude)];
    fold_sessions(&mut sessions);
    let x = &sessions[0];
    assert_eq!((x.workdir.as_str(), x.cwd.as_str()), ("/repo", "/repo/.worktrees/a"));
  }
}
