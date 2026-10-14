// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

use std::ops::{Add, AddAssign};

/// Microseconds since the Unix epoch, the precision of the history timestamps.
pub type Ts = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Provider {
  Claude,
  Codex,
}

impl Provider {
  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "claude" => Some(Self::Claude),
      "codex" => Some(Self::Codex),
      _ => None,
    }
  }

  pub fn name(self) -> &'static str {
    match self {
      Self::Claude => "claude",
      Self::Codex => "codex",
    }
  }

  pub fn pricing_key(self) -> &'static str {
    match self {
      Self::Claude => "anthropic",
      Self::Codex => "openai",
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
  pub input: u64,
  pub cache_write_5m: u64,
  pub cache_write_1h: u64,
  pub cache_read: u64,
  pub output: u64,
  pub reasoning: u64,
}

impl Usage {
  pub fn cache_write(&self) -> u64 {
    self.cache_write_5m + self.cache_write_1h
  }

  pub fn total(&self) -> u64 {
    self.input + self.cache_write() + self.cache_read + self.output + self.reasoning
  }
}

impl Add for Usage {
  type Output = Usage;

  fn add(self, x: Usage) -> Usage {
    Usage {
      input: self.input + x.input,
      cache_write_5m: self.cache_write_5m + x.cache_write_5m,
      cache_write_1h: self.cache_write_1h + x.cache_write_1h,
      cache_read: self.cache_read + x.cache_read,
      output: self.output + x.output,
      reasoning: self.reasoning + x.reasoning,
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
  /// Claude's `(message.id, requestId)`, shared by copies of one API response across files.
  pub identity: Option<(String, Option<String>)>,
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
  pub workdir: String,
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
      provider,
      events: vec![],
    }
  }

  pub fn root(&self) -> (Provider, &str) {
    (self.provider, &self.root_id)
  }
}
