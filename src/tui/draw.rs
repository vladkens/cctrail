// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

//! Colors from the terminal's own palette, the same for an entity on every screen: names and counts
//! in the default foreground, time yellow, cost green; the selected period, tab and metric cyan;
//! dark gray only for headers, separators, hints and inactive choices. The selected row is one
//! even subtle bar across its width. Tables keep an equal gutter left (the `▶` column) and right.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as Text};
use ratatui::widgets::{
  Block, BorderType, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row as TableRow, Sparkline,
  Table, TableState,
};

use super::{App, Col, Hits, Metric, Row, Span, Tab};
use crate::output::duration;

const DIM: Style = Style::new().fg(Color::DarkGray);
/// The selected period, tab and metric, and the keys.
const KEY: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);
const TEXT: Style = Style::new().fg(Color::Reset);
const TIME: Style = Style::new().fg(Color::Yellow);
const COST: Style = Style::new().fg(Color::Green);
/// A value that isn't there (`–`): faint in the default color, so it shows on the selected row.
const NONE: Style = Style::new().fg(Color::Reset).add_modifier(Modifier::DIM);
/// The selected row: a background only, so every cell keeps its own text color.
const SELECTED: Style = Style::new().bg(Color::Indexed(236));
const VERSION: &str = concat!("cctrail v", env!("CARGO_PKG_VERSION"));
/// The `▶ ` column on the left, matched by as much room on the right.
const GUTTER: u16 = 2;
/// Room for a sort mark (` ▼`) after a header; a column fits its header, the mark and a space.
const MARK: u16 = 2;

fn dim(s: impl Into<String>) -> Text<'static> {
  Text::styled(s.into(), DIM)
}

/// A right-aligned cell, as every number is.
fn num(s: String, style: Style) -> Cell<'static> {
  Cell::from(Line::styled(s, style).right_aligned())
}

/// `$18,080.18`.
fn money(x: f64) -> String {
  let s = format!("{x:.2}");
  let (int, frac) = s.split_once('.').unwrap();
  let digits: Vec<char> = int.chars().collect();
  let mut out = String::new();
  for (i, c) in digits.iter().enumerate() {
    if i > 0 && (digits.len() - i).is_multiple_of(3) && *c != '-' && digits[i - 1] != '-' {
      out.push(',');
    }
    out.push(*c);
  }
  format!("${out}.{frac}")
}

/// Tokens with one decimal in every unit: `2.4B`, `58.5M`, `12.4K`, `882`.
fn toks(x: u64) -> String {
  match x {
    1_000_000_000.. => format!("{:.1}B", x as f64 / 1e9),
    1_000_000.. => format!("{:.1}M", x as f64 / 1e6),
    1_000.. => format!("{:.1}K", x as f64 / 1e3),
    _ => x.to_string(),
  }
}

/// `text` cut to `width` cells, ending with `…` when cut.
fn cut(text: &str, width: usize) -> String {
  if text.chars().count() <= width {
    return text.to_string();
  }
  let mut x: String = text.chars().take(width.saturating_sub(1)).collect();
  x.push('…');
  x
}

