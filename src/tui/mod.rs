// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

//! The interactive view: `cctrail` without arguments on a terminal. The history is parsed once;
//! every switch after that is in memory.

mod data;
mod draw;

use anyhow::Result;
pub use data::{Data, Row, Span};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::config::Config;
use crate::model::Provider;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
  Projects,
  Models,
  Timeline,
}

/// What shares, sorting and bars follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
  Time,
  Tokens,
  Cost,
}

impl Metric {
  pub fn label(self) -> &'static str {
    ["time", "tokens", "cost"][self as usize]
  }

  /// The row's value; time falls back to tokens where it doesn't split (models).
  pub fn of(self, x: &Row) -> f64 {
    match (self, x.active) {
      (Self::Time, Some(a)) => a as f64,
      (Self::Cost, _) => x.cost,
      _ => x.tokens as f64,
    }
  }
}

/// A drill-down: the opened row and what it shows.
pub struct Detail {
  pub row: Row,
  pub rows: Vec<Row>,
  /// The project's metric per day (or week), oldest first.
  pub days: Vec<Row>,
  pub paths: Vec<String>,
}

pub struct App {
  pub data: Data,
  pub span: Span,
  pub prov: Option<Provider>,
  pub metric: Metric,
  pub tab: Tab,
  sel: [usize; 3],
  pub open: Option<Detail>,
  /// The selected row of the drill-down.
  pub osel: usize,
  pub help: bool,
  pub filter: String,
  pub typing: bool,
  /// The rows of the current tab, sorted and filtered, and the period's total.
  pub list: Vec<Row>,
  pub total: Row,
  /// Tokens on models without a price in the period: their cost counts as 0.
  pub unpriced: Vec<(String, u64)>,
  pub quit: bool,
}

impl App {
  pub fn new(data: Data) -> Self {
    let mut app = Self {
      data,
      span: Span::Month,
      prov: None,
      metric: Metric::Time,
      tab: Tab::Projects,
      sel: [0; 3],
      open: None,
      osel: 0,
      help: false,
      filter: String::new(),
      typing: false,
      list: vec![],
      total: Row::default(),
      unpriced: vec![],
      quit: false,
    };
    app.refresh();
    app
  }

  pub fn sel(&self) -> usize {
    self.sel[self.tab as usize]
  }

  fn refresh(&mut self) {
    let (projects, total) = self.data.projects(self.span, self.prov, false);
    self.total = total;
    self.unpriced = self.data.unpriced(self.span, self.prov);
    let mut list = match self.tab {
      Tab::Projects => projects,
      Tab::Models => self.data.models(self.span, self.prov, None),
      Tab::Timeline => self.data.timeline(self.span, self.prov, None),
    };
    if self.tab != Tab::Timeline {
      sort(&mut list, self.metric);
    }
    let needle = self.filter.to_lowercase();
    list.retain(|x| x.label.to_lowercase().contains(&needle));
    let sel = &mut self.sel[self.tab as usize];
    *sel = (*sel).min(list.len().saturating_sub(1));
    self.list = list;
    // An open drill-down follows the period and provider: its row as it is now, or closed.
    if let Some(open) = &self.open {
      let row = self.list.iter().find(|x| x.key == open.row.key).cloned();
      self.open = row.map(|x| self.detail(x));
      self.osel = self.osel.min(self.open.as_ref().map_or(0, |x| x.rows.len().saturating_sub(1)));
    }
  }

  fn detail(&self, row: Row) -> Detail {
    let (span, prov) = (self.span, self.prov);
    let mut rows = match self.tab {
      Tab::Projects => self.data.models(span, prov, Some(&row.key)),
      Tab::Models => self.data.model_projects(span, prov, &row.key),
      Tab::Timeline => self.data.bucket_projects(span, prov, row.day.unwrap_or(self.data.today)),
    };
    sort(&mut rows, self.metric);
    let project = self.tab == Tab::Projects;
    Detail {
      days: if project { self.data.timeline(span, prov, Some(&row.key)) } else { vec![] },
      paths: if project { self.data.paths(&row.key) } else { vec![] },
      rows,
      row,
    }
  }

