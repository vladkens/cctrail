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

/// A column of a table; the sorted one also gives the share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Col {
  Name,
  Time,
  Tokens,
  Cost,
  Sessions,
  /// The sorted column's share, Time's when sorted by name.
  Share,
}

impl Col {
  /// The next column in the `s` cycle, as on screen, then the names:
  /// Time → Tokens → Cost → Sessions (where shown) → Name → Time.
  fn next(self, sessions: bool) -> Self {
    match self {
      Self::Time => Self::Tokens,
      Self::Tokens => Self::Cost,
      Self::Cost if sessions => Self::Sessions,
      Self::Cost | Self::Sessions => Self::Name,
      Self::Name | Self::Share => Self::Time,
    }
  }

  /// The column chosen for sorting with its direction: numbers most first, names A to Z.
  fn sorted(self) -> Sort {
    (self, self != Self::Name)
  }

  /// The row's number in this column; a name's share is of the time.
  pub fn of(self, x: &Row) -> f64 {
    match self {
      Self::Tokens => x.tokens as f64,
      Self::Cost => x.cost,
      Self::Sessions => x.sessions.unwrap_or(0) as f64,
      Self::Name | Self::Time | Self::Share => x.active.unwrap_or(0) as f64,
    }
  }

  /// The column a share is of.
  pub fn share(self) -> Self {
    if matches!(self, Self::Name | Self::Share) { Self::Time } else { self }
  }
}

/// A column and whether most first (names: Z to A).
pub type Sort = (Col, bool);

/// A drill-down: the opened row, what it shows and how its rows are sorted.
pub struct Detail {
  pub row: Row,
  pub rows: Vec<Row>,
  pub paths: Vec<String>,
  pub sort: Sort,
  /// Whether its rows count sessions: the projects of a day do.
  pub sessions: bool,
}

/// Where the last frame put what a click can hit.
#[derive(Clone, Debug, Default)]
pub struct Hits {
  pub periods: Vec<(Rect, Span)>,
  pub provider: Rect,
  pub tabs: Vec<(Rect, Tab)>,
  /// The header row of the table and the sortable columns' x ranges.
  pub head: Rect,
  pub cols: Vec<(u16, u16, Col)>,
  /// The rows' area and the first row shown in it.
  pub rows: Rect,
  pub offset: usize,
  /// The timeline's bars and the rows they stand for.
  pub bars: Vec<(Rect, usize)>,
}

pub struct App {
  pub data: Data,
  pub span: Span,
  pub prov: Option<Provider>,
  /// Each tab's sort; the timeline's column is what its bars show, its days stay in order.
  pub sorts: [Sort; 3],
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
  /// The tab `list` was built for and its buckets: days and weeks don't share a selection.
  shown: Option<(Tab, crate::metrics::Granularity)>,
  pub quit: bool,
}