/// Spans cut to `width` cells, the last one ending with `…`.
fn cut_spans(spans: Vec<Text<'static>>, width: usize) -> Vec<Text<'static>> {
  let mut room = width;
  let mut out = vec![];
  for s in spans {
    let w = s.content.chars().count();
    if w <= room {
      room -= w;
      out.push(s);
      continue;
    }
    if room > 0 {
      out.push(Text::styled(cut(&s.content, room), s.style));
    }
    break;
  }
  out
}

/// Parts of a whole rounded so they still add up to it: each gets its floor, the largest
/// remainders one more.
fn largest_remainder(parts: &[f64], whole: i64) -> Vec<i64> {
  let mut out: Vec<i64> = parts.iter().map(|x| x.floor() as i64).collect();
  let rest = whole - out.iter().sum::<i64>();
  let mut order: Vec<usize> = (0..parts.len()).collect();
  order.sort_by(|a, b| {
    (parts[*b] - parts[*b].floor()).partial_cmp(&(parts[*a] - parts[*a].floor())).unwrap()
  });
  for i in order.into_iter().take(rest.max(0) as usize) {
    out[i] += 1;
  }
  out
}

/// Each row's percent of the column, adding up to 100; `<1%` for a row that rounds to none, blank
/// when there is nothing to share.
fn shares(values: &[f64]) -> Vec<String> {
  let sum: f64 = values.iter().sum();
  if sum <= 0.0 {
    return vec![String::new(); values.len()];
  }
  let pct = largest_remainder(&values.iter().map(|x| x / sum * 100.0).collect::<Vec<_>>(), 100);
  values
    .iter()
    .zip(pct)
    .map(|(v, p)| if p == 0 && *v > 0.0 { "<1%".into() } else { format!("{p}%") })
    .collect()
}

/// Time in minutes for each row, adding up to the total's minutes when the rows make the total.
fn minutes(rows: &[Row], total: &Row) -> Vec<Option<String>> {
  let plain = || rows.iter().map(|x| x.active.map(duration)).collect();
  let Some(total) = total.active else { return plain() };
  let actives: Option<Vec<i64>> = rows.iter().map(|x| x.active).collect();
  let Some(actives) = actives.filter(|x| x.iter().sum::<i64>() == total) else { return plain() };
  let whole = (total / 1_000_000 + 30) / 60;
  let parts: Vec<f64> = actives.iter().map(|x| *x as f64 / 60e6).collect();
  largest_remainder(&parts, whole).into_iter().map(|m| Some(duration(m * 60_000_000))).collect()
}

pub fn loading(f: &mut Frame) {
  let line = Line::from(vec![Text::styled(" cctrail ", BOLD), dim("reading the history…")]);
  f.render_widget(Paragraph::new(line), f.area());
}

pub fn draw(f: &mut Frame, app: &App) {
  let mut hits = Hits::default();
  let [head, tabs, body, rule, foot] = Layout::vertical([
    Constraint::Length(1),
    Constraint::Length(2),
    Constraint::Min(1),
    Constraint::Length(1),
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
  f.render_widget(Paragraph::new(dim("─".repeat(rule.width as usize))), rule);
  f.render_widget(footer(app, foot.width), foot);
  // The help is a box over the whole screen, as macmon's.
  if app.help {
    help(f, f.area());
  }
  *app.hits.borrow_mut() = hits;
}

/// Periods and the provider on the left, `cctrail vX` on the right; the version gives way first.
fn header(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  let mut spans = vec![Text::raw(" ")];
  let mut x = area.x + 1;
  for s in Span::ALL {
    let label = s.label();
    hits.periods.push((Rect::new(x, area.y, label.len() as u16, 1), s));
    x += label.len() as u16 + 2;
    spans.push(Text::styled(label, if s == app.span { KEY } else { DIM }));
    spans.push(Text::raw("  "));
  }
  spans.push(Text::raw(" "));
  spans.push(Text::styled(app.prov.map_or("claude+codex", |x| x.name()), TEXT));
  let line = Line::from(spans);
  let free = (area.width as usize).saturating_sub(line.width());
  if free <= VERSION.len() + 2 {
    return f.render_widget(line, area);
  }
  let [left, right] =
    Layout::horizontal([Constraint::Fill(1), Constraint::Length(VERSION.len() as u16 + 1)])
      .areas(area);
  f.render_widget(line, left);
  f.render_widget(Paragraph::new(Text::styled(VERSION, TEXT)), right);
}

/// Tabs, the metric, the filter and the unpriced note, cut with `…` at the edge.
fn tab_line(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  let mut spans = vec![Text::raw(" ")];
  let mut x = area.x + 1;
  for (i, (tab, name)) in
    [(Tab::Projects, "Projects"), (Tab::Models, "Models"), (Tab::Timeline, "Timeline")]
      .into_iter()
      .enumerate()
  {
    let label = format!("{} {name}", i + 1);
    hits.tabs.push((Rect::new(x, area.y, label.len() as u16, 1), tab));
    x += label.len() as u16 + 2;
    spans.push(Text::styled(label, if tab == app.tab { KEY } else { DIM }));
    spans.push(Text::raw("  "));
  }
  spans.push(dim(" metric "));
  spans.push(Text::styled(app.metric.label(), KEY));
  if !app.filter.is_empty() || app.typing {
    spans.push(dim("   filter "));
    let caret = if app.typing { "▏" } else { "" };
    spans.push(Text::styled(format!("{}{caret}", app.filter), TEXT));
  }
  if !app.unpriced.is_empty() {
    let t: u64 = app.unpriced.iter().fold(0, |a, x| a.saturating_add(x.1));
    let names: Vec<&str> = app.unpriced.iter().map(|x| x.0.as_str()).collect();
    spans.push(dim("   unpriced "));
    spans.push(Text::styled(format!("{} {}", toks(t), names.join(", ")), TEXT));
  }
  let spans = cut_spans(spans, area.width.saturating_sub(1) as usize);
  let lines = vec![Line::from(spans), Line::from(dim("─".repeat(area.width as usize)))];
  f.render_widget(Paragraph::new(lines), area);
}

/// The header with its sort mark: `▼` most first (names A to Z), `▲` reversed.
fn head_label(app: &App, col: Col, label: &str) -> String {
  match app.sort {
    Some((c, rev)) if c == col => format!("{label} {}", if rev { "▲" } else { "▼" }),
    _ => label.to_string(),
  }
}

/// A column: its header, what a click on it sorts by, its width.
struct Column {
  head: String,
  col: Col,
  width: Constraint,
}

/// Lays out rows under a header, the `total` row right under them, both gutters, and records where
/// the header columns and rows went. Returns the height used.
#[allow(clippy::too_many_arguments)]
fn grid(
  f: &mut Frame,
  columns: &[Column],
  lines: Vec<TableRow<'static>>,
  total: TableRow<'static>,
  area: Rect,
  sel: usize,
  hits: &mut Hits,
) -> u16 {
  let n = lines.len() as u16;
  let height = (n + 1).min(area.height.saturating_sub(1)).max(1);
  let table = Rect { height, ..area };
  let foot = Rect { y: area.y + height, height: area.height.saturating_sub(height).min(1), ..area };
  // The right gutter: an empty last column, after the usual one-column spacing.
  let mut widths: Vec<Constraint> = columns.iter().map(|x| x.width).collect();
  widths.push(Constraint::Length(GUTTER - 1));
  let cols = layout(&widths, area);
  hits.cols = columns
    .iter()
    .zip(cols.iter())
    .filter(|(c, _)| !c.head.is_empty())
    .map(|(c, r)| (r.x, r.x + r.width, c.col))
    .collect();
  hits.head = Rect { height: 1, ..table };
  hits.rows = Rect { y: table.y + 1, height: height.saturating_sub(1), ..table };
  let head = columns.iter().enumerate().map(|(i, c)| match i {
    0 => Cell::from(c.head.clone()).style(DIM),
    _ => num(c.head.clone(), DIM),
  });
  let t = Table::new(lines, widths.clone())
    .header(TableRow::new(head))
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
  height + foot.height
}

/// The column rectangles of a table in `area`, after the `▶ ` column.
fn layout(widths: &[Constraint], area: Rect) -> std::rc::Rc<[Rect]> {
  Layout::horizontal(widths.to_vec()).spacing(1).split(Rect {
    x: area.x + GUTTER,
    width: area.width.saturating_sub(GUTTER),
    ..area
  })
}

fn empty(f: &mut Frame, area: Rect, hits: &mut Hits) {
  hits.rows = Rect::default();
  f.render_widget(Paragraph::new(dim("  No activity in this period")), area);
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
) -> u16 {
  if rows.is_empty() {
    empty(f, area, hits);
    return 1;
  }
  let m = app.metric;
  let name = match app.tab {
    _ if app.open.is_some() && app.tab == Tab::Projects => "Model",
    Tab::Models if app.open.is_none() => "Model",
    _ => "Project",
  };
  let mut columns = vec![
    Column { head: head_label(app, Col::Name, name), col: Col::Name, width: Constraint::Fill(1) },
    Column {
      head: head_label(app, Col::Time, "Time"),
      col: Col::Time,
      width: Constraint::Length(10 + MARK),
    },
    Column {
      head: head_label(app, Col::Tokens, "Tokens"),
      col: Col::Tokens,
      width: Constraint::Length(7 + MARK),
    },
    Column {
      head: head_label(app, Col::Cost, "Cost"),
      col: Col::Cost,
      width: Constraint::Length(10 + MARK),
    },
  ];
  if sessions {
    columns.push(Column {
      head: head_label(app, Col::Sessions, "Sessions"),
      col: Col::Sessions,
      width: Constraint::Length(9 + MARK),
    });
  }
  columns.push(Column {
    head: head_label(app, Col::Share, "Share"),
    col: Col::Share,
    width: Constraint::Length(6 + MARK),
  });
  let mut widths: Vec<Constraint> = columns.iter().map(|x| x.width).collect();
  widths.push(Constraint::Length(GUTTER - 1));
  let name_width = layout(&widths, area)[0].width as usize;

  let share = shares(&rows.iter().map(|x| m.of(x)).collect::<Vec<_>>());
  let times = minutes(rows, total);
  let cells = |x: &Row, label: &str, time: Option<String>, share: String| {
    let mut cells = vec![
      Cell::from(cut(label, name_width)).style(TEXT),
      match time {
        Some(t) => num(t, TIME),
        None => num("–".into(), NONE),
      },
      num(toks(x.tokens), TEXT),
      if x.unpriced { num("–".into(), NONE) } else { num(money(x.cost), COST) },
    ];
    if sessions {
      cells.push(num(x.sessions.map_or(String::new(), |x| x.to_string()), TEXT));
    }
    cells.push(num(share, TEXT));
    TableRow::new(cells)
  };
  let lines =
    rows.iter().zip(times).zip(share).map(|((x, t), s)| cells(x, &x.label, t, s)).collect();
  let all = if rows.iter().any(|x| m.of(x) > 0.0) { "100%".into() } else { String::new() };
  let total = cells(total, "total", total.active.map(duration), all);
  grid(f, &columns, lines, total, area, sel, hits)
}

/// The metric's text and color for a row.
fn metric_value(m: Metric, x: &Row) -> (String, Style) {
  match m {
    Metric::Time => (duration(x.active.unwrap_or(0)), TIME),
    Metric::Tokens => (toks(x.tokens), TEXT),
    Metric::Cost => (money(x.cost), COST),
  }
}

/// The metric's color, for its bars and sparkline as for its numbers.
fn metric_color(m: Metric) -> Style {
  [TIME, TEXT, COST][m as usize]
}

fn timeline(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  let m = app.metric;
  let rows = &app.list;
  if rows.iter().all(|x| m.of(x) == 0.0) {
    return empty(f, area, hits);
  }
  let max = rows.iter().map(|x| m.of(x)).fold(0.0, f64::max);
  let width = area.width.saturating_sub(36) as usize;
  let lines = rows
    .iter()
    .map(|x| {
      let full = if max > 0.0 { (m.of(x) / max * width as f64).round() as usize } else { 0 };
      let (text, style) =
        if m.of(x) == 0.0 { ("–".to_string(), NONE) } else { metric_value(m, x) };
      TableRow::new(vec![
        Cell::from(x.label.clone()).style(TEXT),
        num(text, style),
        Cell::from(Text::styled("█".repeat(full), metric_color(m))),
      ])
    })
    .collect();
  let (text, style) = metric_value(m, &app.total);
  let total = TableRow::new(vec![Cell::from("total"), num(text, style), Cell::from("")]);
  let day = if app.span.gran() == crate::metrics::Granularity::Week { "Week" } else { "Day" };
  let value = ["Time", "Tokens", "Cost"][m as usize];
  let columns = [
    Column {
      head: head_label(app, Col::Name, day),
      col: Col::Name,
      width: Constraint::Length(14 + MARK),
    },
    Column {
      head: head_label(app, Col::Share, value),
      col: Col::Share,
      width: Constraint::Length(10 + MARK),
    },
    Column { head: String::new(), col: Col::Share, width: Constraint::Fill(1) },
  ];
  grid(f, &columns, lines, total, area, app.sel(), hits);
}

/// A drill-down, in place of the list: its name, its rows with the opened row as their total, the
/// metric per day right under them and its paths.
fn detail(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  let d = app.open.as_ref().unwrap();
  let r = &d.row;
  let title = Line::from(vec![
    Text::styled(format!("  {}", r.label), TEXT.add_modifier(Modifier::BOLD)),
    dim(format!(" · {}", app.span.label())),
  ]);
  f.render_widget(Paragraph::new(title), area);
  let body = Rect { y: area.y + 2, height: area.height.saturating_sub(2), ..area };
  // Room for the sparkline and the paths below the table, when there are any.
  let below = if d.days.is_empty() { 0 } else { 4 } + if d.paths.is_empty() { 0 } else { 2 };
  let room = Rect { height: body.height.saturating_sub(below).max(3), ..body };
  let used = table(f, app, &d.rows, r, app.tab == Tab::Timeline, room, app.osel, hits);
  let mut y = body.y + used;
  if !d.days.is_empty() && y + 4 <= area.bottom() {
    let m = app.metric;
    // Cents for the cost, so a day under a dollar still draws.
    let scale = if m == Metric::Cost { 100.0 } else { 1.0 };
    let values: Vec<u64> = d.days.iter().map(|x| (m.of(x) * scale) as u64).collect();
    let gran = if app.span.gran() == crate::metrics::Granularity::Week { "week" } else { "day" };
    let line = Rect::new(area.x + GUTTER, y + 2, area.width.saturating_sub(2 * GUTTER), 2);
    // The caption starts at the first bucket that fits on the line.
    let skip = values.len().saturating_sub(line.width as usize);
    let first = d.days.get(skip).map_or(String::new(), |x| x.key.clone());
    let caption = format!("  {} by {gran} since {first}", m.label());
    f.render_widget(Paragraph::new(dim(caption)), Rect::new(area.x, y + 1, area.width, 1));
    f.render_widget(Sparkline::default().data(&values[skip..]).style(metric_color(m)), line);
    y += 4;
  }
  if !d.paths.is_empty() && y + 2 <= area.bottom() {
    let paths = Line::from(vec![dim("  Paths "), Text::styled(d.paths.join(", "), TEXT)]);
    f.render_widget(Paragraph::new(paths), Rect::new(area.x, y + 1, area.width, 1));
  }
}

/// The keys in a rounded box centered over `area`, wrapped by words when narrow.
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
    ("mouse", "click a row to select it, again to open it; a header sorts; the wheel scrolls"),
    ("?", "this help"),
    ("q", "quit"),
  ];
  const KEYS: usize = 8;
  let widest = keys.iter().map(|x| x.1.chars().count()).max().unwrap_or(0) + KEYS + 2;
  let width = (widest as u16 + 2).min(area.width);
  let room = (width as usize).saturating_sub(2 + KEYS + 2).max(10);
  let mut lines = vec![];
  for (k, d) in keys {
    let mut first = true;
    let mut line = String::new();
    let mut flush = |line: &mut String, first: &mut bool| {
      let key = if *first { format!(" {k:<7}") } else { " ".repeat(KEYS) };
      lines.push(Line::from(vec![
        Text::styled(key, Style::new().fg(Color::Cyan)),
        Text::styled(std::mem::take(line), TEXT),
      ]));
      *first = false;
    };
    for word in d.split(' ') {
      if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > room {
        flush(&mut line, &mut first);
      }
      if !line.is_empty() {
        line.push(' ');
      }
      line.push_str(word);
    }
    flush(&mut line, &mut first);
  }
  let height = (lines.len() as u16 + 2).min(area.height);
  let rect = area.centered(Constraint::Length(width), Constraint::Length(height));
  f.render_widget(Clear, rect);
  let title = Line::from(Text::styled(" help ", TEXT.add_modifier(Modifier::BOLD)));
  let close = Line::from(vec![Text::styled(" esc", Style::new().fg(Color::Cyan)), dim(" close ")])
    .right_aligned();
  let block = Block::new()
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(DIM)
    .title(title)
    .title(close);
  f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// The hints that fit in `width`, whole ones only.
fn footer(app: &App, width: u16) -> Line<'static> {
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
  let mut used = 1;
  for (k, d) in hints {
    let w = k.chars().count() + 1 + d.len() + 2;
    if used + w > width as usize {
      break;
    }
    used += w;
    spans.push(Text::styled(*k, Style::new().fg(Color::Cyan)));
    spans.push(dim(format!(" {d}  ")));
  }
  Line::from(spans)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn formats_numbers_and_names() {
    assert_eq!(money(18080.184), "$18,080.18");
    assert_eq!(money(999.5), "$999.50");
    assert_eq!(money(1234567.0), "$1,234,567.00");
    assert_eq!(toks(2_400_000_000), "2.4B");
    assert_eq!(toks(58_000_000), "58.0M");
    assert_eq!(toks(12_400), "12.4K");
    assert_eq!(toks(882), "882");
    assert_eq!(cut("oss/demo", 8), "oss/demo");
    assert_eq!(cut("oss/demo", 5), "oss/…");
  }

  #[test]
  fn shares_add_up_to_a_hundred() {
    assert_eq!(shares(&[1.0, 1.0, 1.0]), ["34%", "33%", "33%"]);
    assert_eq!(shares(&[1000.0, 1.0, 0.0]), ["100%", "<1%", "0%"]);
    assert_eq!(shares(&[0.0, 0.0]), ["", ""]);
  }
}
