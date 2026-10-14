// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

//! The interactive view: `cctrail` without arguments on a terminal. The history is parsed once;
//! every switch after that is in memory.

mod data;
mod draw;

use std::cell::RefCell;

use anyhow::Result;
pub use data::{Data, Row, Span};
use ratatui::crossterm::event::{
  self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
  KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Position, Rect};

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

/// A sortable column; a click on its header sorts by it, a second click reverses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Col {
  Name,
  Time,
  Tokens,
  Cost,
  Sessions,
  /// The metric's share.
  Share,
}

/// A drill-down: the opened row and what it shows.
pub struct Detail {
  pub row: Row,
  pub rows: Vec<Row>,
  /// The project's metric per day (or week), oldest first.
  pub days: Vec<Row>,
  pub paths: Vec<String>,
}

/// Where the last frame put what a click can hit.
#[derive(Clone, Debug, Default)]
pub struct Hits {
  pub periods: Vec<(Rect, Span)>,
  pub tabs: Vec<(Rect, Tab)>,
  /// The header row of the table and the columns' x ranges.
  pub head: Rect,
  pub cols: Vec<(u16, u16, Col)>,
  /// The rows' area and the first row shown in it.
  pub rows: Rect,
  pub offset: usize,
}

pub struct App {
  pub data: Data,
  pub span: Span,
  pub prov: Option<Provider>,
  pub metric: Metric,
  /// A clicked column and whether reversed; none sorts by the metric, most first.
  pub sort: Option<(Col, bool)>,
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
  pub hits: RefCell<Hits>,
  /// The tab `list` was built for.
  shown: Option<Tab>,
  pub quit: bool,
}

impl App {
  pub fn new(data: Data) -> Self {
    let mut app = Self {
      data,
      span: Span::Month,
      prov: None,
      metric: Metric::Time,
      sort: None,
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
      hits: RefCell::default(),
      shown: None,
      quit: false,
    };
    app.refresh();
    app
  }

  pub fn sel(&self) -> usize {
    self.sel[self.tab as usize]
  }

  fn refresh(&mut self) {
    // The selection follows its row across sorting, period, metric and provider changes.
    let kept =
      self.shown.filter(|t| *t == self.tab).and(self.list.get(self.sel())).map(|x| x.key.clone());
    let okept = self.open.as_ref().and_then(|d| d.rows.get(self.osel)).map(|x| x.key.clone());
    let (projects, total) = self.data.projects(self.span, self.prov, false);
    self.total = total;
    self.unpriced = self.data.unpriced(self.span, self.prov);
    let mut list = match self.tab {
      Tab::Projects => projects,
      Tab::Models => self.data.models(self.span, self.prov, None),
      Tab::Timeline => self.data.timeline(self.span, self.prov, None),
    };
    // The timeline keeps its days in order unless a column was clicked.
    if self.tab != Tab::Timeline || self.sort.is_some() {
      sort(&mut list, self.metric, self.sort);
    }
    let needle = self.filter.to_lowercase();
    list.retain(|x| x.label.to_lowercase().contains(&needle));
    // With a filter, the total is of the rows shown.
    if !needle.is_empty() {
      self.total = sum(&list);
    }
    let last = list.len().saturating_sub(1);
    let found = kept.and_then(|k| list.iter().position(|x| x.key == k));
    let sel = &mut self.sel[self.tab as usize];
    *sel = match found {
      Some(i) => i,
      // The timeline opens on its newest day.
      None if self.tab == Tab::Timeline && self.sort.is_none() => last,
      None => (*sel).min(last),
    };
    self.list = list;
    self.shown = Some(self.tab);
    // An open drill-down follows the period and provider: its row as it is now, or closed.
    if let Some(open) = &self.open {
      let row = self.list.iter().find(|x| x.key == open.row.key).cloned();
      self.open = row.map(|x| self.detail(x));
      let rows = self.open.as_ref().map_or(&[][..], |x| &x.rows);
      let found = okept.and_then(|k| rows.iter().position(|x| x.key == k));
      self.osel = found.unwrap_or(self.osel).min(rows.len().saturating_sub(1));
    }
  }

