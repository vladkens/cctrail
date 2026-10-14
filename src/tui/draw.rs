// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

//! Colors from the terminal's own palette, the same for an entity on every screen: names in the
//! default foreground, time yellow, cost green, counts in the default color; dark gray only for
//! headers, separators, hints and inactive periods and tabs. The selected row is one even dark
//! gray bar across its width. Tables keep an equal gutter left (the `▶` column) and right.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as Text};
use ratatui::widgets::{
  Block, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row as TableRow, Sparkline, Table,
  TableState,
};

use super::{App, Col, Hits, Metric, Row, Span, Tab};
use crate::output::{cost, duration, tokens};

const DIM: Style = Style::new().fg(Color::DarkGray);
const KEY: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);
const NAME: Style = Style::new().fg(Color::Reset);
const TIME: Style = Style::new().fg(Color::Yellow);
const COST: Style = Style::new().fg(Color::Green);
/// The selected row: a background only, so every cell keeps its own text color.
const SELECTED: Style = Style::new().bg(Color::DarkGray);
const VERSION: &str = concat!("cctrail v", env!("CARGO_PKG_VERSION"));
/// The `▶ ` column on the left, matched by as much room on the right.
const GUTTER: u16 = 2;

fn dim(s: impl Into<String>) -> Text<'static> {
  Text::styled(s.into(), DIM)
}

/// A right-aligned cell, as every number is.
fn num(s: String, style: Style) -> Cell<'static> {
  Cell::from(Line::styled(s, style).right_aligned())
}

pub fn loading(f: &mut Frame) {
  let line = Line::from(vec![Text::styled(" cctrail ", BOLD), dim("reading the history…")]);
  f.render_widget(Paragraph::new(line), f.area());
}

pub fn draw(f: &mut Frame, app: &App) {
  let mut hits = Hits::default();
  let [head, tabs, body, foot] = Layout::vertical([
    Constraint::Length(1),
    Constraint::Length(2),
    Constraint::Min(1),
    Constraint::Length(1),
  ])
  .areas(f.area());
  header(f, app, head, &mut hits);
  tab_line(f, app, tabs, &mut hits);
  match &app.open {
    Some(_) => detail(f, app, body, &mut hits),
    None if app.tab == Tab::Timeline => timeline(f, app, body, &mut hits),
    None => {
      let sessions = app.tab == Tab::Projects;
      table(f, app, &app.list, &app.total, sessions, body, app.sel(), &mut hits);
    }
  }
  f.render_widget(footer(app), foot);
  // The help is a box over the whole screen, as macmon's.
  if app.help {
    help(f, f.area());
  }
  *app.hits.borrow_mut() = hits;
}

/// Periods and the provider on the left, `cctrail vX` on the right; the version gives way first.
fn header(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  let mut spans = vec![Text::styled(" cctrail  ", BOLD)];
  let mut x = area.x + 10;
  for s in Span::ALL {
    let style = if s == app.span { KEY } else { DIM };
    let label = format!(" {} ", s.label());
    hits.periods.push((Rect::new(x, area.y, label.len() as u16, 1), s));
    x += label.len() as u16;
    spans.push(Text::styled(label, style));
  }
  spans.push(dim(format!("   {}", app.prov.map_or("claude+codex", |x| x.name()))));
  let line = Line::from(spans);
  let free = (area.width as usize).saturating_sub(line.width());
  if free <= VERSION.len() + 2 {
    return f.render_widget(line, area);
  }
  let [left, right] =
    Layout::horizontal([Constraint::Fill(1), Constraint::Length(VERSION.len() as u16 + 1)])
      .areas(area);
  f.render_widget(line, left);
  f.render_widget(Paragraph::new(dim(VERSION)), right);
}

