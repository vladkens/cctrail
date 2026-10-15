// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

//! The interactive view: `cctrail` without arguments on a terminal. The history is parsed once;
//! every switch after that is in memory.

mod boxes;
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
  /// Its tables, each by the header of its names: a project's models, a day's projects, a
  /// model's speeds and efforts.
  pub parts: Vec<(&'static str, Vec<Row>)>,
  pub paths: Vec<String>,
  pub sort: Sort,
  /// Whether its rows count sessions: the projects of a day do.
  pub sessions: bool,
}

impl Detail {
  /// The rows of all its tables, in order; the selection counts through them.
  pub fn rows(&self) -> Vec<&Row> {
    self.parts.iter().flat_map(|x| &x.1).collect()
  }

  fn sort(&mut self, by: Sort) {
    self.sort = by;
    for part in &mut self.parts {
      sort(&mut part.1, by);
    }
  }
}

/// What a click on a choice on a border picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
  Period(Span),
  Provider,
  Tab(Tab),
  /// What the timeline's bars show.
  Bars(Col),
}

/// Where the last frame put what a click can hit.
#[derive(Clone, Debug, Default)]
pub struct Hits {
  /// The choices on the borders: periods, provider, tabs, the bars' column.
  pub clicks: Vec<(Rect, Click)>,
  /// The header rows of the tables and the sortable columns' x ranges.
  pub heads: Vec<Rect>,
  pub cols: Vec<(u16, u16, Col)>,
  /// The tables' rows: their area and the index of the first one shown; the first table's
  /// scrolling.
  pub rows: Vec<(Rect, usize)>,
  pub offset: usize,
  /// The timeline's bars and the rows they stand for.
  pub bars: Vec<(Rect, usize)>,
  /// The tab and whether a drill-down was drawn: what `offset` is of.
  pub view: Option<(Tab, bool)>,
  /// How far the help can scroll.
  pub help_max: u16,
}

impl Hits {
  /// The first row shown of a view the last frame drew, else none scrolled.
  pub fn offset_of(&self, view: (Tab, bool)) -> usize {
    if self.view == Some(view) { self.offset } else { 0 }
  }

  /// The rows a page holds: the first table's.
  fn page(&self) -> usize {
    self.rows.first().map_or(1, |x| usize::from(x.0.height).max(1))
  }
}

