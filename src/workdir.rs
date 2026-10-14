// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::collections::{BTreeSet, HashMap};

use crate::model::{Session, Ts};

/// Folds agent worktrees into their repository by path alone, so it works for deleted ones too,
/// and clones of a repository (directories of the same name) into the one used last.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folds {
  /// The Codex and Claude homes (`same_path`): their `worktrees` name no repository.
  homes: Vec<String>,
  /// A worktree-folded directory (`same_path`) → the clone it folds into.
  clones: HashMap<String, String>,
}

impl Folds {
  pub fn new(homes: &[String]) -> Self {
    Self { homes: homes.iter().map(|x| same_path(x)).collect(), clones: HashMap::new() }
  }

  /// The repository directory a directory belongs to: its worktree's repository, then the clone
  /// that repository folds into.
  pub fn fold(&self, dir: &str) -> String {
    let repo = self.worktree(dir);
    self.clones.get(&same_path(&repo)).cloned().unwrap_or(repo)
  }

  /// The repository a directory belongs to: `<repo>` for `<repo>/.<tool>/worktrees/<name>`,
  /// `<repo>/.worktrees/<name>` and `<repo>-worktrees/<name>` (and below them); else itself.
  fn worktree(&self, dir: &str) -> String {
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
  let mut folds = Folds::new(&homes());
  // Sessions share a few hundred directories: fold each directory once.
  let mut cache: HashMap<String, String> = HashMap::new();
  for x in sessions.iter_mut() {
    x.cwd = x.workdir.clone();
    x.workdir = cache.entry(x.cwd.clone()).or_insert_with(|| folds.worktree(&x.cwd)).clone();
  }

  // One entry per directory however it was spelled (`same_path`), shown as its latest spelling.
  let mut seen: HashMap<String, Dir> = HashMap::new();
  for x in sessions.iter() {
    let last = x.events.iter().filter_map(|e| e.ts).max();
    let dir = seen.entry(same_path(&x.workdir)).or_insert_with(|| Dir {
      path: x.workdir.clone(),
      last: None,
      remotes: BTreeSet::new(),
    });
    if last > dir.last {
      (dir.path, dir.last) = (x.workdir.clone(), last);
    }
    dir.remotes.extend(x.remote.clone());
  }
  let dirs: Vec<Dir> = seen.into_values().collect();
  folds.clones = clones(&dirs, &temp_dirs());
  for x in sessions.iter_mut() {
    if let Some(clone) = folds.clones.get(&same_path(&x.workdir)) {
      x.workdir = clone.clone();
    }
  }
  folds
}

/// A repository directory as the sessions saw it.
struct Dir {
  path: String,
  /// The latest activity in it.
  last: Option<Ts>,
  /// Every git remote Codex logged for it.
  remotes: BTreeSet<String>,
}

/// Clones of one repository: directories of the same name fold into the one with the latest
/// activity. A temp directory, a directory holding other session directories (`~/Code`) and
/// directories Codex saw only with different git remotes never fold together. Keys are `same_path`.
fn clones(dirs: &[Dir], temps: &[String]) -> HashMap<String, String> {
  let keys: Vec<String> = dirs.iter().map(|x| same_path(&x.path)).collect();
  let below =
    |dir: &str, root: &str| dir.len() > root.len() + 1 && dir.starts_with(&format!("{root}/"));
  let mut groups: HashMap<&str, Vec<&Dir>> = HashMap::new();
  for (x, key) in dirs.iter().zip(&keys) {
    let Some((_, name)) = key.rsplit_once('/').filter(|x| !x.1.is_empty()) else { continue };
    let temp = temps.iter().any(|t| key == t || below(key, t));
    if temp || keys.iter().any(|other| below(other, key)) {
      continue;
    }
    groups.entry(name).or_default().push(x);
  }

  let mut map = HashMap::new();
  for mut group in groups.into_values() {
    // The latest first; ties by path, so the result doesn't depend on the order of sessions.
    group.sort_by(|a, b| b.last.cmp(&a.last).then_with(|| a.path.cmp(&b.path)));
    let mut repos: Vec<(&str, BTreeSet<String>)> = vec![];
    for x in group {
      // Unknown remotes match anything; known ones must share at least one.
      let fits =
        |r: &BTreeSet<String>| r.is_empty() || x.remotes.is_empty() || !r.is_disjoint(&x.remotes);
      match repos.iter_mut().find(|r| fits(&r.1)) {
        Some(repo) => {
          repo.1.extend(x.remotes.iter().cloned());
          if repo.0 != x.path {
            map.insert(same_path(&x.path), repo.0.to_string());
          }
        }
        None => repos.push((&x.path, x.remotes.clone())),
      }
    }
  }
  map
}

/// System temp directories (`same_path`): sessions there are throwaway, never a clone.
fn temp_dirs() -> Vec<String> {
  let fixed = ["/tmp", "/var/tmp", "/var/folders"].map(String::from);
  // macOS keeps them under /private too.
  let fixed = fixed.iter().flat_map(|x| [x.clone(), format!("/private{x}")]).collect::<Vec<_>>();
  let env = ["TMPDIR", "TEMP", "TMP"].into_iter().filter_map(std::env::var_os);
  let env = env.map(|x| x.to_string_lossy().to_string()).filter(|x| !x.is_empty());
  fixed.into_iter().chain(env).map(|x| same_path(&x)).collect()
}

/// `host/owner/repo` of a git remote URL: scheme, user, port, `git@host:` and `.git` dropped, the
/// host lowercased. None for a local path or anything else without a host.
pub fn normalize_remote(url: &str) -> Option<String> {
  let url = url.trim();
  let (host, path) = match url.split_once("://") {
    Some((scheme, _)) if scheme.eq_ignore_ascii_case("file") => return None,
    Some((_, rest)) => {
      let (authority, path) = rest.split_once('/')?;
      let host = authority.rsplit_once('@').map_or(authority, |x| x.1);
      // A bracketed IPv6 host keeps its colons; otherwise `:` starts the port.
      let host = match host.strip_prefix('[').and_then(|x| x.split_once(']')) {
        Some((ip, _)) => &host[..ip.len() + 2],
        None => host.split_once(':').map_or(host, |x| x.0),
      };
      (host, path)
    }
    // scp-like `user@host:owner/repo`; one letter before `:` is a Windows drive.
    None => {
      let (authority, path) = url.split_once(':')?;
      if authority.contains('/') || authority.len() < 2 {
        return None;
      }
      (authority.rsplit_once('@').map_or(authority, |x| x.1), path)
    }
  };
  let path = path.trim_matches('/');
  let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
  match host.is_empty() || path.is_empty() {
    true => None,
    false => Some(format!("{}/{path}", host.to_lowercase())),
  }
}

/// The Codex and Claude homes, the configured and the default ones.
fn homes() -> Vec<String> {
  let home = crate::config::home_dir();
  let roots = [
    crate::parser::history_roots("CODEX_HOME", ".codex"),
    crate::parser::history_roots("CLAUDE_CONFIG_DIR", ".claude"),
  ];
  let defaults = [".codex", ".claude"].into_iter().filter_map(|x| Some(home.as_ref()?.join(x)));
  // Logged directories are absolute and resolved: a relative override (`CODEX_HOME=./.codex`) or a
  // profile linked elsewhere is compared by both its absolute and its real path.
  let both = |x: std::path::PathBuf| {
    let real = std::fs::canonicalize(&x).ok();
    [Some(std::path::absolute(&x).unwrap_or(x)), real].into_iter().flatten()
  };
  let homes = roots.into_iter().flatten().chain(defaults).flat_map(both);
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

  fn dir(path: &str, last: Ts, remote: Option<&str>) -> Dir {
    Dir {
      path: path.into(),
      last: Some(last),
      remotes: remote.map(String::from).into_iter().collect(),
    }
  }

  #[test]
  fn folds_clones_by_name_into_the_latest() {
    let temps = ["/tmp".to_string()];
    let dirs = [
      dir("/h/Code/macmon", 30, None),
      dir("/h/Code/pub/macmon", 20, Some("github.com/me/macmon")),
      dir("/Volumes/Data/h/Code/pub/macmon", 10, None),
      dir("/h/Code/other", 5, None),
    ];
    let map = clones(&dirs, &temps);
    assert_eq!(
      map.get(&same_path("/h/Code/pub/macmon")).map(String::as_str),
      Some("/h/Code/macmon")
    );
    assert_eq!(
      map.get(&same_path("/Volumes/Data/h/Code/pub/macmon")).map(String::as_str),
      Some("/h/Code/macmon")
    );
    assert_eq!(map.len(), 2);
  }

  #[test]
  fn keeps_temp_parents_and_other_remotes_apart() {
    let temps = ["/tmp".to_string(), "/private/var/folders".to_string()];
    let dirs = [
      // Temp directories never fold, not even with a same-named repository.
      dir("/tmp/x/run-1", 10, None),
      dir("/private/var/folders/ab/T/run-1", 20, None),
      dir("/h/run-1", 5, None),
      // A directory holding other session directories is no clone.
      dir("/h/Code", 10, None),
      dir("/h/Code/app", 10, None),
      dir("/h/old/Code", 30, None),
      // Different remotes stay apart; one without a remote joins the latest.
      dir("/h/vpn", 30, Some("github.com/me/vpn")),
      dir("/h/priv/vpn", 20, Some("github.com/me/other-vpn")),
      dir("/h/old/vpn", 10, None),
    ];
    let map = clones(&dirs, &temps);
    assert_eq!(map.get(&same_path("/h/old/vpn")).map(String::as_str), Some("/h/vpn"));
    assert_eq!(map.len(), 1, "{map:?}");
  }

  #[test]
  fn folds_worktrees_then_clones() {
    let mut folds = Folds::new(&[]);
    folds.clones.insert("/a/app".into(), "/b/app".into());
    assert_eq!(folds.fold("/a/app/.worktrees/x"), "/b/app");
    assert_eq!(folds.fold("/a/app"), "/b/app");
    assert_eq!(folds.fold("/c/app"), "/c/app");
  }

  #[test]
  fn normalizes_remotes() {
    let repo = Some("github.com/vladkens/macmon".to_string());
    for url in [
      "git@github.com:vladkens/macmon.git",
      "https://github.com/vladkens/macmon",
      "https://user:token@GitHub.com/vladkens/macmon.git/",
      "ssh://git@github.com:22/vladkens/macmon.git",
    ] {
      assert_eq!(normalize_remote(url), repo, "{url}");
    }
    for url in ["", "/srv/git/repo.git", "file:///srv/repo.git", "C:\\repo", "https://host"] {
      assert_eq!(normalize_remote(url), None, "{url}");
    }
    let some = |x: &str| Some(x.to_string());
    assert_eq!(
      normalize_remote("ssh://git@[2001:db8::1]:22/team/app.git"),
      some("[2001:db8::1]/team/app")
    );
    assert_ne!(
      normalize_remote("ssh://git@[2001:db8::1]/team/app.git"),
      normalize_remote("ssh://git@[2001:db8::2]/team/app.git")
    );
    assert_eq!(normalize_remote("git@host:/srv/git/app.git"), some("host/srv/git/app"));
  }

  #[test]
  fn folds_clones_sharing_any_remote() {
    let dir = |path: &str, last: Ts, remotes: &[&str]| Dir {
      path: path.into(),
      last: Some(last),
      remotes: remotes.iter().map(|x| x.to_string()).collect(),
    };
    // /a/app saw remotes A and B, /b/app only A: one repository whichever order.
    let dirs = [dir("/a/app", 2, &["B", "A"]), dir("/b/app", 1, &["A"]), dir("/c/app", 0, &["C"])];
    let map = clones(&dirs, &[]);
    assert_eq!(map.get(&same_path("/b/app")).map(String::as_str), Some("/a/app"));
    assert_eq!(map.get(&same_path("/c/app")), None);
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