  fn detail(&self, row: Row) -> Detail {
    let (span, prov) = (self.span, self.prov);
    let mut rows = match self.tab {
      Tab::Projects => self.data.models(span, prov, Some(&row.key)),
      Tab::Models => self.data.model_projects(span, prov, &row.key),
      Tab::Timeline => self.data.bucket_projects(span, prov, row.day.unwrap_or(self.data.today)),
    };
    sort(&mut rows, self.metric, self.sort);
    let project = self.tab == Tab::Projects;
    let mut row = row;
    if self.tab == Tab::Timeline {
      row.sessions = Some(rows.iter().filter_map(|x| x.sessions).sum());
    }
    Detail {
      days: if project { self.data.timeline(span, prov, Some(&row.key)) } else { vec![] },
      paths: if project { self.data.paths(&row.key) } else { vec![] },
      rows,
      row,
    }
  }

  /// Opens the selected row; `other` and empty days open nothing.
  fn open(&mut self) {
    let row = self.list.get(self.sel()).filter(|x| !x.key.is_empty());
    if let Some(row) = row.filter(|x| x.tokens > 0 || x.active.unwrap_or(0) > 0) {
      self.open = Some(self.detail(row.clone()));
      self.osel = 0;
    }
  }

  /// Moves the selection of the list or the drill-down by `by` rows.
  fn step(&mut self, by: isize) {
    let (sel, len) = match &self.open {
      Some(d) => (&mut self.osel, d.rows.len()),
      None => (&mut self.sel[self.tab as usize], self.list.len()),
    };
    *sel = sel.saturating_add_signed(by).min(len.saturating_sub(1));
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
    let back = matches!(key.code, KeyCode::Esc | KeyCode::Backspace);
    match key.code {
      // Moves and toggles that change no data return without rebuilding the rows.
      KeyCode::Char('q') => return self.quit = true,
      KeyCode::Char('?') => return self.help = !self.help,
      _ if back && self.help => return self.help = false,
      _ if back && self.open.is_some() => return self.open = None,
      _ if back && self.filter.is_empty() => return,
      _ if back => self.filter.clear(),
      KeyCode::Char(c @ '1'..='3') => self.tab(c as usize - '1' as usize),
      KeyCode::Left => self.span = Span::ALL[(self.span as usize).saturating_sub(1)],
      KeyCode::Right => self.span = Span::ALL[(self.span as usize + 1).min(Span::ALL.len() - 1)],
      KeyCode::Char('m') => {
        self.metric = [Metric::Tokens, Metric::Cost, Metric::Time][self.metric as usize];
        self.sort = None;
      }
      KeyCode::Char('p') => {
        self.prov = match self.prov {
          None => Some(Provider::Claude),
          Some(Provider::Claude) => Some(Provider::Codex),
          Some(Provider::Codex) => None,
        }
      }
      KeyCode::Char('/') => return self.typing = true,
      KeyCode::Up => return self.step(-1),
      KeyCode::Down => return self.step(1),
      KeyCode::Enter if self.open.is_none() => return self.open(),
      _ => return,
    }
    self.refresh();
  }

  fn tab(&mut self, i: usize) {
    self.tab = [Tab::Projects, Tab::Models, Tab::Timeline][i];
    (self.open, self.help, self.sort) = (None, false, None);
  }

  /// Clicks on periods, tabs, column headers and rows; the wheel scrolls; a click on the selected
  /// row opens it (so does a double click), anywhere while the help is open closes it.
  pub fn mouse(&mut self, ev: MouseEvent) {
    let at = Position::new(ev.column, ev.row);
    match ev.kind {
      MouseEventKind::ScrollUp => return self.step(-1),
      MouseEventKind::ScrollDown => return self.step(1),
      MouseEventKind::Down(MouseButton::Left) => {}
      _ => return,
    }
    if self.help {
      return self.help = false;
    }
    let hits = self.hits.borrow().clone();
    if let Some((_, span)) = hits.periods.iter().find(|x| x.0.contains(at)) {
      self.span = *span;
    } else if let Some((_, tab)) = hits.tabs.iter().find(|x| x.0.contains(at)) {
      self.tab(*tab as usize);
    } else if hits.head.contains(at) {
      let Some(&(_, _, col)) = hits.cols.iter().find(|x| (x.0..x.1).contains(&at.x)) else {
        return;
      };
      self.sort = match self.sort {
        Some((c, rev)) if c == col => Some((col, !rev)),
        _ => Some((col, false)),
      };
      if let Some(d) = &mut self.open {
        let key = d.rows.get(self.osel).map(|x| x.key.clone());
        sort(&mut d.rows, self.metric, self.sort);
        self.osel = d.rows.iter().position(|x| Some(&x.key) == key.as_ref()).unwrap_or(0);
        return;
      }
    } else if hits.rows.contains(at) {
      let i = hits.offset + usize::from(at.y - hits.rows.y);
      let (sel, len) = match &self.open {
        Some(d) => (self.osel, d.rows.len()),
        None => (self.sel(), self.list.len()),
      };
      if i >= len {
        return;
      }
      match (&self.open, i == sel) {
        (None, true) => self.open(),
        (None, false) => self.sel[self.tab as usize] = i,
        (Some(_), _) => self.osel = i,
      }
      return;
    } else {
      return;
    }
    self.refresh();
  }
}