impl App {
  pub fn new(data: Data) -> Self {
    let mut app = Self {
      data,
      span: Span::Month,
      prov: None,
      sorts: [Col::Time.sorted(); 3],
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

  /// The sort of what is on screen: the drill-down's or the tab's.
  pub fn sort(&self) -> Sort {
    self.open.as_ref().map_or(self.sorts[self.tab as usize], |d| d.sort)
  }

  /// Rebuilds the rows for the current tab, period, provider, sort and filter.
  fn refresh(&mut self) {
    // The selection follows its row across sorting, period and provider changes.
    let now = (self.tab, self.span.gran());
    let kept =
      self.shown.filter(|x| *x == now).and(self.list.get(self.sel())).map(|x| x.key.clone());
    let okept = self.open.as_ref().and_then(|d| d.rows.get(self.osel)).map(|x| x.key.clone());
    let (projects, total) = self.data.projects(self.span, self.prov, false);
    self.total = total;
    self.unpriced = self.data.unpriced(self.span, self.prov);
    let mut list = match self.tab {
      Tab::Projects => projects,
      Tab::Models => self.data.models(self.span, self.prov, None),
      Tab::Timeline => self.data.timeline(self.span, self.prov, None),
    };
    // The timeline keeps its days in order.
    if self.tab != Tab::Timeline {
      sort(&mut list, self.sorts[self.tab as usize]);
    }
    let needle = self.filter.to_lowercase();
    list.retain(|x| x.label.to_lowercase().contains(&needle));
    // With a filter, the total is of the rows shown.
    if !needle.is_empty() {
      self.total = sum(&list);
    }
    let found = kept.and_then(|k| list.iter().position(|x| x.key == k));
    let sel = &mut self.sel[self.tab as usize];
    *sel = match found {
      Some(i) => i,
      // The timeline opens on its newest day.
      None if self.tab == Tab::Timeline => list.len().saturating_sub(1),
      None => (*sel).min(list.len().saturating_sub(1)),
    };
    self.list = list;
    self.shown = Some(now);
    // An open drill-down follows the period and provider: its row as it is now, or closed.
    if let Some(open) = &self.open {
      let sort = open.sort;
      let row = self.list.iter().find(|x| x.key == open.row.key).cloned();
      self.open = row.map(|x| self.detail(x, sort));
      let rows = self.open.as_ref().map_or(&[][..], |x| &x.rows);
      let found = okept.and_then(|k| rows.iter().position(|x| x.key == k));
      self.osel = found.unwrap_or(self.osel).min(rows.len().saturating_sub(1));
    }
  }

  fn detail(&self, row: Row, sort: Sort) -> Detail {
    let (span, prov) = (self.span, self.prov);
    let mut rows = match self.tab {
      Tab::Projects => self.data.models(span, prov, Some(&row.key)),
      Tab::Models => self.data.model_projects(span, prov, &row.key),
      Tab::Timeline => self.data.bucket_projects(span, prov, row.day.unwrap_or(self.data.today)),
    };
    let sessions = self.tab == Tab::Timeline;
    let sort = if sort.0 == Col::Sessions && !sessions { Col::Time.sorted() } else { sort };
    self::sort(&mut rows, sort);
    let mut row = row;
    if sessions {
      row.sessions = Some(rows.iter().filter_map(|x| x.sessions).sum());
    }
    let project = self.tab == Tab::Projects;
    Detail {
      paths: if project { self.data.paths(&row.key) } else { vec![] },
      rows,
      row,
      sort,
      sessions,
    }
  }

  /// Opens the selected row; `other` and empty days open nothing.
  fn open(&mut self) {
    let row = self.list.get(self.sel()).filter(|x| !x.key.is_empty());
    if let Some(row) = row.filter(|x| x.tokens > 0 || x.active.unwrap_or(0) > 0) {
      let sort = self.sorts[self.tab as usize];
      self.open = Some(self.detail(row.clone(), sort));
      self.osel = 0;
    }
  }

  /// The selection of the list or the drill-down and its rows' count.
  fn selection(&mut self) -> (&mut usize, usize) {
    match &self.open {
      Some(d) => (&mut self.osel, d.rows.len()),
      None => (&mut self.sel[self.tab as usize], self.list.len()),
    }
  }

  /// Moves the selection by a key: ↑ / ↓ wrap around at the ends, PgUp / PgDn and Home / End
  /// stop there, a page is the rows on screen (a week of the timeline's days).
  fn nav(&mut self, code: KeyCode) {
    let page = match self.open.is_none() && self.tab == Tab::Timeline {
      true => 7,
      false => usize::from(self.hits.borrow().rows.height).max(1),
    };
    let (sel, len) = self.selection();
    if let Some(i) = step(code, *sel, page, len) {
      *sel = i;
    }
  }

  /// Sorts what is on screen by `sort`: a drill-down keeps its selected row, a list rebuilds.
  fn sort_by(&mut self, by: Sort) {
    match &mut self.open {
      Some(d) => {
        let key = d.rows.get(self.osel).map(|x| x.key.clone());
        d.sort = by;
        sort(&mut d.rows, by);
        self.osel = d.rows.iter().position(|x| Some(&x.key) == key.as_ref()).unwrap_or(0);
      }
      None => {
        self.sorts[self.tab as usize] = by;
        self.refresh();
      }
    }
  }

  /// `s`: the next column; on the timeline what the bars show, time, tokens, cost.
  fn next_sort(&mut self) {
    let (col, _) = self.sort();
    let by = match (&self.open, self.tab) {
      (None, Tab::Timeline) => match col {
        Col::Time => Col::Tokens.sorted(),
        Col::Tokens => Col::Cost.sorted(),
        _ => Col::Time.sorted(),
      },
      (Some(d), _) => col.next(d.sessions).sorted(),
      (None, tab) => col.next(tab == Tab::Projects).sorted(),
    };
    self.sort_by(by);
  }

  /// A header click: its column, again to reverse. Share follows the sorted column.
  fn click_sort(&mut self, col: Col) {
    let (now, most) = self.sort();
    match col {
      Col::Share => {}
      _ if col == now => self.sort_by((col, !most)),
      _ => self.sort_by(col.sorted()),
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
    // The help takes every key while open, as macmon's: Esc, `?` and `q` close it.
    if self.help {
      if matches!(key.code, KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('?' | 'q')) {
        self.help = false;
      }
      return;
    }
    // `q` in a drill-down goes back like Esc; it quits only from the lists.
    let back = matches!(key.code, KeyCode::Esc | KeyCode::Backspace)
      || (key.code == KeyCode::Char('q') && self.open.is_some());
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
      // Moves and toggles that change no data return without rebuilding the rows.
      _ if back && self.open.is_some() => return self.open = None,
      _ if back && self.filter.is_empty() => return,
      _ if back => self.filter.clear(),
      KeyCode::Char('q') => return self.quit = true,
      KeyCode::Char('?') => return self.help = true,
      KeyCode::Char(c @ '1'..='3') => self.tab(c as usize - '1' as usize),
      KeyCode::Left => self.span = Span::ALL[(self.span as usize + Span::ALL.len() - 1) % 5],
      KeyCode::Right => self.span = Span::ALL[(self.span as usize + 1) % Span::ALL.len()],
      KeyCode::Char('p') => self.next_provider(),
      KeyCode::Char('S') => return self.reverse(),
      KeyCode::Char('s') if shift => return self.reverse(),
      KeyCode::Char('s') => return self.next_sort(),
      KeyCode::Char('/') => return self.typing = true,
      KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
        return self.nav(key.code);
      }
      KeyCode::Home | KeyCode::End => return self.nav(key.code),
      KeyCode::Enter if self.open.is_none() => return self.open(),
      _ => return,
    }
    self.refresh();
  }

  /// `S`: the other direction; the timeline's days stay in order.
  fn reverse(&mut self) {
    if self.open.is_some() || self.tab != Tab::Timeline {
      let (col, most) = self.sort();
      self.sort_by((col, !most));
    }
  }

  fn next_provider(&mut self) {
    self.prov = match self.prov {
      None => Some(Provider::Claude),
      Some(Provider::Claude) => Some(Provider::Codex),
      Some(Provider::Codex) => None,
    }
  }

  fn tab(&mut self, i: usize) {
    self.tab = [Tab::Projects, Tab::Models, Tab::Timeline][i];
    (self.open, self.help) = (None, false);
  }

  /// Clicks on periods, the provider, tabs, column headers, rows and bars; the wheel scrolls,
  /// stopping at the ends; a click on the selected row or bar opens it (so does a double click),
  /// anywhere while the help is open closes it.
  pub fn mouse(&mut self, ev: MouseEvent) {
    let at = Position::new(ev.column, ev.row);
    let wheel = |app: &mut Self, up: bool| {
      let (sel, len) = app.selection();
      *sel = if up { sel.saturating_sub(1) } else { (*sel + 1).min(len.saturating_sub(1)) };
    };
    match ev.kind {
      MouseEventKind::ScrollUp => return wheel(self, true),
      MouseEventKind::ScrollDown => return wheel(self, false),
      MouseEventKind::Down(MouseButton::Left) => {}
      _ => return,
    }
    if self.help {
      return self.help = false;
    }
    let hits = self.hits.borrow().clone();
    if let Some((_, span)) = hits.periods.iter().find(|x| x.0.contains(at)) {
      self.span = *span;
    } else if hits.provider.contains(at) {
      self.next_provider();
    } else if let Some((_, tab)) = hits.tabs.iter().find(|x| x.0.contains(at)) {
      self.tab(*tab as usize);
    } else if hits.head.contains(at) {
      if let Some(&(_, _, col)) = hits.cols.iter().find(|x| (x.0..x.1).contains(&at.x)) {
        self.click_sort(col);
      }
      return;
    } else if let Some(&(_, i)) = hits.bars.iter().find(|x| x.0.contains(at)) {
      match i == self.sel() {
        true => self.open(),
        false => self.sel[self.tab as usize] = i,
      }
      return;
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

/// The row of `len` a navigation key moves the selection to from row `at`, with `page` rows on
/// screen: ↑ / ↓ wrap around at the ends, the others stop there; `None` for other keys.
fn step(code: KeyCode, at: usize, page: usize, len: usize) -> Option<usize> {
  let last = len.saturating_sub(1);
  Some(match code {
    KeyCode::Home => 0,
    KeyCode::End => last,
    KeyCode::Up if at == 0 => last,
    KeyCode::Up => at - 1,
    KeyCode::Down if at >= last => 0,
    KeyCode::Down => at + 1,
    KeyCode::PageUp => at.saturating_sub(page),
    KeyCode::PageDown => (at + page).min(last),
    _ => return None,
  })
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

/// Rows by a column, most first or least first; ties by name, `other` last whatever the order.
fn sort(rows: &mut [Row], (col, most): Sort) {
  rows.sort_by(|a, b| {
    let order = match col {
      Col::Name => a.label.to_lowercase().cmp(&b.label.to_lowercase()),
      _ => col.of(a).partial_cmp(&col.of(b)).unwrap(),
    };
    let order = if most { order.reverse() } else { order };
    a.key.is_empty().cmp(&b.key.is_empty()).then(order).then_with(|| a.label.cmp(&b.label))
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
  use ratatui::buffer::Buffer;
  use ratatui::style::{Color, Modifier};

  use super::*;

  fn press(app: &mut App, code: KeyCode) {
    app.key(KeyEvent::new(code, KeyModifiers::NONE));
  }

  fn click(app: &mut App, x: u16, y: u16) {
    let kind = MouseEventKind::Down(MouseButton::Left);
    app.mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
  }

  fn wheel(app: &mut App, kind: MouseEventKind) {
    app.mouse(MouseEvent { kind, column: 0, row: 0, modifiers: KeyModifiers::NONE });
  }

  fn frame(app: &App) -> Buffer {
    frame_of(app, 100, 30)
  }

  fn frame_of(app: &App, width: u16, height: u16) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
    term.draw(|f| draw::draw(f, app)).unwrap();
    term.backend().buffer().clone()
  }

  fn screen_of(buf: &Buffer) -> String {
    (0..buf.area.height).map(|y| line(buf, y) + "\n").collect()
  }

  fn screen(app: &App) -> String {
    screen_of(&frame(app))
  }

  fn line(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
  }

  /// The column of `text` on line `y`.
  fn col(buf: &Buffer, y: u16, text: &str) -> u16 {
    let l = line(buf, y);
    let x = l.find(text).unwrap_or_else(|| panic!("{text:?} not in {l:?}"));
    l[..x].chars().count() as u16
  }

  /// The fg of the first cell of `text` on line `y`.
  fn fg(buf: &Buffer, y: u16, text: &str) -> Color {
    buf[(col(buf, y, text), y)].fg
  }

  /// The first line under the header and tabs with `text`.
  fn find(buf: &Buffer, text: &str) -> u16 {
    (3..buf.area.height)
      .find(|y| line(buf, *y).contains(text))
      .unwrap_or_else(|| panic!("{text:?} in {}", screen_of(buf)))
  }

  /// The cells of a table line: runs split by two or more spaces.
  fn cells(line: &str) -> Vec<String> {
    let line = line.trim_start_matches('▶');
    line.split("  ").map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
  }

  /// The fixtures, all of their history.
  fn app() -> App {
    let mut app = App::new(data::tests::data());
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Right);
    app
  }

  /// Many projects, all of their history.
  fn many() -> App {
    let mut app = App::new(data::tests::many());
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Right);
    app
  }

  #[test]
  fn draws_the_look_of_the_checklist() {
    let mut app = app();
    assert_eq!(app.span, Span::All);
    let buf = frame(&app);
    let out = screen_of(&buf);
    // Version top right, totals not in the header but as the last `total` row.
    let head = line(&buf, 0);
    assert!(head.trim_end().ends_with(concat!("cctrail v", env!("CARGO_PKG_VERSION"))), "{head}");
    assert!(!head.contains('$'), "{head}");
    let sel = find(&buf, "▶");
    let total = find(&buf, "total");
    assert_eq!(total, sel + app.list.len() as u16, "{out}");
    assert!(buf[(2, total)].modifier.contains(Modifier::BOLD));
    // No bars, percentages only; numbers right-aligned up to an equal right gutter.
    assert!(!out.contains('█') && out.contains('%'), "{out}");
    for y in [sel - 1, sel, total] {
      let l = line(&buf, y);
      assert_eq!(l.chars().count() - l.trim_end().chars().count(), 2, "gutter on {l:?}");
    }
    assert!(line(&buf, sel - 1).trim_end().ends_with("Share"));
    // Selected row: one subtle bar, nothing reversed, colors kept.
    for x in 0..buf.area.width {
      assert_eq!(buf[(x, sel)].bg, Color::Indexed(236), "column {x}");
      assert!(!buf[(x, sel)].modifier.contains(Modifier::REVERSED), "column {x}");
    }
    let row = &app.list[0];
    assert_eq!(fg(&buf, sel, &row.label), Color::Reset);
    assert_eq!(fg(&buf, sel, &crate::output::duration(row.active.unwrap())), Color::Yellow);
    assert_eq!(fg(&buf, sel, "$"), Color::Green);
    assert_eq!(fg(&buf, sel - 1, "Time"), Color::DarkGray);
    // Active period and tab: bold cyan text, others gray, no filled block; no metric.
    assert!((0..buf.area.width).all(|x| !buf[(x, 0)].modifier.contains(Modifier::REVERSED)));
    assert_eq!(fg(&buf, 0, "All"), Color::Cyan);
    assert_eq!(fg(&buf, 0, "Week"), Color::DarkGray);
    assert_eq!(fg(&buf, 0, "claude+codex"), Color::Reset);
    assert_eq!(fg(&buf, 0, "cctrail v"), Color::Reset);
    assert_eq!(fg(&buf, 1, "1 Projects"), Color::Cyan);
    assert_eq!(fg(&buf, 1, "2 Models"), Color::DarkGray);
    assert!(!out.contains("metric"), "{out}");

    // A project's drill-down takes the screen: models with time summing to the project's, its
    // paths, no chart.
    press(&mut app, KeyCode::Enter);
    let buf = frame(&app);
    let out = screen_of(&buf);
    assert!(!out.contains('╭') && !out.contains('█') && out.contains("Paths"), "{out}");
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

  #[test]
  fn esc_and_q_go_back_and_q_quits_only_the_lists() {
    let mut app = app();
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Char('q'));
    assert!(!app.help && !app.quit);
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Esc);
    assert!(!app.help);
    // The help takes every key while open.
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Char('2'));
    assert_eq!((app.help, app.tab), (true, Tab::Projects));
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.open.is_none() && !app.quit);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);
    assert!(app.open.is_none());
    press(&mut app, KeyCode::Char('q'));
    assert!(app.quit);
  }

