// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::ops::{Add, AddAssign};

/// Microseconds since the Unix epoch, the precision of the history timestamps.
pub type Ts = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Provider {
  Claude,
  Codex,
  Pi,
}

impl Provider {
  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "claude" => Some(Self::Claude),
      "codex" => Some(Self::Codex),
      "pi" => Some(Self::Pi),
      _ => None,
    }
  }

  pub fn name(self) -> &'static str {
    match self {
      Self::Claude => "claude",
      Self::Codex => "codex",
      Self::Pi => "pi",
    }
  }

  /// The price catalogs its models are in: pi calls the models of any provider.
  pub fn catalogs(self) -> &'static [&'static str] {
    match self {
      Self::Claude => &["anthropic"],
      Self::Codex => &["openai"],
      Self::Pi => &["anthropic", "openai"],
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
  Standard,
  Fast,
}

impl Speed {
  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "fast" | "priority" => Some(Self::Fast),
      "standard" | "default" => Some(Self::Standard),
      _ => None,
    }
  }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Usage {
  pub input: u64,
  pub cache_write_5m: u64,
  pub cache_write_1h: u64,
  pub cache_read: u64,
  pub output: u64,
  pub reasoning: u64,
}

// Sums saturate: history is untrusted, a crafted token count must not wrap or panic.
impl Usage {
  pub fn cache_write(&self) -> u64 {
    self.cache_write_5m.saturating_add(self.cache_write_1h)
  }

  pub fn total(&self) -> u64 {
    [self.cache_write(), self.cache_read, self.output, self.reasoning]
      .into_iter()
      .fold(self.input, u64::saturating_add)
  }
}

impl Add for Usage {
  type Output = Usage;

  fn add(self, x: Usage) -> Usage {
    Usage {
      input: self.input.saturating_add(x.input),
      cache_write_5m: self.cache_write_5m.saturating_add(x.cache_write_5m),
      cache_write_1h: self.cache_write_1h.saturating_add(x.cache_write_1h),
      cache_read: self.cache_read.saturating_add(x.cache_read),
      output: self.output.saturating_add(x.output),
      reasoning: self.reasoning.saturating_add(x.reasoning),
    }
  }
}

impl AddAssign for Usage {
  fn add_assign(&mut self, x: Usage) {
    *self = *self + x;
  }
}

/// A user or assistant event; one with usage is billable unless it failed.
#[derive(Clone, Debug, Default)]
pub struct Event {
  pub ts: Option<Ts>,
  pub usage: Option<Usage>,
  pub model: Option<String>,
  pub speed: Option<Speed>,
  /// The reasoning effort as logged: `low`, `medium`, `high`, `xhigh`, `max`…
  pub effort: Option<String>,
  /// Claude's `(message.id, requestId)`, shared by copies of one API response across files.
  pub identity: Option<(String, Option<String>)>,
  /// The harness's own cost estimate (pi's), used for a model the price catalog lacks.
  pub cost: Option<f64>,
  pub sidechain: bool,
  pub failed: bool,
}

impl Event {
  pub fn activity(ts: Ts) -> Self {
    Self { ts: Some(ts), ..Default::default() }
  }
}

#[derive(Clone, Debug)]
pub struct Session {
  pub id: String,
  /// The id of the main session an agent belongs to, its own id for a main session.
  pub root_id: String,
  pub is_main: bool,
  /// The repository directory the session ran in, worktrees folded into it.
  pub workdir: String,
  /// The working directory as logged.
  pub cwd: String,
  /// The git remote Codex logged for the working directory, normalized; Claude logs none.
  pub remote: Option<String>,
  pub provider: Provider,
  pub events: Vec<Event>,
}

impl Session {
  pub fn new(id: &str, is_main: bool, workdir: &str, provider: Provider) -> Self {
    Self {
      id: id.to_string(),
      root_id: id.to_string(),
      is_main,
      workdir: workdir.to_string(),
      cwd: workdir.to_string(),
      remote: None,
      provider,
      events: vec![],
    }
  }

  pub fn root(&self) -> (Provider, &str) {
    (self.provider, &self.root_id)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn saturates_huge_token_counts() {
    let x = Usage { input: u64::MAX, output: 1, ..Default::default() };
    assert_eq!(x.total(), u64::MAX);
    assert_eq!((x + x).input, u64::MAX);
    assert_eq!((x + x).output, 2);
    let y = Usage { cache_write_5m: u64::MAX, cache_write_1h: 1, ..Default::default() };
    assert_eq!(y.cache_write(), u64::MAX);
  }
}