/// The sum of rows: time, tokens, cost and sessions where they have them.
fn sum(rows: &[Row]) -> Row {
  let opt = |f: fn(&Row) -> Option<i64>| rows.iter().map(f).sum::<Option<i64>>();
  Row {
    key: "total".into(),
    label: "total".into(),
    active: opt(|x| x.active),
    tokens: rows.iter().fold(0, |a, x| a.saturating_add(x.tokens)),
    cost: rows.iter().map(|x| x.cost).sum(),
    sessions: opt(|x| x.sessions.map(|x| x as i64)).map(|x| x as usize),
    ..Default::default()
  }
}

/// Rows by a clicked column (names A to Z, numbers most first; reversed on a second click), else
/// by the metric, most first; ties by name, `other` last whatever the order.
fn sort(rows: &mut [Row], m: Metric, by: Option<(Col, bool)>) {
  let value = |x: &Row| match by.map(|x| x.0) {
    Some(Col::Time) => x.active.unwrap_or(0) as f64,
    Some(Col::Tokens) => x.tokens as f64,
    Some(Col::Cost) => x.cost,
    Some(Col::Sessions) => x.sessions.unwrap_or(0) as f64,
    _ => m.of(x),
  };
  let rev = by.is_some_and(|x| x.1);
  rows.sort_by(|a, b| {
    let order = match by.map(|x| x.0) {
      Some(Col::Name) => a.label.cmp(&b.label),
      _ => value(b).partial_cmp(&value(a)).unwrap().then_with(|| a.label.cmp(&b.label)),
    };
    let order = if rev { order.reverse() } else { order };
    a.key.is_empty().cmp(&b.key.is_empty()).then(order)
  });
}