  #[test]
  fn sorts_by_a_column_with_its_arrow_like_macmon() {
    let mut app = many();
    let head = |app: &App| {
      let buf = frame(app);
      cells(&line(&buf, find(&buf, "Project")))
    };
    // Time, most first, by default; `s` steps through the columns, `S` reverses.
    assert_eq!(head(&app), ["Project", "Time ↓", "Tokens", "Cost", "Sessions", "Share"]);
    assert!(
      app
        .list
        .windows(2)
        .all(|w| w[0].key.is_empty() || w[1].key.is_empty() || w[0].active >= w[1].active)
    );
    let order = [Col::Tokens, Col::Cost, Col::Sessions, Col::Name, Col::Time];
    for col in order {
      press(&mut app, KeyCode::Char('s'));
      assert_eq!(app.sort(), (col, col != Col::Name));
    }
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Char('S'));
    assert_eq!(app.sort(), (Col::Tokens, false));
    assert_eq!(head(&app)[2], "Tokens ↑");
    let named: Vec<_> = app.list.iter().filter(|x| !x.key.is_empty()).collect();
    assert!(named.windows(2).all(|w| w[0].tokens <= w[1].tokens));
    // The share is of the sorted column: tokens here, so the largest share is the last row's.
    let buf = frame(&app);
    let share =
      |y| cells(&line(&buf, y)).pop().unwrap().trim_end_matches('%').parse::<u32>().unwrap();
    let (first, total) = (find(&buf, "Project") + 1, find(&buf, "total"));
    let tokens = |i: usize| app.list[i].tokens as f64 / app.total.tokens as f64 * 100.0;
    let i = app.list.iter().position(|x| x.key == "w/api").unwrap();
    assert!((share(first + i as u16) as f64 - tokens(i)).abs() <= 1.0);
    assert!(total > first);
    // By name, the share is of the time; models have no sessions to sort by.
    press(&mut app, KeyCode::Char('2'));
    let order = [Col::Tokens, Col::Cost, Col::Name, Col::Time];
    for col in order {
      press(&mut app, KeyCode::Char('s'));
      assert_eq!(app.sort().0, col);
    }
    // A model without a price has no cost nor a share of it, both faint.
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Char('s'));
    let buf = frame(&app);
    let y = find(&buf, "claude-unknown-1");
    assert_eq!(cells(&line(&buf, y))[3..], ["–", "–"]);
    let x = col(&buf, y, "–");
    assert_eq!((buf[(x, y)].fg, buf[(x, y)].modifier), (Color::Reset, Modifier::DIM));
  }

  #[test]
  fn lists_wrap_around_and_page_like_macmon() {
    use KeyCode::{Down, End, Home, PageDown, PageUp, Up};
    // ↑ / ↓ wrap around at the ends, PgUp / PgDn and Home / End stop there.
    let steps = [(Up, 0, 6), (Down, 6, 0), (Down, 0, 1), (PageDown, 1, 6), (PageDown, 6, 6)];
    let back = [(PageUp, 6, 1), (PageUp, 1, 0), (End, 0, 6), (Home, 6, 0), (Home, 0, 0)];
    for (code, at, to) in steps.into_iter().chain(back) {
      assert_eq!(step(code, at, 5, 7), Some(to), "{code:?} from {at}");
    }
    let mut app = many();
    let last = app.list.len() - 1;
    press(&mut app, KeyCode::Up);
    assert_eq!(app.sel(), last);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.sel(), 0);
    // The wheel stops at the ends.
    wheel(&mut app, MouseEventKind::ScrollUp);
    assert_eq!(app.sel(), 0);
    press(&mut app, KeyCode::End);
    wheel(&mut app, MouseEventKind::ScrollDown);
    assert_eq!(app.sel(), last);
    // Periods wrap around too.
    assert_eq!(app.span, Span::All);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.span, Span::Today);
    press(&mut app, KeyCode::Left);
    assert_eq!(app.span, Span::All);
    // A scrollbar in the right gutter when the rows don't fit, none when they do.
    let bar = |buf: &Buffer| {
      (0..buf.area.height)
        .map(|y| buf[(buf.area.width - 1, y)].symbol().to_string())
        .collect::<String>()
    };
    let buf = frame_of(&app, 80, 10);
    assert!(bar(&buf).contains('┃') && bar(&buf).contains('↓'), "{}", screen_of(&buf));
    assert!(!bar(&frame_of(&app, 80, 24)).contains('┃'));
  }

  #[test]
  fn clicks_periods_provider_tabs_headers_and_rows() {
    let mut app = app();
    let buf = frame(&app);
    click(&mut app, col(&buf, 0, "Week"), 0);
    assert_eq!(app.span, Span::Week);
    click(&mut app, col(&buf, 0, "All"), 0);
    click(&mut app, col(&buf, 0, "claude+codex"), 0);
    assert_eq!(app.prov, Some(Provider::Claude));
    let buf = frame(&app);
    click(&mut app, col(&buf, 0, "claude") + 1, 0);
    click(&mut app, col(&buf, 0, "claude") + 1, 0);
    assert_eq!(app.prov, None);
    click(&mut app, col(&buf, 1, "2 Models"), 1);
    assert_eq!(app.tab, Tab::Models);
    // A header sorts by its column, most first; again reverses; Share follows the sort.
    let buf = frame(&app);
    let y = find(&buf, "Tokens");
    let x = col(&buf, y, "Tokens");
    click(&mut app, x, y);
    assert_eq!(app.sort(), (Col::Tokens, true));
    assert!(app.list.windows(2).all(|w| w[0].tokens >= w[1].tokens));
    frame(&app);
    click(&mut app, x, y);
    assert_eq!(app.sort(), (Col::Tokens, false));
    click(&mut app, col(&buf, y, "Share"), y);
    assert_eq!(app.sort(), (Col::Tokens, false));
    // A row selects; the selected row opens.
    let buf = frame(&app);
    let head = find(&buf, "Tokens");
    click(&mut app, 4, head + 2);
    assert_eq!(app.sel(), 1);
    click(&mut app, 4, head + 2);
    assert!(app.open.is_some());
  }

  #[test]
  fn switches_views_and_drills_down() {
    let mut app = app();
    press(&mut app, KeyCode::Enter);
    let out = screen(&app);
    assert!(out.contains("claude/claude-sonnet-5") && out.contains(data::tests::REPO), "{out}");
    let models = app.open.as_ref().unwrap().rows.len();
    press(&mut app, KeyCode::Down);
    assert_eq!(app.osel, 1.min(models - 1));
    // A drill-down sorts on its own, keeping its selected row.
    let key = app.open.as_ref().unwrap().rows[app.osel].key.clone();
    press(&mut app, KeyCode::Char('s'));
    let d = app.open.as_ref().unwrap();
    assert_eq!((d.sort, &d.rows[app.osel].key), ((Col::Tokens, true), &key));
    assert_eq!(app.sorts[0], (Col::Time, true));
    // The opened row follows the provider: codex alone has no oss/demo models from claude.
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Char('p'));
    let open = app.open.as_ref().unwrap();
    assert!(open.rows.iter().all(|x| x.key.starts_with("codex/")));
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Esc);
    assert!(app.open.is_none());

    press(&mut app, KeyCode::Char('2'));
    assert!(screen(&app).contains("codex/gpt-5.4"));
    press(&mut app, KeyCode::Enter);
    assert!(screen(&app).contains("oss/demo"));
    press(&mut app, KeyCode::Char('3'));
    assert!(app.list.iter().all(|x| x.day.is_some()));
  }

  #[test]
  fn lays_out_many_projects_at_every_width() {
    for (width, height) in [(80, 24), (100, 30), (140, 30)] {
      let mut app = many();
      let buf = frame_of(&app, width, height);
      let head = find(&buf, "Project");
      let total = find(&buf, "total");
      let labels = cells(&line(&buf, head));
      assert_eq!(labels, ["Project", "Time ↓", "Tokens", "Cost", "Sessions", "Share"]);
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
      // A separator over the footer, which keeps whole hints, help and quit the longest.
      assert!(line(&buf, height - 2).chars().all(|x| x == '─'));
      let foot = line(&buf, height - 1);
      assert!(foot.contains("? help") && foot.trim_end().ends_with("q quit"), "{foot}");
      assert_eq!(foot.contains("1-3 view"), width > 80, "{foot}");
      // The unpriced note shows whole or not at all.
      let tabs = line(&buf, 1);
      assert_eq!(tabs.contains("unpriced"), tabs.contains("claude-unknown-1"), "{tabs}");
      assert!(!tabs.contains('…'), "{tabs}");
      // A sort arrow fits its column and keeps the headers apart.
      click(&mut app, col(&buf, head, "Sessions"), head);
      let buf = frame_of(&app, width, height);
      let labels = cells(&line(&buf, head));
      assert_eq!(labels, ["Project", "Time", "Tokens", "Cost", "Sessions ↓", "Share"], "{width}");

      // A project's drill-down shows the same times as the list.
      press(&mut app, KeyCode::Char('s'));
      let project = crate::output::duration(app.list[app.sel()].active.unwrap());
      press(&mut app, KeyCode::Enter);
      let buf = frame_of(&app, width, height);
      let head = find(&buf, "Model");
      let total = find(&buf, "total");
      assert_eq!(cells(&line(&buf, total))[1], project);
      let d = app.open.as_ref().unwrap();
      for (y, row) in (head + 1..total).zip(&d.rows) {
        assert_eq!(cells(&line(&buf, y))[1], crate::output::duration(row.active.unwrap()));
      }
      assert!(!line(&buf, head).contains("Sessions"));
    }
  }

  #[test]
  fn keeps_the_selection_and_sums_the_filter() {
    let mut app = many();
    press(&mut app, KeyCode::Down);
    let key = app.list[app.sel()].key.clone();
    for change in ["sort", "reverse", "provider", "period"] {
      match change {
        "sort" => press(&mut app, KeyCode::Char('s')),
        "reverse" => press(&mut app, KeyCode::Char('S')),
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
    // A filter with no rows says so.
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('z'));
    assert!(app.list.is_empty());
    assert!(screen(&app).contains("No match for \"wz\""), "{}", screen(&app));
    // The help box says how to close it, the footer doesn't.
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('?'));
    assert!(!line(&frame(&app), 29).contains("esc"));
  }

  #[test]
  fn timeline_draws_a_bar_per_day() {
    let mut app = many();
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Left); // 30 days
    let last = app.list.len() - 1;
    assert_eq!((app.list.len(), app.sel()), (30, last));
    let buf = frame(&app);
    let out = screen_of(&buf);
    // The title says what the bars show; the largest value labels the axis.
    assert!(line(&buf, 3).contains("Time by day"), "{out}");
    assert_eq!(fg(&buf, 3, "Time"), Color::Cyan);
    let max = app.list.iter().filter_map(|x| x.active).max().unwrap();
    assert!(line(&buf, 5).contains(&crate::output::duration(max)), "{out}");
    // One bar per busy day with a gap after each, the selected one in the selection color, the
    // others in the time's.
    let base = find(&buf, "01-");
    let bars = line(&buf, base - 1);
    let blocks = |t: &str| !t.is_empty() && t.chars().all(|c| "▁▂▃▄▅▆▇█".contains(c));
    let busy = app.list.iter().filter(|x| x.tokens > 0).count();
    assert_eq!(bars.split(' ').filter(|t| blocks(t)).count(), busy, "{out}");
    assert_eq!(fg(&buf, base - 1, "█"), Color::Yellow);
    // The dates under the bars end with the newest day; the numbers of the selected day and the
    // period's total under the chart.
    let newest = app.list[last].day.unwrap().strftime("%m-%d").to_string();
    assert!(line(&buf, base).trim_end().ends_with(&newest), "{out}");
    let sel = find(&buf, "▶");
    assert!(line(&buf, sel).contains(&app.list[last].label));
    assert!(line(&buf, sel + 1).contains("total"));
    // ↑ selects the day before, `s` shows tokens then cost.
    let busy = app.list.iter().rposition(|x| x.tokens > 0).unwrap();
    for _ in busy..last {
      press(&mut app, KeyCode::Up);
    }
    assert_eq!(app.sel(), busy);
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Char('s'));
    let buf = frame(&app);
    assert!(line(&buf, 3).contains("Cost by day"));
    let bars = line(&buf, base - 1);
    let x = bars.chars().take_while(|x| *x != '█').count() as u16;
    let cyan = (0..buf.area.width).filter(|x| buf[(*x, base - 1)].fg == Color::Cyan).count();
    assert!(cyan > 0 && buf[(x, base - 1)].fg == Color::Green, "{}", screen_of(&buf));
    assert_eq!(app.sel(), busy);
    // A click on a bar selects it, a second one opens its day.
    let (rect, i) = app
      .hits
      .borrow()
      .bars
      .iter()
      .find(|x| x.1 != busy && app.list[x.1].tokens > 0)
      .copied()
      .unwrap();
    click(&mut app, rect.x, rect.bottom() - 1);
    assert_eq!(app.sel(), i);
    click(&mut app, rect.x, rect.bottom() - 1);
    let d = app.open.as_ref().unwrap();
    assert_eq!(d.row.sessions, Some(d.rows.iter().filter_map(|x| x.sessions).sum()));
    press(&mut app, KeyCode::Esc);
    // Days and weeks don't share a selection: a change between them starts at the newest.
    press(&mut app, KeyCode::Up);
    let day = app.list[app.sel()].key.clone();
    press(&mut app, KeyCode::Left);
    assert_eq!(app.list[app.sel()].key, day);
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.span, Span::All);
    assert_eq!(app.sel(), app.list.len() - 1);
    assert!(line(&frame(&app), 3).contains("Cost by week"));

    let mut app = App::new(data::tests::data());
    press(&mut app, KeyCode::Right);
    assert_eq!(app.span, Span::Days30);
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.span, Span::Today);
    assert!(screen(&app).contains("No activity in this period"));
    press(&mut app, KeyCode::Char('3'));
    assert!(screen(&app).contains("No activity in this period"));
  }
}