fn tab_line(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  let mut spans = vec![Text::raw(" ")];
  let mut x = area.x + 1;
  for (i, (tab, name)) in
    [(Tab::Projects, "Projects"), (Tab::Models, "Models"), (Tab::Timeline, "Timeline")]
      .into_iter()
      .enumerate()
  {
    let label = format!("{} {name}", i + 1);
    let label = if tab == app.tab { format!("[{label}]") } else { format!(" {label} ") };
    hits.tabs.push((Rect::new(x, area.y, label.len() as u16, 1), tab));
    x += label.len() as u16 + 2;
    let style = if tab == app.tab { BOLD } else { DIM };
    spans.push(Text::styled(label, style));
    spans.push(Text::raw("  "));
  }
  spans.push(dim("metric: "));
  spans.push(Text::styled(app.metric.label(), TIME));
  if !app.unpriced.is_empty() {
    let t: u64 = app.unpriced.iter().fold(0, |a, x| a.saturating_add(x.1));
    let names: Vec<&str> = app.unpriced.iter().map(|x| x.0.as_str()).collect();
    spans.push(dim(format!("   cost excludes {} on {}", tokens(t), names.join(", "))));
  }
  if !app.filter.is_empty() || app.typing {
    spans.push(dim("   filter: "));
    spans.push(Text::styled(format!("{}{}", app.filter, if app.typing { "▏" } else { "" }), KEY));
  }
  let lines = vec![Line::from(spans), Line::from(dim("─".repeat(area.width as usize)))];
  f.render_widget(Paragraph::new(lines), area);
}

/// The percent of the column total.
fn share(value: f64, sum: f64) -> String {
  let pct = if sum > 0.0 { (value / sum * 100.0).round() } else { 0.0 };
  format!("{pct}%")
}

/// The header with its sort mark: `▼` most first (names A to Z), `▲` reversed.
fn head_label(app: &App, col: Col, label: &str) -> String {
  match app.sort {
    Some((c, rev)) if c == col => format!("{label} {}", if rev { "▲" } else { "▼" }),
    _ => label.to_string(),
  }
}

/// Lays out `cells` rows with a header, the `total` row right under them and both gutters, and
/// records where the header columns and rows went.
#[allow(clippy::too_many_arguments)]
fn grid(
  f: &mut Frame,
  head: Vec<(Cell<'static>, Col)>,
  widths: &[Constraint],
  lines: Vec<TableRow<'static>>,
  total: TableRow<'static>,
  area: Rect,
  sel: usize,
  hits: &mut Hits,
) {
  let n = lines.len() as u16;
  let height = (n + 1).min(area.height.saturating_sub(1)).max(1);
  let table = Rect { height, ..area };
  let foot = Rect { y: area.y + height, height: area.height.saturating_sub(height).min(1), ..area };
  // The right gutter: an empty last column, after the usual one-column spacing.
  let mut widths = widths.to_vec();
  widths.push(Constraint::Length(GUTTER - 1));
  let cols = Layout::horizontal(widths.clone()).spacing(1).split(Rect {
    x: area.x + GUTTER,
    width: area.width.saturating_sub(GUTTER),
    ..area
  });
  hits.cols =
    head.iter().zip(cols.iter()).map(|((_, col), r)| (r.x, r.x + r.width, *col)).collect();
  hits.head = Rect { height: 1, ..table };
  hits.rows = Rect { y: table.y + 1, height: height.saturating_sub(1), ..table };
  let head = TableRow::new(head.into_iter().map(|x| x.0));
  let t = Table::new(lines, widths.clone())
    .header(head)
    .row_highlight_style(SELECTED)
    .highlight_symbol("▶ ")
    .highlight_spacing(HighlightSpacing::Always);
  let mut state = TableState::default().with_selected(Some(sel));
  f.render_stateful_widget(t, table, &mut state);
  hits.offset = state.offset();
  if foot.height > 0 {
    // Unselected, but with the same `▶ ` column, so its cells line up with the rows'.
    let t = Table::new([total.style(BOLD)], widths)
      .highlight_symbol("▶ ")
      .highlight_spacing(HighlightSpacing::Always);
    f.render_widget(t, foot);
  }
}

/// A row's cells: name, time, tokens, cost, sessions and the metric's share of `sum`.
fn cells(x: &Row, label: String, m: Metric, sum: f64) -> TableRow<'static> {
  cells_with(x, label, share(m.of(x), sum))
}