/// Runs the interface until `q`: a loading frame while the history is parsed, then the app.
pub fn run() -> Result<()> {
  let mut term = ratatui::init();
  execute!(std::io::stdout(), EnableMouseCapture)?;
  let res = (|| -> Result<()> {
    term.draw(draw::loading)?;
    let config = Config::load()?;
    let sessions = crate::cli::sessions(None);
    let today = crate::metrics::local_date(jiff::Timestamp::now().as_microsecond());
    let mut app = App::new(Data::new(sessions, config, today));
    while !app.quit {
      term.draw(|f| draw::draw(f, &app))?;
      match event::read()? {
        Event::Key(key) => app.key(key),
        Event::Mouse(ev) => app.mouse(ev),
        _ => {}
      }
    }
    Ok(())
  })();
  _ = execute!(std::io::stdout(), DisableMouseCapture);
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
    frame_of(app, 100, 30)
  }

  fn frame_of(app: &App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
    term.draw(|f| draw::draw(f, app)).unwrap();
    term.backend().buffer().clone()
  }

  fn screen(app: &App) -> String {
    let buf = frame(app);
    (0..buf.area.height)
      .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
      .collect()
  }

  /// The fg of the first cell of `text` on line `y`.
  fn fg(buf: &ratatui::buffer::Buffer, y: u16, text: &str) -> ratatui::style::Color {
    let line: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
    let x = line.find(text).unwrap_or_else(|| panic!("{text:?} not in {line:?}"));
    buf[(line[..x].chars().count() as u16, y)].fg
  }

  fn line(buf: &ratatui::buffer::Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
  }

  /// The first line under the header and tabs with `text`.
  fn find(buf: &ratatui::buffer::Buffer, text: &str) -> u16 {
    (3..buf.area.height)
      .find(|y| line(buf, *y).contains(text))
      .unwrap_or_else(|| panic!("{text:?}"))
  }

  #[test]
  fn draws_the_look_of_the_checklist() {
    use ratatui::style::{Color, Modifier};

    let mut app = app();
    let buf = frame(&app);
    let out = screen(&app);
    // Version top right, totals not in the header but as the last `total` row.
    let head = line(&buf, 0);
    assert!(head.trim_end().ends_with(concat!("cctrail v", env!("CARGO_PKG_VERSION"))), "{head}");
    assert!(!head.contains('$'), "{head}");
    let sel = find(&buf, "▶");
    let total = find(&buf, "total");
    assert_eq!(total, sel + app.list.len() as u16, "{out}");
    assert!(buf[(2, total)].modifier.contains(Modifier::BOLD));
    // No bars, percentages only; numbers right-aligned up to an equal right gutter.
    assert!(!out.contains('█') && !out.contains('░') && out.contains('%'), "{out}");
    for y in [sel - 1, sel, total] {
      let l = line(&buf, y);
      assert_eq!(l.chars().count() - l.trim_end().chars().count(), 2, "gutter on {l:?}");
    }
    assert!(line(&buf, sel - 1).trim_end().ends_with("Share"));
    // Selected row: one subtle bar, nothing reversed, colors kept: name default, time yellow,
    // cost green.
    for x in 0..buf.area.width {
      assert_eq!(buf[(x, sel)].bg, Color::Indexed(236), "column {x}");
      assert!(!buf[(x, sel)].modifier.contains(Modifier::REVERSED), "column {x}");
    }
    let row = &app.list[0];
    assert_eq!(fg(&buf, sel, &row.label), Color::Reset);
    assert_eq!(fg(&buf, sel, &crate::output::duration(row.active.unwrap())), Color::Yellow);
    assert_eq!(fg(&buf, sel, "$"), Color::Green);
    assert_eq!(fg(&buf, sel - 1, "Time"), Color::DarkGray);
    // Active period: bold cyan text, others gray, no filled block.
    assert!((0..buf.area.width).all(|x| !buf[(x, 0)].modifier.contains(Modifier::REVERSED)));
    assert_eq!(fg(&buf, 0, "All"), Color::Cyan);
    assert_eq!(fg(&buf, 0, "Week"), Color::DarkGray);
    assert_eq!(fg(&buf, 0, "claude+codex"), Color::Reset);
    assert_eq!(fg(&buf, 0, "cctrail v"), Color::Reset);
    // The tab and the metric look like the period.
    assert_eq!(fg(&buf, 1, "1 Projects"), Color::Cyan);
    assert_eq!(fg(&buf, 1, "time"), Color::Cyan);
    assert_eq!(fg(&buf, 1, "2 Models"), Color::DarkGray);

    // A project's drill-down takes the screen: models with time summing to the project's.
    press(&mut app, KeyCode::Enter);
    let buf = frame(&app);
    let out = screen(&app);
    assert!(!out.contains('┌'), "{out}");
    let d = app.open.as_ref().unwrap();
    let times: i64 = d.rows.iter().map(|x| x.active.unwrap()).sum();
    assert_eq!(times, d.row.active.unwrap(), "{out}");
    let y = find(&buf, "claude/claude-sonnet-5");
    assert_eq!(fg(&buf, y, "claude/claude-sonnet-5"), Color::Reset);
    // Backspace goes back; the same model looks the same in the models tab.
    press(&mut app, KeyCode::Backspace);
    assert!(app.open.is_none());
    press(&mut app, KeyCode::Char('2'));
    let buf = frame(&app);
    let y = find(&buf, "claude/claude-sonnet-5");
    assert_eq!(fg(&buf, y, "claude/claude-sonnet-5"), Color::Reset);
    // The help is a box over the screen.
    press(&mut app, KeyCode::Char('?'));
    let out = screen(&app);
    assert!(out.contains('╭') && out.contains("help") && out.contains("Model"), "{out}");
  }

  fn click(app: &mut App, x: u16, y: u16) {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let kind = MouseEventKind::Down(MouseButton::Left);
    app.mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
  }

  #[test]
  fn clicks_tabs_periods_headers_and_rows() {
    let mut app = app();
    let buf = frame(&app);
    // A period, then a tab, by their text.
    let x = line(&buf, 0).find("Week").unwrap() as u16;
    click(&mut app, x, 0);
    assert_eq!(app.span, Span::Week);
    click(&mut app, line(&buf, 0).find("All").unwrap() as u16, 0);
    let x = line(&buf, 1).find("2 Models").unwrap() as u16;
    click(&mut app, x, 1);
    assert_eq!(app.tab, Tab::Models);
    // A header sorts by its column; again reverses.
    let buf = frame(&app);
    let y = find(&buf, "Tokens");
    let x = line(&buf, y).find("Tokens").unwrap() as u16;
    click(&mut app, x, y);
    assert_eq!(app.sort, Some((Col::Tokens, false)));
    assert!(app.list.windows(2).all(|w| w[0].tokens >= w[1].tokens));
    frame(&app);
    click(&mut app, x, y);
    assert_eq!(app.sort, Some((Col::Tokens, true)));
    assert!(app.list.windows(2).all(|w| w[0].tokens <= w[1].tokens));
    // A row selects; the selected row opens.
    let buf = frame(&app);
    let head = find(&buf, "Tokens");
    click(&mut app, 4, head + 2);
    assert_eq!(app.sel(), 1);
    click(&mut app, 4, head + 2);
    assert!(app.open.is_some());
    // The wheel scrolls the drill-down.
    use ratatui::crossterm::event::{MouseEvent, MouseEventKind};
    let ev = MouseEvent {
      kind: MouseEventKind::ScrollDown,
      column: 0,
      row: 0,
      modifiers: KeyModifiers::NONE,
    };
    app.mouse(ev);
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
    press(&mut app, KeyCode::Char('?'));
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

  fn many() -> App {
    let mut app = App::new(data::tests::many());
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Right);
    app
  }

  /// Minutes in `1d 2h 03m`.
  fn minutes(text: &str) -> i64 {
    let unit = |x: &str, u: char| x.strip_suffix(u).map(|n| n.parse::<i64>().unwrap());
    text
      .split(' ')
      .map(|x| unit(x, 'd').map(|n| n * 1440).or(unit(x, 'h').map(|n| n * 60)).or(unit(x, 'm')))
      .map(|x| x.unwrap_or_else(|| panic!("{text:?}")))
      .sum()
  }

  /// The cells of a table line: runs split by two or more spaces.
  fn cells(line: &str) -> Vec<String> {
    let line = line.trim_start_matches('▶');
    line.split("  ").map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
  }

  #[test]
  fn lays_out_many_projects_at_every_width() {
    use ratatui::style::{Color, Modifier};

    for (width, height) in [(80, 24), (100, 30), (140, 30)] {
      let mut app = many();
      let buf = frame_of(&app, width, height);
      let head = find(&buf, "Project");
      let total = find(&buf, "total");
      assert_eq!(
        cells(&line(&buf, head)),
        ["Project", "Time", "Tokens", "Cost", "Sessions", "Share"]
      );
      for y in head..=total {
        let l = line(&buf, y);
        assert_eq!(l.chars().count() - l.trim_end().chars().count(), 2, "{width}: {l:?}");
      }
      // Shares add up to 100; the long name is cut where it doesn't fit.
      let shares: i64 = (head + 1..total)
        .map(|y| cells(&line(&buf, y)).pop().unwrap())
        .map(|x| x.trim_end_matches('%').parse::<i64>().unwrap_or(0))
        .sum();
      assert_eq!(shares, 100, "{width}");
      assert!(screen_of(&buf).contains("other (2 dirs)"));
      let long = "w/a-project-with-a-name-too-long-for-a-narrow-terminal";
      assert_eq!(screen_of(&buf).contains(long), width == 140, "{width}");
      assert!(width == 140 || line(&buf, find(&buf, "w/a-project-with-")).contains('…'));
      // A separator over the footer, which keeps whole hints only.
      assert!(line(&buf, height - 2).chars().all(|x| x == '─'));
      let foot = line(&buf, height - 1);
      assert!(["quit", "help", "filter", "provider"].iter().any(|x| foot.trim_end().ends_with(x)));
      // A sort mark fits its column and keeps the headers apart.
      let x = line(&buf, head).find("Tokens").unwrap() as u16;
      click(&mut app, x, head);
      let buf = frame_of(&app, width, height);
      let labels = cells(&line(&buf, head));
      assert_eq!(labels, ["Project", "Time", "Tokens ▼", "Cost", "Sessions", "Share"], "{width}");

      // A model without a price has no cost, shown faint.
      press(&mut app, KeyCode::Char('2'));
      let buf = frame_of(&app, width, height);
      let y = find(&buf, "claude-unknown-1");
      let l = line(&buf, y);
      let x = l.chars().position(|x| x == '–').unwrap() as u16;
      assert_eq!((buf[(x, y)].fg, buf[(x, y)].modifier), (Color::Reset, Modifier::DIM));
      assert!(!line(&buf, find(&buf, "Model")).contains("Sessions"));

      // A project's models show minutes that add up to the project's.
      press(&mut app, KeyCode::Char('1'));
      press(&mut app, KeyCode::Enter);
      let buf = frame_of(&app, width, height);
      let head = find(&buf, "Model");
      let total = find(&buf, "total");
      let time = |y| minutes(&cells(&line(&buf, y))[1]);
      assert_eq!((head + 1..total).map(time).sum::<i64>(), time(total), "{}", screen_of(&buf));
      assert!(!line(&buf, head).contains("Sessions"));
    }
  }

  fn screen_of(buf: &ratatui::buffer::Buffer) -> String {
    (0..buf.area.height).map(|y| line(buf, y) + "\n").collect()
  }

  #[test]
  fn keeps_the_selection_and_sums_the_filter() {
    let mut app = many();
    press(&mut app, KeyCode::Down);
    let key = app.list[app.sel()].key.clone();
    let buf = frame(&app);
    let head = find(&buf, "Tokens");
    let x = line(&buf, head).find("Tokens").unwrap() as u16;
    for change in ["sort", "reverse", "metric", "provider", "period"] {
      match change {
        "sort" | "reverse" => click(&mut app, x, head),
        "metric" => press(&mut app, KeyCode::Char('m')),
        "provider" => press(&mut app, KeyCode::Char('p')),
        _ => press(&mut app, KeyCode::Left),
      }
      assert_eq!(app.list[app.sel()].key, key, "after {change}");
    }

    let mut app = many();
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('w'));
    press(&mut app, KeyCode::Enter);
    assert!(app.list.len() > 1 && app.list.len() < many().list.len());
    assert_eq!(app.total.tokens, app.list.iter().map(|x| x.tokens).sum::<u64>());
    // The typed filter shows before the unpriced note.
    let tabs = line(&frame(&app), 1);
    assert!(tabs.find("filter w").unwrap() < tabs.find("unpriced").unwrap(), "{tabs}");
  }

  #[test]
  fn timeline_starts_at_the_newest_day_and_empty_periods_say_so() {
    use ratatui::style::Color;

    let mut app = many();
    press(&mut app, KeyCode::Char('3'));
    assert_eq!(app.sel(), app.list.len() - 1);
    let buf = frame(&app);
    let head = find(&buf, "Week");
    assert_eq!(cells(&line(&buf, head)), ["Week", "Time"]);
    let sel = find(&buf, "▶");
    assert!(line(&buf, sel).contains(&app.list.last().unwrap().label));
    assert_eq!(fg(&buf, sel, "█"), Color::Yellow);
    press(&mut app, KeyCode::Char('m'));
    press(&mut app, KeyCode::Char('m'));
    let buf = frame(&app);
    assert_eq!(cells(&line(&buf, head))[1], "Cost");
    assert_eq!(fg(&buf, find(&buf, "▶"), "█"), Color::Green);
    // A day's drill-down counts its sessions.
    press(&mut app, KeyCode::Enter);
    let d = app.open.as_ref().unwrap();
    assert_eq!(d.row.sessions, Some(d.rows.iter().filter_map(|x| x.sessions).sum()));

    let mut app = App::new(data::tests::data());
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Left);
    assert_eq!(app.span, Span::Today);
    assert!(screen(&app).contains("No activity in this period"));
  }
}