  pub fn key(&mut self, key: KeyEvent) {
    if key.kind != KeyEventKind::Press {
      return;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
      return self.quit = true;
    }
    if self.typing {
      match key.code {
        KeyCode::Char(c) => self.filter.push(c),
        KeyCode::Backspace => _ = self.filter.pop(),
        KeyCode::Esc => (self.filter.clear(), self.typing = false).1,
        KeyCode::Enter => self.typing = false,
        _ => return,
      }
      return self.refresh();
    }
    let len = self.list.len();
    let olen = self.open.as_ref().map_or(0, |x| x.rows.len());
    let sel = &mut self.sel[self.tab as usize];
    match key.code {
      // Moves and toggles that change no data return without rebuilding the rows.
      KeyCode::Char('q') => return self.quit = true,
      KeyCode::Char('?') => return self.help = !self.help,
      KeyCode::Esc if self.help => return self.help = false,
      KeyCode::Esc if self.open.is_some() => return self.open = None,
      KeyCode::Esc if self.filter.is_empty() => return,
      KeyCode::Esc => self.filter.clear(),
      KeyCode::Char(c @ '1'..='3') => {
        self.tab = [Tab::Projects, Tab::Models, Tab::Timeline][c as usize - '1' as usize];
        (self.open, self.help) = (None, false);
      }
      KeyCode::Left => self.span = Span::ALL[(self.span as usize).saturating_sub(1)],
      KeyCode::Right => self.span = Span::ALL[(self.span as usize + 1).min(Span::ALL.len() - 1)],
      KeyCode::Char('m') => {
        self.metric = [Metric::Tokens, Metric::Cost, Metric::Time][self.metric as usize]
      }
      KeyCode::Char('p') => {
        self.prov = match self.prov {
          None => Some(Provider::Claude),
          Some(Provider::Claude) => Some(Provider::Codex),
          Some(Provider::Codex) => None,
        }
      }
      KeyCode::Char('/') => return self.typing = true,
      KeyCode::Up if self.open.is_some() => return self.osel = self.osel.saturating_sub(1),
      KeyCode::Down if self.open.is_some() => {
        return self.osel = (self.osel + 1).min(olen.saturating_sub(1));
      }
      KeyCode::Up => return *sel = sel.saturating_sub(1),
      KeyCode::Down => return *sel = (*sel + 1).min(len.saturating_sub(1)),
      KeyCode::Enter if self.open.is_none() => {
        // `other` and empty days open nothing.
        if let Some(row) = self
          .list
          .get(*sel)
          .filter(|x| !x.key.is_empty() && (x.tokens > 0 || x.active.unwrap_or(0) > 0))
        {
          self.open = Some(self.detail(row.clone()));
          self.osel = 0;
        }
        return;
      }
      _ => return,
    }
    self.refresh();
  }
}

/// Rows by the metric, most first, ties by name; `other` stays last whatever the metric.
fn sort(rows: &mut [Row], m: Metric) {
  rows.sort_by(|a, b| {
    let key = |x: &Row| (x.key.is_empty(), -m.of(x));
    key(a).partial_cmp(&key(b)).unwrap().then_with(|| a.label.cmp(&b.label))
  });
}

/// Runs the interface until `q`: a loading frame while the history is parsed, then the app.
pub fn run() -> Result<()> {
  let mut term = ratatui::init();
  let res = (|| -> Result<()> {
    term.draw(draw::loading)?;
    let config = Config::load()?;
    let sessions = crate::cli::sessions(None);
    let today = crate::metrics::local_date(jiff::Timestamp::now().as_microsecond());
    let mut app = App::new(Data::new(sessions, config, today));
    while !app.quit {
      term.draw(|f| draw::draw(f, &app))?;
      if let Event::Key(key) = event::read()? {
        app.key(key);
      }
    }
    Ok(())
  })();
  ratatui::restore();
  res
}

#[cfg(test)]
mod tests {
  use ratatui::Terminal;
  use ratatui::backend::TestBackend;

  use super::*;