fn cells_with(x: &Row, label: String, share: String) -> TableRow<'static> {
  TableRow::new(vec![
    Cell::from(label).style(NAME),
    num(x.active.map_or("–".into(), duration), if x.active.is_some() { TIME } else { DIM }),
    num(tokens(x.tokens), Style::new()),
    num(cost(x.cost), COST),
    num(x.sessions.map_or(String::new(), |x| x.to_string()), Style::new()),
    num(share, Style::new()),
  ])
}

#[allow(clippy::too_many_arguments)]
fn table(
  f: &mut Frame,
  app: &App,
  rows: &[Row],
  total: &Row,
  sessions: bool,
  area: Rect,
  sel: usize,
  hits: &mut Hits,
) {
  let m = app.metric;
  let sum: f64 = rows.iter().map(|x| m.of(x)).sum();
  let lines = rows.iter().map(|x| cells(x, x.label.clone(), m, sum)).collect();
  let mut total = total.clone();
  total.sessions = total.sessions.filter(|_| sessions);
  // The total is all of the rows: 100% (when there is anything to share).
  let total = cells_with(&total, "total".into(), share(sum, sum));
  let name = match app.tab {
    _ if app.open.is_some() && app.tab == Tab::Projects => "Model",
    Tab::Models if app.open.is_none() => "Model",
    _ => "Project",
  };
  let head = [
    (name, Col::Name),
    ("Time", Col::Time),
    ("Tokens", Col::Tokens),
    ("Cost", Col::Cost),
    (if sessions { "Sessions" } else { "" }, Col::Sessions),
    ("Share", Col::Share),
  ];
  let head = head
    .into_iter()
    .map(|(label, col)| {
      let text = if label.is_empty() { String::new() } else { head_label(app, col, label) };
      let cell = if col == Col::Name { Cell::from(text).style(DIM) } else { num(text, DIM) };
      (cell, col)
    })
    .collect();
  let widths = [
    Constraint::Fill(1),
    Constraint::Length(12),
    Constraint::Length(9),
    Constraint::Length(11),
    Constraint::Length(8),
    Constraint::Length(6),
  ];
  grid(f, head, &widths, lines, total, area, sel, hits);
}

fn timeline(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  let m = app.metric;
  let rows = &app.list;
  let max = rows.iter().map(|x| m.of(x)).fold(0.0, f64::max);
  let width = area.width.saturating_sub(36) as usize;
  let value = |x: &Row| match m {
    Metric::Time => (duration(x.active.unwrap_or(0)), TIME),
    Metric::Tokens => (tokens(x.tokens), Style::new()),
    Metric::Cost => (cost(x.cost), COST),
  };
  let lines = rows
    .iter()
    .map(|x| {
      let full = if max > 0.0 { (m.of(x) / max * width as f64).round() as usize } else { 0 };
      let (text, style) = if m.of(x) == 0.0 { ("–".to_string(), DIM) } else { value(x) };
      TableRow::new(vec![
        Cell::from(x.label.clone()).style(NAME),
        num(text, style),
        Cell::from(Text::styled("█".repeat(full), Style::new().fg(Color::Cyan))),
      ])
    })
    .collect();
  let (text, style) = value(&app.total);
  let total = TableRow::new(vec![Cell::from("total"), num(text, style), Cell::from("")]);
  let head = vec![
    (Cell::from(head_label(app, Col::Name, "Day")).style(DIM), Col::Name),
    (num(head_label(app, Col::Share, m.label()), DIM), Col::Share),
  ];
  let widths = [Constraint::Length(16), Constraint::Length(14), Constraint::Fill(1)];
  grid(f, head, &widths, lines, total, area, app.sel(), hits);
}