pub struct App {
  pub data: Data,
  pub span: Span,
  pub prov: Option<Provider>,
  /// Each tab's sort; the timeline's days newest first.
  pub sorts: [Sort; 3],
  /// What the timeline's bars show: time, tokens or cost.
  pub bars: Col,
  pub tab: Tab,
  sel: [usize; 3],
  pub open: Option<Detail>,
  /// The selected row of the drill-down.
  pub osel: usize,
  pub help: bool,
  /// The first line of the help shown, when it doesn't fit.
  pub help_top: u16,
  pub filter: String,
  pub typing: bool,
  /// The rows of the current tab, sorted and filtered, and the period's total.
  pub list: Vec<Row>,
  /// How many rows the tab has before the filter.
  pub all: usize,
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
      sorts: [Col::Time.sorted(), Col::Time.sorted(), (Col::Name, true)],
      bars: Col::Time,
      tab: Tab::Projects,
      sel: [0; 3],
      open: None,
      osel: 0,
      help: false,
      help_top: 0,
      filter: String::new(),
      typing: false,
      list: vec![],
      all: 0,
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
    let okept = self.open.as_ref().and_then(|d| d.rows().get(self.osel).map(|x| x.key.clone()));
    // Every project: the list scrolls, `other` is for the text report.
    let (projects, total) = self.data.projects(self.span, self.prov, true);
    self.total = total;
    self.unpriced = self.data.unpriced(self.span, self.prov);
    let mut list = match self.tab {
      Tab::Projects => projects,
      Tab::Models => self.data.models(self.span, self.prov, None),
      Tab::Timeline => self.data.timeline(self.span, self.prov, None),
    };
    sort(&mut list, self.sorts[self.tab as usize]);
    self.all = list.len();
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
      None if self.tab == Tab::Timeline => {
        list.iter().enumerate().max_by_key(|x| x.1.day).map_or(0, |x| x.0)
      }
      None => (*sel).min(list.len().saturating_sub(1)),
    };
    self.list = list;
    self.shown = Some(now);
    // An open drill-down follows the period and provider: its row as it is now, or closed.
    if let Some(open) = &self.open {
      let sort = open.sort;
      let row = self.list.iter().find(|x| x.key == open.row.key).cloned();
      self.open = row.map(|x| self.detail(x, sort));
      let rows = self.open.as_ref().map_or(vec![], |x| x.rows());
      let found = okept.and_then(|k| rows.iter().position(|x| x.key == k));
      self.osel = found.unwrap_or(self.osel).min(rows.len().saturating_sub(1));
    }
  }

  fn detail(&self, row: Row, sort: Sort) -> Detail {
    let (span, prov) = (self.span, self.prov);
    let day = row.day.unwrap_or(self.data.today);
    let parts = match self.tab {
      Tab::Projects => vec![("Model", self.data.models(span, prov, Some(&row.key)))],
      Tab::Models => {
        let [speed, effort] = self.data.model_usage(span, prov, &row.key);
        vec![("Speed", speed), ("Effort", effort)]
      }
      Tab::Timeline => vec![("Project", self.data.bucket_projects(span, prov, day))],
    };
    let sessions = self.tab == Tab::Timeline;
    let sort = if sort.0 == Col::Sessions && !sessions { Col::Time.sorted() } else { sort };
    let mut row = row;
    if sessions {
      row.sessions = Some(parts[0].1.iter().filter_map(|x| x.sessions).sum());
    }
    let project = self.tab == Tab::Projects;
    let paths = if project { self.data.paths(&row.key) } else { vec![] };
    let mut d = Detail { parts, paths, row, sort, sessions };
    d.sort(sort);
    d
  }

  /// Opens the selected row; an empty day opens nothing.
  fn open(&mut self) {
    let row = self.list.get(self.sel());
    if let Some(row) = row.filter(|x| x.tokens > 0 || x.active.unwrap_or(0) > 0) {
      let sort = self.sorts[self.tab as usize];
      self.open = Some(self.detail(row.clone(), sort));
      self.osel = 0;
    }
  }

  /// The selection of the list or the drill-down and its rows' count.
  fn selection(&mut self) -> (&mut usize, usize) {
    match &self.open {
      Some(d) => (&mut self.osel, d.rows().len()),
      None => (&mut self.sel[self.tab as usize], self.list.len()),
    }
  }

  /// Moves the selection by a key: ↑ / ↓ wrap around at the ends, PgUp / PgDn and Home / End
  /// stop there, a page is the rows on screen.
  fn nav(&mut self, code: KeyCode) {
    let page = self.hits.borrow().page();
    let (sel, len) = self.selection();
    if let Some(i) = step(code, *sel, page, len) {
      *sel = i;
    }
  }

  /// Sorts what is on screen by `sort`: a drill-down keeps its selected row, a list rebuilds.
  fn sort_by(&mut self, by: Sort) {
    match &mut self.open {
      Some(d) => {
        let key = d.rows().get(self.osel).map(|x| x.key.clone());
        d.sort(by);
        self.osel = d.rows().iter().position(|x| Some(&x.key) == key.as_ref()).unwrap_or(0);
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
      (None, Tab::Timeline) => {
        return self.bars =
          [Col::Tokens, Col::Cost, Col::Time][(self.bars as usize).clamp(1, 3) - 1];
      }
      (Some(d), _) => col.next(d.sessions).sorted(),
      (None, tab) => col.next(tab != Tab::Models).sorted(),
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
    // The help takes every key while open, as macmon's: Esc, ←, `?` and `q` close it, ↑ ↓
    // scroll it when it doesn't fit.
    if self.help {
      match key.code {
        KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('?' | 'q') => {
          self.help = false;
        }
        KeyCode::Up => self.scroll_help(-1),
        KeyCode::Down => self.scroll_help(1),
        _ => {}
      }
      return;
    }
    // `q` in a drill-down goes back like Esc and ←; it quits only from the lists.
    let back = matches!(key.code, KeyCode::Esc | KeyCode::Backspace | KeyCode::Left)
      || (key.code == KeyCode::Char('q') && self.open.is_some());
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
      // Moves and toggles that change no data return without rebuilding the rows.
      _ if back && self.open.is_some() => return self.open = None,
      _ if back && self.filter.is_empty() => return,
      _ if back => self.filter.clear(),
      KeyCode::Char('q') => return self.quit = true,
      KeyCode::Char('?') => return (self.help, self.help_top) = (true, 0),
      KeyCode::Char(c @ '1'..='3') => self.tab(c as usize - '1' as usize),
      KeyCode::Char('T') => self.span = Span::ALL[(self.span as usize + Span::ALL.len() - 1) % 5],
      KeyCode::Char('t') if shift => {
        self.span = Span::ALL[(self.span as usize + Span::ALL.len() - 1) % 5];
      }
      KeyCode::Char('t') => self.span = Span::ALL[(self.span as usize + 1) % Span::ALL.len()],
      KeyCode::Char('p') => self.next_provider(),
      KeyCode::Char('S') => return self.reverse(),
      KeyCode::Char('s') if shift => return self.reverse(),
      KeyCode::Char('s') => return self.next_sort(),
      KeyCode::Char('/') => return self.typing = true,
      KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
        return self.nav(key.code);
      }
      KeyCode::Home | KeyCode::End => return self.nav(key.code),
      // → goes deeper, as Enter.
      KeyCode::Enter | KeyCode::Right if self.open.is_none() => return self.open(),
      _ => return,
    }
    self.refresh();
  }

  /// `S`: the other direction.
  fn reverse(&mut self) {
    let (col, most) = self.sort();
    self.sort_by((col, !most));
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

  /// Moves the selection `rows` rows (negative: up), past an end around to the other as ↑ ↓ do,
  /// and scrolls the list as much, so the selected row keeps its place on screen until the list
  /// hits its top or end.
  fn wheel(&mut self, rows: isize) {
    let (sel, len) = self.selection();
    let Some(last) = len.checked_sub(1) else { return };
    let from = *sel;
    let to = match (from, rows < 0) {
      (0, true) => last,
      (_, false) if from == last => 0,
      _ => from.saturating_add_signed(rows).min(last),
    };
    *sel = to;
    let mut hits = self.hits.borrow_mut();
    let page = hits.page();
    hits.offset = ((hits.offset + to).saturating_sub(from)).min(len.saturating_sub(page));
  }

  /// Scrolls the help by `lines`, as far as the last frame left lines below it.
  fn scroll_help(&mut self, lines: isize) {
    let max = self.hits.borrow().help_max;
    self.help_top = self.help_top.saturating_add_signed(lines as i16).min(max);
  }

  /// Clicks on periods, the provider, tabs, column headers, rows and bars; the wheel scrolls; a
  /// click on the selected row or bar opens it (so does a double click), anywhere while the help
  /// is open closes it.
  pub fn mouse(&mut self, ev: MouseEvent) {
    let at = Position::new(ev.column, ev.row);
    match ev.kind {
      // Over the help, the wheel scrolls it, not the list under it.
      MouseEventKind::ScrollUp if self.help => return self.scroll_help(-WHEEL_ROWS),
      MouseEventKind::ScrollDown if self.help => return self.scroll_help(WHEEL_ROWS),
      MouseEventKind::ScrollUp => return self.wheel(-WHEEL_ROWS),
      MouseEventKind::ScrollDown => return self.wheel(WHEEL_ROWS),
      MouseEventKind::Down(MouseButton::Left) => {}
      _ => return,
    }
    if self.help {
      return self.help = false;
    }
    let hits = self.hits.borrow().clone();
    if let Some(&(_, click)) = hits.clicks.iter().find(|x| x.0.contains(at)) {
      match click {
        Click::Period(span) => self.span = span,
        Click::Provider => self.next_provider(),
        Click::Tab(tab) => self.tab(tab as usize),
        Click::Bars(col) => return self.bars = col,
      }
    } else if hits.heads.iter().any(|x| x.contains(at)) {
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
    } else if let Some((rect, first)) = hits.rows.iter().find(|x| x.0.contains(at)) {
      let i = first + usize::from(at.y - rect.y);
      let (sel, len) = match &self.open {
        Some(d) => (self.osel, d.rows().len()),
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

/// Rows one wheel step moves the selection and scrolls the list, as macmon's.
const WHEEL_ROWS: isize = 3;
/// The least time between two wheel steps.
const WHEEL_EVERY: std::time::Duration = std::time::Duration::from_millis(30);

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

/// Rows by a column, most first or least first; ties by name.
fn sort(rows: &mut [Row], (col, most): Sort) {
  rows.sort_by(|a, b| {
    let order = match col {
      Col::Name => a.label.to_lowercase().cmp(&b.label.to_lowercase()),
      _ => col.of(a).partial_cmp(&col.of(b)).unwrap(),
    };
    let order = if most { order.reverse() } else { order };
    order.then_with(|| a.label.cmp(&b.label))
  });
}

/// Applies a batch of queued events: keys and clicks in order, the wheel's steps folded into one,
/// the last one's direction, and that only `WHEEL_EVERY` after the last step (`wheeled`).
fn pump(
  app: &mut App,
  events: Vec<Event>,
  wheeled: &mut Option<std::time::Instant>,
  now: std::time::Instant,
) {
  let mut wheel = None;
  for ev in events {
    match ev {
      Event::Key(key) => app.key(key),
      Event::Mouse(ev)
        if matches!(ev.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) =>
      {
        wheel = Some(ev);
      }
      Event::Mouse(ev) => app.mouse(ev),
      _ => {}
    }
  }
  if let Some(ev) = wheel.filter(|_| wheeled.is_none_or(|x| now - x >= WHEEL_EVERY)) {
    app.mouse(ev);
    *wheeled = Some(now);
  }
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
    let mut wheeled = None;
    while !app.quit {
      term.draw(|f| draw::draw(f, &app))?;
      // The queued events all before the next frame.
      let mut events = vec![event::read()?];
      while event::poll(std::time::Duration::ZERO)? {
        events.push(event::read()?);
      }
      pump(&mut app, events, &mut wheeled, std::time::Instant::now());
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
    mouse(app, MouseEventKind::Down(MouseButton::Left), x, y);
  }

  fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    app.mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
  }

  fn wheel(app: &mut App, kind: MouseEventKind) {
    mouse(app, kind, 0, 0);
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
    (0..buf.area.height).map(|y| raw(buf, y) + "\n").collect()
  }

  fn screen(app: &App) -> String {
    screen_of(&frame(app))
  }

  /// A whole line, borders included.
  fn raw(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
  }

  /// A line inside the box: without its border cells.
  fn line(buf: &Buffer, y: u16) -> String {
    (1..buf.area.width - 1).map(|x| buf[(x, y)].symbol()).collect()
  }

  /// The column of `text` on line `y`.
  fn col(buf: &Buffer, y: u16, text: &str) -> u16 {
    let l = raw(buf, y);
    let x = l.find(text).unwrap_or_else(|| panic!("{text:?} not in {l:?}"));
    l[..x].chars().count() as u16
  }

  /// The fg of the first cell of `text` on line `y`.
  fn fg(buf: &Buffer, y: u16, text: &str) -> Color {
    buf[(col(buf, y, text), y)].fg
  }

  /// The first line inside the list (or chart) box with `text`.
  fn find(buf: &Buffer, text: &str) -> u16 {
    (4..buf.area.height - 1)
      .find(|y| line(buf, *y).contains(text))
      .unwrap_or_else(|| panic!("{text:?} in {}", screen_of(buf)))
  }

  /// The cells of a table line: runs split by two or more spaces.
  fn cells(line: &str) -> Vec<String> {
    let line = line.trim_start().trim_start_matches('▶');
    line.split("  ").map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
  }

  /// The fixtures, all of their history.
  fn app() -> App {
    let mut app = App::new(data::tests::data());
    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Char('t'));
    app
  }

  /// Many projects, all of their history.
  fn many() -> App {
    let mut app = App::new(data::tests::many());
    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Char('t'));
    app
  }

  #[test]
  fn draws_the_look_of_the_checklist() {
    let mut app = app();
    assert_eq!(app.span, Span::All);
    let buf = frame(&app);
    let out = screen_of(&buf);
    // Boxes as macmon's: the header with the views and the version on its border, the periods
    // and the provider inside; the list's box with its name, the filter, `sort` and `→ open` on
    // its top border, the keys on its bottom one; the totals as the last `total` row.
    let (top, bottom) = (raw(&buf, 0), raw(&buf, 29));
    assert!(
      top.starts_with("╭─ Projects 1  Models 2  Timeline 3 ") && top.ends_with("─╮"),
      "{top}"
    );
    assert!(top.contains(concat!(" cctrail v", env!("CARGO_PKG_VERSION"), " ")), "{top}");
    assert!(!raw(&buf, 1).contains('$') && !top.contains('$'), "{out}");
    let title = raw(&buf, 3);
    assert!(title.starts_with("╭─ projects 1 ─ / filter ─ sort ─"), "{title}");
    assert!(title.ends_with(" → open ─╮"), "{title}");
    assert!(
      bottom.starts_with("╰─") && bottom.contains("? help | quit | t period | provider"),
      "{bottom}"
    );
    // The filter once, on the list's title.
    assert_eq!(out.matches("filter").count(), 1, "{out}");
    let sel = find(&buf, "▶");
    let total = find(&buf, "total");
    assert_eq!(total, sel + app.list.len() as u16, "{out}");
    assert!(buf[(4, total)].modifier.contains(Modifier::BOLD));
    // No bars, percentages only; numbers right-aligned up to an equal right gutter.
    assert!(!out.contains('█') && out.contains('%'), "{out}");
    for y in [sel - 1, sel, total] {
      let l = line(&buf, y);
      assert_eq!(l.chars().count() - l.trim_end().chars().count(), 2, "gutter on {l:?}");
    }
    assert!(line(&buf, sel - 1).trim_end().ends_with("Share"));
    // Selected row: one subtle bar inside the box, nothing reversed, colors kept.
    for x in 1..buf.area.width - 1 {
      assert_eq!(buf[(x, sel)].bg, Color::Indexed(236), "column {x}");
      assert!(!buf[(x, sel)].modifier.contains(Modifier::REVERSED), "column {x}");
    }
    let row = &app.list[0];
    assert_eq!(fg(&buf, sel, &row.label), Color::Reset);
    assert_eq!(fg(&buf, sel, &crate::output::duration(row.active.unwrap())), Color::Yellow);
    assert_eq!(fg(&buf, sel, "$"), Color::Green);
    // Headers bold, the sorted one in the text color, the others gray, as macmon's.
    assert_eq!(fg(&buf, sel - 1, "Time ↓"), Color::Reset);
    assert_eq!(fg(&buf, sel - 1, "Tokens"), Color::DarkGray);
    assert!(buf[(col(&buf, sel - 1, "Tokens"), sel - 1)].modifier.contains(Modifier::BOLD));
    // The picked tab and period: bold cyan, the others gray, no filled block; the border gray.
    assert!((0..buf.area.width).all(|x| !buf[(x, 0)].modifier.contains(Modifier::REVERSED)));
    assert_eq!(fg(&buf, 1, "All"), Color::Cyan);
    assert_eq!(fg(&buf, 1, "Week"), Color::DarkGray);
    assert_eq!(fg(&buf, 1, "claude+codex"), Color::Reset);
    assert_eq!(fg(&buf, 0, "cctrail v"), Color::Reset);
    assert_eq!(fg(&buf, 0, "Projects"), Color::Cyan);
    assert_eq!(fg(&buf, 0, "Models"), Color::DarkGray);
    assert_eq!(fg(&buf, 0, "2"), Color::Cyan);
    assert_eq!((fg(&buf, 0, "╭"), fg(&buf, 29, "╰")), (Color::DarkGray, Color::DarkGray));
    // Hints as macmon's: the key in cyan, in its word where it is one, the rest faint.
    assert_eq!(fg(&buf, 29, "quit"), Color::Cyan);
    let x = col(&buf, 29, "quit") + 1;
    assert_eq!(buf[(x, 29)].modifier, Modifier::DIM);
    assert_eq!(fg(&buf, 29, "| quit"), Color::DarkGray);

    // A project's drill-down takes the box: where it is on the top border, its paths on the
    // bottom one, its models with time summing to the project's, no chart.
    press(&mut app, KeyCode::Enter);
    let buf = frame(&app);
    let out = screen_of(&buf);
    assert!(raw(&buf, 3).contains(" oss/demo › models 4 ─ sort "), "{out}");
    assert!(raw(&buf, 29).contains(" paths "), "{out}");
    assert!(!out.contains('█'), "{out}");
    let d = app.open.as_ref().unwrap();
    let times: i64 = d.rows().into_iter().map(|x| x.active.unwrap()).sum();
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
    // The help is a box over the screen, without hints under it.
    press(&mut app, KeyCode::Char('?'));
    let buf = frame(&app);
    let out = screen_of(&buf);
    assert!(out.matches('╭').count() == 3 && out.contains(" help "), "{out}");
    assert!(!raw(&buf, 29).contains("quit"), "{out}");
    assert_eq!(out.matches(" q ").count(), 1, "{out}");
  }

  #[test]
  fn arrows_esc_and_q_go_deeper_and_back() {
    let mut app = app();
    // → opens, ← goes back, as Enter and Esc.
    press(&mut app, KeyCode::Right);
    assert!(app.open.is_some());
    press(&mut app, KeyCode::Right);
    assert!(app.open.is_some());
    press(&mut app, KeyCode::Left);
    assert!(app.open.is_none());
    press(&mut app, KeyCode::Left);
    assert!(app.open.is_none() && !app.quit);
    // The help takes every key while open; Esc, `?` and `q` close it.
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Char('2'));
    assert_eq!((app.help, app.tab), (true, Tab::Projects));
    press(&mut app, KeyCode::Char('q'));
    assert!(!app.help && !app.quit);
    // `q` goes back from a drill-down and quits only from a list.
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.open.is_none() && !app.quit);
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
    assert!(app.list.windows(2).all(|w| w[0].active >= w[1].active));
    let order = [Col::Tokens, Col::Cost, Col::Sessions, Col::Name, Col::Time];
    for col in order {
      press(&mut app, KeyCode::Char('s'));
      assert_eq!(app.sort(), (col, col != Col::Name));
    }
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Char('S'));
    assert_eq!(app.sort(), (Col::Tokens, false));
    assert_eq!(head(&app)[2], "Tokens ↑");
    assert!(app.list.windows(2).all(|w| w[0].tokens <= w[1].tokens));
    // The share is of the sorted column: tokens here.
    let buf = frame(&app);
    let share =
      |y| cells(&line(&buf, y)).pop().unwrap().trim_end_matches('%').parse::<u32>().unwrap();
    let first = find(&buf, "Project") + 1;
    let i = app.list.iter().position(|x| x.key == "w/api").unwrap();
    let tokens = app.list[i].tokens as f64 / app.total.tokens as f64 * 100.0;
    assert!((share(first + i as u16) as f64 - tokens).abs() <= 1.0);
    // By name, the share is of the time; models have no sessions to sort by.
    press(&mut app, KeyCode::Char('2'));
    for col in [Col::Tokens, Col::Cost, Col::Name, Col::Time] {
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
  fn lists_wrap_around_page_and_scroll_like_macmon() {
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
    // Periods wrap around too.
    press(&mut app, KeyCode::Char('t'));
    assert_eq!(app.span, Span::Today);
    press(&mut app, KeyCode::Char('T'));
    assert_eq!(app.span, Span::All);
    // Five rows in a list four high: a scrollbar on the right border, ↑ and ↓ at its ends; the
    // wheel moves three rows and the list as much, the selected row keeping its place until the
    // end, then around to the top; the other mouse events do nothing.
    let bar = |buf: &Buffer| -> String {
      (0..buf.area.height).map(|y| buf[(buf.area.width - 1, y)].symbol().to_string()).collect()
    };
    assert!(!bar(&frame_of(&app, 80, 24)).contains('┃'));
    let buf = frame_of(&app, 80, 11);
    let edge = bar(&buf);
    assert!(edge.contains("↑┃") && edge.contains('↓'), "{}", screen_of(&buf));
    wheel(&mut app, MouseEventKind::ScrollDown);
    assert_eq!(app.sel(), 3);
    let buf = frame_of(&app, 80, 11);
    assert_eq!(app.hits.borrow().offset, 1);
    assert!(line(&buf, 7).trim_start().starts_with('▶'), "{}", screen_of(&buf));
    wheel(&mut app, MouseEventKind::ScrollDown);
    wheel(&mut app, MouseEventKind::ScrollDown);
    assert_eq!(app.sel(), 0);
    wheel(&mut app, MouseEventKind::ScrollUp);
    assert_eq!(app.sel(), last);
    for kind in [
      MouseEventKind::Up(MouseButton::Left),
      MouseEventKind::Drag(MouseButton::Left),
      MouseEventKind::Moved,
      MouseEventKind::Down(MouseButton::Right),
    ] {
      mouse(&mut app, kind, 4, 3);
      assert_eq!((app.sel(), app.open.is_none()), (last, true), "{kind:?}");
    }
  }

  #[test]
  fn clicks_the_borders_choices_headers_and_rows() {
    let mut app = app();
    let buf = frame(&app);
    click(&mut app, col(&buf, 1, "Week"), 1);
    assert_eq!(app.span, Span::Week);
    click(&mut app, col(&buf, 1, "All"), 1);
    click(&mut app, col(&buf, 1, "claude+codex"), 1);
    assert_eq!(app.prov, Some(Provider::Claude));
    let buf = frame(&app);
    click(&mut app, col(&buf, 1, "claude") + 1, 1);
    click(&mut app, col(&buf, 1, "claude") + 1, 1);
    assert_eq!(app.prov, None);
    click(&mut app, col(&buf, 0, "Models 2"), 0);
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
    // A row selects; the selected row opens; its tab on the top border goes back.
    let buf = frame(&app);
    let head = find(&buf, "Tokens");
    click(&mut app, 4, head + 2);
    assert_eq!(app.sel(), 1);
    click(&mut app, 4, head + 2);
    assert!(app.open.is_some());
    let buf = frame(&app);
    click(&mut app, col(&buf, 3, "›"), 3);
    assert!(app.open.is_none());
  }

  #[test]
  fn switches_views_and_drills_down() {
    let mut app = app();
    press(&mut app, KeyCode::Enter);
    let out = screen(&app);
    assert!(out.contains("claude/claude-sonnet-5") && out.contains(data::tests::REPO), "{out}");
    let models = app.open.as_ref().unwrap().rows().len();
    press(&mut app, KeyCode::Down);
    assert_eq!(app.osel, 1.min(models - 1));
    // A drill-down sorts on its own, keeping its selected row.
    let key = app.open.as_ref().unwrap().rows()[app.osel].key.clone();
    press(&mut app, KeyCode::Char('s'));
    let d = app.open.as_ref().unwrap();
    assert_eq!((d.sort, &d.rows()[app.osel].key), ((Col::Tokens, true), &key));
    assert_eq!(app.sorts[0], (Col::Time, true));
    // The opened row follows the provider: codex alone has no oss/demo models from claude.
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Char('p'));
    let open = app.open.as_ref().unwrap();
    assert!(open.rows().into_iter().all(|x| x.key.starts_with("codex/")));
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Esc);
    assert!(app.open.is_none());

    press(&mut app, KeyCode::Char('2'));
    assert!(screen(&app).contains("codex/gpt-5.4"));
    press(&mut app, KeyCode::Enter);
    let out = screen(&app);
    assert!(out.contains("Speed") && out.contains("Effort") && out.contains("standard"), "{out}");
    press(&mut app, KeyCode::Char('3'));
    assert!(app.list.iter().all(|x| x.day.is_some()));
  }

  #[test]
  fn a_model_shows_its_speeds_and_efforts() {
    let mut app = many();
    press(&mut app, KeyCode::Char('2'));
    // One row per model: fast and standard together.
    assert_eq!(app.list.iter().filter(|x| x.key.contains("gpt-5.4")).count(), 1);
    app.sel[1] = app.list.iter().position(|x| x.key == "codex/gpt-5.4").unwrap();
    press(&mut app, KeyCode::Enter);
    let buf = frame(&app);
    let out = screen_of(&buf);
    // A speed table and an effort table, each totalling the model.
    let (speed, effort) = (find(&buf, "Speed"), find(&buf, "Effort"));
    assert!(speed < effort && !out.contains("w/api"), "{out}");
    let names = |from: u16, to: u16| {
      (from + 1..to).map(|y| cells(&line(&buf, y))[0].clone()).collect::<Vec<_>>()
    };
    let mut speeds = names(speed, effort - 2);
    speeds.sort();
    assert_eq!(speeds, ["fast", "standard"], "{out}");
    let total = (effort + 1..buf.area.height).find(|y| line(&buf, *y).contains("total")).unwrap();
    let mut efforts = names(effort, total);
    efforts.sort();
    assert_eq!(efforts, ["high", "low"], "{out}");
    let model = crate::output::duration(app.open.as_ref().unwrap().row.active.unwrap());
    assert_eq!(cells(&line(&buf, total))[1], model);
    // The selection goes through both tables.
    press(&mut app, KeyCode::End);
    assert_eq!(app.osel, 3);
    assert!(line(&frame(&app), total - 1).trim_start().starts_with('▶'));
    // An effort that isn't logged reads `–`.
    press(&mut app, KeyCode::Esc);
    app.sel[1] = app.list.iter().position(|x| x.key == "claude/claude-unknown-1").unwrap();
    press(&mut app, KeyCode::Enter);
    let buf = frame(&app);
    assert_eq!(cells(&line(&buf, find(&buf, "Effort") + 1))[0], "–");
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
      // Every project, `other` only in the text report.
      let out = screen_of(&buf);
      assert!(out.contains("/tmp/b") && !out.contains("other"), "{out}");
      let long = "w/a-project-with-a-name-too-long-for-a-narrow-terminal";
      assert_eq!(out.contains(long), width == 140, "{width}");
      assert!(width == 140 || line(&buf, find(&buf, "w/a-project-wi")).contains('…'));
      // The header keeps the views, periods, provider and version; the list's bottom border the
      // keys right and the unpriced note left, cut to fit; its title the filter.
      let (top, bottom) = (raw(&buf, 0), raw(&buf, height - 1));
      assert!(top.contains("Timeline 3") && top.contains("cctrail v"), "{top}");
      assert!(raw(&buf, 1).contains("All") && raw(&buf, 1).contains("claude+codex"));
      assert!(bottom.contains("? help | quit") && raw(&buf, 3).contains("/ filter"), "{bottom}");
      assert!(bottom.contains(" unpriced 37.8"), "{bottom}");
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
      for (y, row) in (head + 1..total).zip(d.rows()) {
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
        _ => press(&mut app, KeyCode::Char('T')),
      }
      assert_eq!(app.list[app.sel()].key, key, "after {change}");
    }

    let mut app = many();
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('w'));
    press(&mut app, KeyCode::Enter);
    assert!(app.list.len() > 1 && app.list.len() < many().list.len());
    assert_eq!(app.total.tokens, app.list.iter().map(|x| x.tokens).sum::<u64>());
    // The typed filter shows once, on the list's title with the count of its rows.
    let buf = frame(&app);
    assert!(raw(&buf, 3).starts_with("╭─ projects 3/5 ─ /w ─ sort "), "{}", raw(&buf, 3));
    assert_eq!(screen_of(&buf).matches(" /w").count(), 1, "{}", screen_of(&buf));
    // A filter with no rows says so.
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('z'));
    assert!(app.list.is_empty());
    assert!(screen(&app).contains("No match for \"wz\""), "{}", screen(&app));
  }

  #[test]
  fn timeline_charts_the_models_over_its_days() {
    let mut app = many();
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Char('T')); // 30 days
    // The days newest first, the newest selected.
    assert_eq!((app.list.len(), app.sel()), (30, 0));
    assert_eq!(app.list[0].day.unwrap(), jiff::civil::date(2026, 1, 10));
    let buf = frame(&app);
    let out = screen_of(&buf);
    // The chart's box: on its title a choice of what the bars show, the largest value, the
    // legend without the `claude-` of the names.
    let title = raw(&buf, 3);
    assert!(title.starts_with("╭─ Time  Tokens  Cost ─ by day · max "), "{out}");
    assert_eq!((fg(&buf, 3, "Time"), fg(&buf, 3, "Tokens")), (Color::Cyan, Color::DarkGray));
    for model in ["sonnet-5", "unknown-1", "gpt-5.4"] {
      assert!(title.contains(&format!("■ {model}")), "{out}");
    }
    let max = app.list.iter().filter_map(|x| x.active).max().unwrap();
    assert!(title.contains(&format!("max {}", crate::output::duration(max))), "{out}");
    // The days' box under it: its name, count and `sort` on its title, a table with its total.
    let list =
      (4..29).find(|y| raw(&buf, *y).starts_with("╭─ days 30 ─ / filter ─ sort ")).unwrap();
    assert_eq!(list, 16, "{out}");
    let divider = list;
    let head = cells(&line(&buf, divider + 1));
    assert_eq!(head, ["Day ↓", "Time", "Tokens", "Cost", "Sessions", "Share"], "{out}");
    let newest = cells(&line(&buf, divider + 2));
    assert_eq!(newest, ["2026-01-10 Sa", "–", "–", "–", "–", "–"], "{out}");
    assert!(line(&buf, 28).trim_start().starts_with("total"), "{out}");
    // One bar per busy day, all as wide, one cell apart, stacked in the models' colors.
    let axis = (4..divider).find(|y| line(&buf, *y).contains('━')).unwrap();
    let bars = line(&buf, axis - 1);
    let blocks = |t: &str| !t.is_empty() && t.chars().all(|c| "▁▂▃▄▅▆▇█".contains(c));
    let busy = app.list.iter().filter(|x| x.tokens > 0).count();
    let widths: Vec<usize> =
      bars.split(' ').filter(|t| blocks(t)).map(|t| t.chars().count()).collect();
    assert!(widths.len() == busy && widths.windows(2).all(|w| w[0] == w[1]), "{out}");
    let from = bars.find(|c| "▁▂▃▄▅▆▇█".contains(c)).unwrap();
    let to = bars.rfind(|c| "▁▂▃▄▅▆▇█".contains(c)).unwrap();
    assert!(!bars[from..to].contains("  "), "{out}");
    let cells_of = (4..axis).flat_map(|y| (1..99).map(move |x| (x, y)));
    let colors: std::collections::HashSet<Color> =
      cells_of.filter(|p| "▁▂▃▄▅▆▇█".contains(buf[*p].symbol())).map(|p| buf[p].fg).collect();
    assert!(colors.len() >= 2 && colors.iter().all(|c| matches!(c, Color::Indexed(_))), "{out}");
    // The newest bar at the right gutter; empty, yet marked on the axis, its date under it.
    let axis_line = line(&buf, axis);
    assert!(axis_line.ends_with("━  ") && fg(&buf, axis, "━") == Color::Cyan, "{out}");
    assert!(line(&buf, axis + 1).trim_end().ends_with("01-10"), "{out}");
    // ↓ selects the day before: its bar marked with the selection's background.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.list[app.sel()].day.unwrap(), jiff::civil::date(2026, 1, 8));
    let buf = frame(&app);
    let (rect, _) = *app.hits.borrow().bars.iter().find(|x| x.1 == app.sel()).unwrap();
    let low = rect.bottom() - 3;
    assert_eq!((buf[(rect.x, low)].fg, buf[(rect.x, low)].bg), (Color::Reset, Color::Reset));
    // `s` or a click on the choice picks what the bars show.
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Char('s'));
    assert_eq!(app.bars, Col::Cost);
    let buf = frame(&app);
    assert_eq!(fg(&buf, 3, "Cost"), Color::Cyan);
    click(&mut app, col(&buf, 3, "Tokens"), 3);
    assert_eq!(app.bars, Col::Tokens);
    // A header sorts the days; the chart keeps them in order and the selection follows.
    let key = app.list[app.sel()].key.clone();
    let buf = frame(&app);
    click(&mut app, col(&buf, divider + 1, "Time"), divider + 1);
    assert_eq!(app.list[app.sel()].key, key);
    assert!(app.list.windows(2).all(|w| w[0].active >= w[1].active));
    // A click on a bar selects its day, a second one opens it; ← back, → opens.
    frame(&app);
    let (rect, i) = *app
      .hits
      .borrow()
      .bars
      .iter()
      .find(|x| x.1 != app.sel() && app.list[x.1].tokens > 0)
      .unwrap();
    click(&mut app, rect.x, rect.bottom() - 1);
    assert_eq!(app.sel(), i);
    click(&mut app, rect.x, rect.bottom() - 1);
    let d = app.open.as_ref().unwrap();
    assert_eq!(d.row.sessions, Some(d.rows().into_iter().filter_map(|x| x.sessions).sum()));
    assert_eq!(d.row.sessions, app.list[i].sessions);
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Right);
    assert!(app.open.is_some());
    press(&mut app, KeyCode::Left);
    // Days and weeks don't share a selection: a change between them starts at the newest.
    press(&mut app, KeyCode::Char('t'));
    assert_eq!(app.span, Span::All);
    assert_eq!(app.list[app.sel()].day, app.list.iter().filter_map(|x| x.day).max());
    assert!(line(&frame(&app), 3).contains("by week"));

    let mut app = App::new(data::tests::data());
    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Char('t'));
    assert_eq!(app.span, Span::Today);
    assert!(screen(&app).contains("No activity in this period"));
    press(&mut app, KeyCode::Char('3'));
    assert!(screen(&app).contains("No activity in this period"));
  }

  #[test]
  fn small_screens_say_so() {
    let mut app = many();
    assert!(screen_of(&frame_of(&app, 80, 7)).contains("too small"));
    assert!(!screen_of(&frame_of(&app, 80, 8)).contains("too small"));
    press(&mut app, KeyCode::Char('3'));
    assert!(screen_of(&frame_of(&app, 80, 16)).contains("too small"));
    assert!(!screen_of(&frame_of(&app, 80, 17)).contains("too small"));
  }

  #[test]
  fn the_wheel_steps_once_per_batch_and_not_too_often() {
    let mut app = many();
    let spin = |n| {
      let ev = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
      };
      vec![Event::Mouse(ev); n]
    };
    let now = std::time::Instant::now();
    let mut wheeled = None;
    pump(&mut app, spin(10), &mut wheeled, now);
    assert_eq!(app.sel(), 3);
    pump(&mut app, spin(1), &mut wheeled, now + std::time::Duration::from_millis(10));
    assert_eq!(app.sel(), 3);
    pump(&mut app, spin(1), &mut wheeled, now + std::time::Duration::from_millis(40));
    assert_eq!(app.sel(), 4);
  }

  /// The color of the legend's square for `model` on the chart's title.
  fn square(buf: &Buffer, model: &str) -> Color {
    fg(buf, 3, &format!("■ {model}"))
  }

  #[test]
  fn models_keep_their_colors_and_the_legend_names_them() {
    let mut app = many();
    press(&mut app, KeyCode::Char('3'));
    let buf = frame(&app);
    let models = ["gpt-5.4", "sonnet-5", "unknown-1"];
    let colors: Vec<Color> = models.map(|m| square(&buf, m)).to_vec();
    // The same colors whatever the bars show, the period and the provider (a model without a
    // price has no cost to show).
    for key in ['s', 's', 'T', 't', 't', 'p', 'p'] {
      press(&mut app, KeyCode::Char(key));
      let buf = frame(&app);
      for (model, color) in models.iter().zip(&colors) {
        if raw(&buf, 3).contains(&format!("■ {model} ")) || raw(&buf, 3).ends_with(model) {
          assert_eq!(square(&buf, model), *color, "{key}: {}", screen_of(&buf));
        }
      }
    }
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Char('T'));
    press(&mut app, KeyCode::Char('s'));
    assert_eq!((app.span, app.prov, app.bars), (Span::All, None, Col::Time));
    // Narrow, the line gives up words for the legend, which names models in full (fewer, the
    // rest in `other`) and every color in the bars; never `other` alone.
    for width in [44, 52, 60, 80] {
      let buf = frame_of(&app, width, 30);
      let named: std::collections::HashSet<Color> =
        (1..width - 1).filter(|x| buf[(*x, 3)].symbol() == "■").map(|x| buf[(x, 3)].fg).collect();
      let drawn: std::collections::HashSet<Color> = (4..16)
        .flat_map(|y| (1..width - 1).map(move |x| (x, y)))
        .filter(|p| "▁▂▃▄▅▆▇█".contains(buf[*p].symbol()) && buf[*p].fg != Color::Reset)
        .map(|p| buf[p].fg)
        .collect();
      assert!(drawn.is_subset(&named), "{width}: {}", screen_of(&buf));
      let legend = raw(&buf, 3);
      let squares = legend.matches('■').count();
      assert!(squares != 1 || !legend.contains("■ other"), "{width}: {legend}");
      assert!(width < 60 || squares >= 2, "{width}: {legend}");
      assert!(!legend.contains("  ·"), "{width}: {legend}");
    }
  }

  #[test]
  fn narrow_tables_drop_columns_and_never_cut_numbers() {
    let app = many();
    for (width, gone) in [(60, &["Sessions", "Share"][..]), (44, &["Sessions", "Share", "Tokens"])]
    {
      let buf = frame_of(&app, width, 24);
      let head = cells(&line(&buf, find(&buf, "Project")));
      assert!(gone.iter().all(|x| !head.contains(&x.to_string())), "{width}: {head:?}");
      assert!(head.contains(&"Time ↓".to_string()), "{width}: {head:?}");
      let y = find(&buf, "w/api");
      let row = cells(&line(&buf, y));
      assert_eq!(row[1], crate::output::duration(app.list[0].active.unwrap()), "{width}");
      assert!(row[0] == "w/api" && line(&buf, find(&buf, "total")).contains("18h 36m"));
    }
  }

  #[test]
  fn the_help_scrolls_and_closes_with_left() {
    let mut app = many();
    press(&mut app, KeyCode::Char('?'));
    let buf = frame_of(&app, 60, 20);
    assert!(!screen_of(&buf).contains("quits from a list"), "{}", screen_of(&buf));
    for _ in 0..20 {
      press(&mut app, KeyCode::Down);
    }
    let buf = frame_of(&app, 60, 20);
    assert!(screen_of(&buf).contains("quits from a list"), "{}", screen_of(&buf));
    // The wheel scrolls the help, not the list under it.
    let top = app.help_top;
    wheel(&mut app, MouseEventKind::ScrollUp);
    assert!(top > 0 && app.help_top < top && app.sel() == 0);
    press(&mut app, KeyCode::Left);
    assert!(!app.help);
  }

  #[test]
  fn an_empty_period_totals_dashes_and_dates_are_even() {
    let mut app = App::new(data::tests::data());
    press(&mut app, KeyCode::Char('3'));
    for _ in 0..3 {
      press(&mut app, KeyCode::Char('t'));
    }
    assert_eq!(app.span, Span::Today);
    let buf = frame(&app);
    let total = cells(&line(&buf, find(&buf, "total")));
    assert_eq!(total[1..], ["–", "–", "–", "–", "–"], "{}", screen_of(&buf));
    // At 140 columns over 30 days, the dates under the bars are as far apart from each other.
    let mut app = many();
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Char('T'));
    let buf = frame_of(&app, 140, 30);
    let axis = (2..15).find(|y| line(&buf, *y).contains('━')).unwrap();
    let dates = line(&buf, axis + 1);
    let at: Vec<usize> = dates.match_indices('-').map(|x| dates[..x.0].chars().count()).collect();
    assert!(at.len() > 3 && at.windows(3).all(|w| w[1] - w[0] == w[2] - w[1]), "{dates}");
    // Off the grid, the selected day adds no label: the grid stays.
    press(&mut app, KeyCode::Down);
    let buf = frame_of(&app, 140, 30);
    assert_eq!(line(&buf, axis + 1), dates);
  }

  #[test]
  fn dates_never_overlap_at_any_width_or_period() {
    for span in Span::ALL {
      let mut app = App::new(data::tests::long());
      press(&mut app, KeyCode::Char('3'));
      while app.span != span {
        press(&mut app, KeyCode::Char('t'));
      }
      let wide = if span == Span::All { 10 } else { 5 };
      for width in 40..=140 {
        let buf = frame_of(&app, width, 30);
        let Some(axis) = (2..15).find(|y| line(&buf, *y).contains('━')) else { continue };
        let dates = line(&buf, axis + 1);
        // Every label whole (no two run together) and two cells from the next.
        let (mut at, mut end) = (0, None);
        for label in dates.split(' ') {
          if !label.is_empty() {
            assert_eq!(label.len(), wide, "{span:?} {width}: {dates:?}");
            assert!(end.is_none_or(|e| e + 2 <= at), "{span:?} {width}: {dates:?}");
            end = Some(at + label.len());
          }
          at += label.len() + 1;
        }
      }
    }
  }
}