  fn press(app: &mut App, code: KeyCode) {
    app.key(KeyEvent::new(code, KeyModifiers::NONE));
  }

  fn frame(app: &App) -> ratatui::buffer::Buffer {
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| draw::draw(f, app)).unwrap();
    term.backend().buffer().clone()
  }

  fn screen(app: &App) -> String {
    let buf = frame(app);
    (0..buf.area.height)
      .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
      .collect()
  }

  #[test]
  fn draws_one_even_selection_and_no_bars() {
    use ratatui::style::{Color, Modifier};

    let mut app = app();
    let buf = frame(&app);
    let out = screen(&app);
    assert!(!out.contains('█') && !out.contains('░') && out.contains('%'), "{out}");
    // The selected row (the first under the header) is one dark gray bar, nothing reversed.
    let y = (0..buf.area.height).find(|y| buf[(0, *y)].symbol() == "▶").unwrap();
    for x in 0..buf.area.width {
      let cell = &buf[(x, y)];
      assert_eq!(cell.bg, Color::DarkGray, "column {x}");
      assert!(!cell.modifier.contains(Modifier::REVERSED), "column {x}");
    }
    // The active period is marked by its text, not a filled block.
    assert!((0..buf.area.width).all(|x| !buf[(x, 0)].modifier.contains(Modifier::REVERSED)));

    // A drill-down is a bordered box over the list, which stays drawn around it.
    press(&mut app, KeyCode::Enter);
    let out = screen(&app);
    assert!(out.contains('┌') && out.contains('┘') && out.contains("Name"), "{out}");
  }

  fn app() -> App {
    let mut app = App::new(data::tests::data());
    press(&mut app, KeyCode::Right); // the fixtures are older than the month: show all
    press(&mut app, KeyCode::Right);
    app
  }

  #[test]
  fn switches_views_and_drills_down() {
    let mut app = app();
    assert_eq!(app.span, Span::All);
    let out = screen(&app);
    assert!(out.contains("Projects") && out.contains("oss/demo"), "{out}");

    press(&mut app, KeyCode::Enter);
    let out = screen(&app);
    assert!(out.contains("claude/claude-sonnet-5") && out.contains(data::tests::REPO), "{out}");
    let models = app.open.as_ref().unwrap().rows.len();
    press(&mut app, KeyCode::Down);
    assert_eq!(app.osel, 1.min(models - 1));
    // The opened row follows the provider: codex alone has no oss/demo models from claude.
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Char('p'));
    let open = app.open.as_ref().unwrap();
    assert!(
      open.rows.iter().all(|x| x.key.starts_with("codex/"))
        && open.row.tokens < app.total.tokens + 1
    );
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Esc);
    assert!(app.open.is_none());

    press(&mut app, KeyCode::Char('2'));
    assert!(screen(&app).contains("codex/gpt-5.4"));
    press(&mut app, KeyCode::Enter);
    assert!(screen(&app).contains("oss/demo"));

    press(&mut app, KeyCode::Char('3'));
    assert!(app.list.iter().all(|x| x.day.is_some()));
    press(&mut app, KeyCode::Char('?'));
    assert!(screen(&app).contains("provider"));
  }

  #[test]
  fn metric_provider_and_filter_change_the_rows() {
    let mut app = app();
    press(&mut app, KeyCode::Char('2'));
    let first = app.list[0].key.clone();
    press(&mut app, KeyCode::Char('m'));
    press(&mut app, KeyCode::Char('m'));
    assert_eq!(app.metric, Metric::Cost);
    assert!(app.list.windows(2).all(|x| x[0].cost >= x[1].cost), "{first}");

    press(&mut app, KeyCode::Char('p'));
    assert!(app.list.iter().all(|x| x.key.starts_with("claude/")));
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(app.prov, None);

    press(&mut app, KeyCode::Char('/'));
    for c in "mini".chars() {
      press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    assert!(!app.list.is_empty() && app.list.iter().all(|x| x.label.contains("mini")));
    press(&mut app, KeyCode::Esc);
    assert!(app.filter.is_empty());
  }
}