/// A drill-down, in place of the list: its name, its rows with the opened row as their total, the
/// metric per day and its paths.
fn detail(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  let d = app.open.as_ref().unwrap();
  let r = &d.row;
  let title = vec![
    Text::styled(format!(" {}", r.label), NAME.add_modifier(Modifier::BOLD)),
    dim(format!(" · {}", app.span.label())),
  ];
  let spark = if d.days.is_empty() { 0 } else { 4 };
  let paths = if d.paths.is_empty() { 0 } else { 2 };
  let [head, rows, days, foot] = Layout::vertical([
    Constraint::Length(2),
    Constraint::Min(2),
    Constraint::Length(spark),
    Constraint::Length(paths),
  ])
  .areas(area);
  f.render_widget(Paragraph::new(Line::from(title)), head);
  let sessions = app.tab == Tab::Timeline;
  table(f, app, &d.rows, r, sessions, rows, app.osel, hits);
  if spark > 0 {
    let m = app.metric;
    // Cents for the cost, so a day under a dollar still draws.
    let scale = if m == Metric::Cost { 100.0 } else { 1.0 };
    let values: Vec<u64> = d.days.iter().map(|x| (m.of(x) * scale) as u64).collect();
    let gran = if app.span.gran() == crate::metrics::Granularity::Week { "week" } else { "day" };
    let [_, label, line] =
      Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(2)])
        .areas(days);
    // The caption starts at the first bucket that fits on the line.
    let line = Rect { x: line.x + GUTTER, width: line.width.saturating_sub(2 * GUTTER), ..line };
    let skip = values.len().saturating_sub(line.width as usize);
    let first = d.days.get(skip).map_or(String::new(), |x| x.key.clone());
    let caption = format!("   {} by {gran} since {first}", m.label());
    f.render_widget(Paragraph::new(Line::from(dim(caption))), label);
    let style = Style::new().fg(Color::Green);
    f.render_widget(Sparkline::default().data(&values[skip..]).style(style), line);
  }
  if paths > 0 {
    let line = Line::from(dim(format!("   Paths: {}", d.paths.join(", "))));
    f.render_widget(Paragraph::new(vec![Line::default(), line]), foot);
  }
}

/// The keys in a box centered over `area`.
fn help(f: &mut Frame, area: Rect) {
  let keys = [
    ("1 2 3", "projects / models / timeline"),
    ("← →", "period: today, week, month, 30 days, all"),
    ("↑ ↓", "select a row"),
    ("enter", "open: project → models, model → projects, day → projects"),
    ("esc ⌫", "back"),
    ("m", "metric for shares, sorting and bars: time / tokens / cost"),
    ("p", "provider: both / claude / codex"),
    ("/", "filter by name"),
    ("mouse", "click a row to select, again to open; a header sorts; wheel scrolls"),
    ("?", "this help"),
    ("q", "quit"),
  ];
  let lines: Vec<Line> = keys
    .iter()
    .map(|(k, d)| {
      Line::from(vec![Text::styled(format!(" {k:<7}"), KEY), Text::raw(*d), " ".into()])
    })
    .collect();
  let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 2;
  let height = lines.len() as u16 + 2;
  let rect = area.centered(
    Constraint::Length(width.min(area.width)),
    Constraint::Length(height.min(area.height)),
  );
  f.render_widget(Clear, rect);
  let title = Line::from(vec![Text::styled(" help ", BOLD)]);
  let close = Line::from(vec![Text::styled(" esc", KEY), dim(" close ")]).right_aligned();
  let block = Block::new().borders(Borders::ALL).border_style(DIM).title(title).title(close);
  f.render_widget(Paragraph::new(lines).block(block), rect);
}

fn footer(app: &App) -> Line<'static> {
  let hints: &[(&str, &str)] = match () {
    _ if app.typing => &[("enter", "keep"), ("esc", "clear")],
    _ if app.help => &[("esc", "close"), ("q", "quit")],
    _ if app.open.is_some() => {
      &[("↑↓", "select"), ("esc", "back"), ("m", "metric"), ("←→", "period"), ("?", "help")]
    }
    _ => &[
      ("↑↓", "select"),
      ("enter", "open"),
      ("1-3", "view"),
      ("←→", "period"),
      ("m", "metric"),
      ("p", "provider"),
      ("/", "filter"),
      ("?", "help"),
      ("q", "quit"),
    ],
  };
  let mut spans = vec![Text::raw(" ")];
  for (k, d) in hints {
    spans.push(Text::styled(*k, KEY));
    spans.push(dim(format!(" {d}  ")));
  }
  Line::from(spans)
}
