// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

//! Colors from the terminal's own palette, the same for an entity on every screen: names and counts
//! in the default foreground, time yellow, cost green; the selected period, tab and bar cyan;
//! dark gray only for headers, separators, hints and inactive choices. The selected row is one
//! even subtle bar across its width. Tables keep an equal gutter left (the `▶` column) and right.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as Text};
use ratatui::widgets::{
  Block, BorderType, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row as TableRow, Scrollbar,
  ScrollbarOrientation, ScrollbarState, Table, TableState,
};

use super::{App, Col, Hits, Row, Sort, Span, Tab};
use crate::output::duration;

const DIM: Style = Style::new().fg(Color::DarkGray);
/// The selected period and tab, and the keys.
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
/// Room for a sort arrow (` ↓`) after a header; a column fits its header, the mark and a space.
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
  let provider = app.prov.map_or("claude+codex", |x| x.name());
  hits.provider = Rect::new(x + 1, area.y, provider.len() as u16, 1).intersection(area);
  spans.push(Text::raw(" "));
  spans.push(Text::styled(provider, TEXT));
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

/// Tabs and the filter, cut with `…` at the edge, and the unpriced note when it fits.
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
  if !app.filter.is_empty() || app.typing {
    spans.push(dim(" filter "));
    let caret = if app.typing { "▏" } else { "" };
    spans.push(Text::styled(format!("{}{caret}", app.filter), TEXT));
  }
  if !app.unpriced.is_empty() {
    let t: u64 = app.unpriced.iter().fold(0, |a, x| a.saturating_add(x.1));
    let names: Vec<&str> = app.unpriced.iter().map(|x| x.0.as_str()).collect();
    let note = format!("{} {}", toks(t), names.join(", "));
    let room = (area.width as usize).saturating_sub(1 + Line::from(spans.clone()).width());
    // Three spaces from what is before it, as between the tabs and the filter.
    let gap = if app.filter.is_empty() && !app.typing { " " } else { "   " };
    if gap.len() + 9 + note.chars().count() <= room {
      spans.push(dim(format!("{gap}unpriced ")));
      spans.push(Text::styled(note, TEXT));
    }
  }
  let spans = cut_spans(spans, area.width.saturating_sub(1) as usize);
  let lines = vec![Line::from(spans), Line::from(dim("─".repeat(area.width as usize)))];
  f.render_widget(Paragraph::new(lines), area);
}

/// The header with the sort arrow on the sorted column, as macmon's: `↓` most first, `↑` least
/// first (names A to Z).
fn head_label(sort: Sort, col: Col, label: &str) -> String {
  match sort {
    (c, most) if c == col => format!("{label} {}", if most { "↓" } else { "↑" }),
    _ => label.to_string(),
  }
}

/// A column: its header, what a click on it sorts by, its width.
struct Column {
  head: String,
  col: Col,
  width: Constraint,
}

/// Lays out rows under a header, the `total` row right under them, both gutters and a scrollbar
/// in the right one when the rows don't fit, and records where the header columns and rows went.
/// Returns the height used.
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
  let n = lines.len();
  let height = (n as u16 + 1).min(area.height.saturating_sub(1)).max(1);
  let table = Rect { height, ..area };
  let foot = Rect { y: area.y + height, height: area.height.saturating_sub(height).min(1), ..area };
  // The right gutter: an empty last column, after the usual one-column spacing.
  let mut widths: Vec<Constraint> = columns.iter().map(|x| x.width).collect();
  widths.push(Constraint::Length(GUTTER - 1));
  let cols = layout(&widths, area);
  hits.cols = columns
    .iter()
    .zip(cols.iter())
    .filter(|(c, _)| !c.head.is_empty() && c.col != Col::Share)
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
  scrollbar(f, area, hits.rows, n, hits.offset);
  if foot.height > 0 {
    // Unselected, but with the same `▶ ` column, so its cells line up with the rows'.
    let t = Table::new([total.style(BOLD)], widths)
      .highlight_symbol("▶ ")
      .highlight_spacing(HighlightSpacing::Always);
    f.render_widget(t, foot);
  }
  height + foot.height
}

/// Where the visible rows are among `len`, in the last column of the right gutter, as macmon's.
fn scrollbar(f: &mut Frame, area: Rect, rows: Rect, len: usize, offset: usize) {
  let page = usize::from(rows.height);
  if len <= page || page < 3 || area.width < 2 {
    return;
  }
  let bar = Rect { x: area.right() - 1, width: 1, ..rows };
  // Scroll positions, not rows, so the thumb reaches the bottom at the last one.
  let mut state =
    ScrollbarState::new(len - page + 1).position(offset).viewport_content_length(page);
  let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
    .begin_symbol(Some("↑"))
    .end_symbol(Some("↓"))
    .track_symbol(None)
    .thumb_symbol("┃")
    .style(DIM)
    .thumb_style(TEXT);
  f.render_stateful_widget(scrollbar, bar, &mut state);
}

/// The column rectangles of a table in `area`, after the `▶ ` column.
fn layout(widths: &[Constraint], area: Rect) -> std::rc::Rc<[Rect]> {
  Layout::horizontal(widths.to_vec()).spacing(1).split(Rect {
    x: area.x + GUTTER,
    width: area.width.saturating_sub(GUTTER),
    ..area
  })
}

fn empty(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  hits.rows = Rect::default();
  let text = match app.filter.is_empty() || !app.list.is_empty() {
    true => "No activity in this period".to_string(),
    false => format!("No match for {:?}", app.filter),
  };
  f.render_widget(Paragraph::new(dim(format!("  {text}"))), area);
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
    empty(f, app, area, hits);
    return 1;
  }
  let sort = app.sort();
  let name = match app.tab {
    _ if app.open.is_some() && app.tab == Tab::Projects => "Model",
    Tab::Models if app.open.is_none() => "Model",
    _ => "Project",
  };
  let column = |col, label, width| Column {
    head: head_label(sort, col, label),
    col,
    width: Constraint::Length(width + MARK),
  };
  let mut columns = vec![
    Column { head: head_label(sort, Col::Name, name), col: Col::Name, width: Constraint::Fill(1) },
    column(Col::Time, "Time", 10),
    column(Col::Tokens, "Tokens", 7),
    column(Col::Cost, "Cost", 10),
  ];
  if sessions {
    columns.push(column(Col::Sessions, "Sessions", 9));
  }
  columns.push(column(Col::Share, "Share", 6));
  let mut widths: Vec<Constraint> = columns.iter().map(|x| x.width).collect();
  widths.push(Constraint::Length(GUTTER - 1));
  let name_width = layout(&widths, area)[0].width as usize;

  let of = sort.0.share();
  let share = shares(&rows.iter().map(|x| of.of(x)).collect::<Vec<_>>());
  let cells = |x: &Row, label: &str, share: String| {
    let mut cells = vec![
      Cell::from(cut(label, name_width)).style(TEXT),
      match x.active {
        Some(t) => num(duration(t), TIME),
        None => num("–".into(), NONE),
      },
      num(toks(x.tokens), TEXT),
      if x.unpriced { num("–".into(), NONE) } else { num(money(x.cost), COST) },
    ];
    if sessions {
      cells.push(num(x.sessions.map_or(String::new(), |x| x.to_string()), TEXT));
    }
    // An unknown cost has no share of the cost either.
    cells.push(match x.unpriced && of == Col::Cost {
      true => num("–".into(), NONE),
      false => num(share, TEXT),
    });
    TableRow::new(cells)
  };
  let lines = rows.iter().zip(share).map(|(x, s)| cells(x, &x.label, s)).collect();
  let all = if rows.iter().any(|x| of.of(x) > 0.0) { "100%".into() } else { String::new() };
  let total = cells(total, "total", all);
  grid(f, &columns, lines, total, area, sel, hits)
}

/// The column's text and color for a row.
fn value(col: Col, x: &Row) -> (String, Style) {
  match col {
    Col::Tokens => (toks(x.tokens), TEXT),
    Col::Cost => (money(x.cost), COST),
    _ => (duration(x.active.unwrap_or(0)), TIME),
  }
}

/// A bar `eighths` high, from the bottom: its cell on line `line` (0 at the bottom).
fn bar_cell(eighths: usize, line: usize) -> &'static str {
  const PARTS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
  PARTS[eighths.saturating_sub(line * 8).min(8)]
}

/// The timeline: a bar per day (per week for all of the history) with the date under some, the
/// largest value on the axis, the selected bar in the selection color and its numbers under the
/// chart over the period's total.
fn timeline(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  let col = app.sort().0;
  let rows = &app.list;
  if rows.iter().all(|x| col.of(x) == 0.0) || area.height < 8 {
    return empty(f, app, area, hits);
  }
  let week = app.span.gran() == crate::metrics::Granularity::Week;
  let name = ["Time", "Tokens", "Cost"][(col as usize).clamp(1, 3) - 1];
  let title =
    vec![Text::raw("  "), Text::styled(name, KEY), dim(if week { " by week" } else { " by day" })];
  f.render_widget(Paragraph::new(Line::from(title)), area);

  // Title, a blank line, the bars, their dates, a blank line, the numbers table.
  let height = area.height - 7;
  let chart = Rect::new(area.x, area.y + 2, area.width, height);
  let max = rows.iter().map(|x| col.of(x)).fold(0.0, f64::max);
  let top = value(col, rows.iter().max_by(|a, b| col.of(a).total_cmp(&col.of(b))).unwrap()).0;
  let axis = top.chars().count() as u16 + 1;
  let x0 = area.x + GUTTER + axis;
  let room = area.right().saturating_sub(GUTTER + x0) as usize;
  // Each bar and the gap after it: at least two cells, at most eight, the same for all.
  let n = rows.len();
  let shown = (room / 2).min(n).max(1);
  let pitch = (room / shown).min(8);
  let at = |i: usize| i * pitch;
  let width = (pitch as u16 - 1).max(1);
  let sel = app.sel().min(n - 1);
  let first = n.saturating_sub(shown).min(sel);
  let style = match col {
    Col::Tokens => TEXT,
    Col::Cost => COST,
    _ => TIME,
  };
  f.render_widget(
    Paragraph::new(Line::from(dim(top)).right_aligned()),
    Rect::new(area.x, chart.y, x0 - area.x - 1, 1),
  );
  f.render_widget(
    Paragraph::new(Line::from(dim("0")).right_aligned()),
    Rect::new(area.x, chart.bottom() - 1, x0 - area.x - 1, 1),
  );
  let buf = f.buffer_mut();
  for (i, row) in rows.iter().enumerate().skip(first).take(shown) {
    let x = x0 + at(i - first) as u16;
    let v = col.of(row);
    let eighths = match v > 0.0 {
      true => ((v / max * height as f64 * 8.0).round() as usize).max(1),
      false => 0,
    };
    let style = if i == sel { Style::new().fg(Color::Cyan) } else { style };
    for line in 0..height as usize {
      let y = chart.bottom() - 1 - line as u16;
      for dx in 0..width {
        buf[(x + dx, y)].set_symbol(bar_cell(eighths, line)).set_style(style);
      }
    }
    hits.bars.push((Rect::new(x, chart.y, width, height), i));
  }
  // Dates under every few bars, counted back from the newest so that it has one.
  let date = |x: &Row| {
    x.day
      .map_or(String::new(), |d| if week { d.to_string() } else { d.strftime("%m-%d").to_string() })
  };
  let wide: usize = if week { 10 } else { 5 };
  let every = (wide + 1).div_ceil(pitch);
  let last = first + shown - 1;
  // From the newest back, each under its bar (the newest kept on screen) and clear of the next.
  let end = usize::from(area.right().saturating_sub(GUTTER + x0));
  let mut marks: Vec<(usize, usize)> = vec![];
  for i in (first..=last).rev().step_by(every) {
    let x = at(i - first).min(end.saturating_sub(wide));
    if marks.last().is_none_or(|(next, _)| x + wide + 1 < *next) {
      marks.push((x, i));
    }
  }
  let mut labels = vec![Text::raw(" ".repeat((x0 - area.x) as usize))];
  let mut free = 0;
  for (x, i) in marks.into_iter().rev() {
    let style = if i == sel { Style::new().fg(Color::Cyan) } else { DIM };
    labels.push(Text::raw(" ".repeat(x - free)));
    labels.push(Text::styled(date(&rows[i]), style));
    free = x + wide;
  }
  let labels = cut_spans(labels, area.width.saturating_sub(GUTTER) as usize);
  f.render_widget(
    Paragraph::new(Line::from(labels)),
    Rect::new(area.x, chart.bottom(), area.width, 1),
  );

  // The selected bar's numbers over the period's total, in the tables' columns.
  let numbers = Rect { y: chart.bottom() + 2, height: 3, ..area };
  let head = if week { "Week" } else { "Day" };
  let columns = [
    Column { head: head.into(), col: Col::Name, width: Constraint::Fill(1) },
    Column { head: "Time".into(), col: Col::Time, width: Constraint::Length(10 + MARK) },
    Column { head: "Tokens".into(), col: Col::Tokens, width: Constraint::Length(7 + MARK) },
    Column { head: "Cost".into(), col: Col::Cost, width: Constraint::Length(10 + MARK) },
  ];
  let line = |x: &Row, label: &str| {
    TableRow::new(vec![
      Cell::from(label.to_string()).style(TEXT),
      match x.active.filter(|x| *x > 0) {
        Some(t) => num(duration(t), TIME),
        None => num("–".into(), NONE),
      },
      num(toks(x.tokens), TEXT),
      num(money(x.cost), COST),
    ])
  };
  let row = &rows[sel];
  grid(f, &columns, vec![line(row, &row.label)], line(&app.total, "total"), numbers, 0, hits);
  // Bars take the clicks: the table's single row is the selected bar.
  (hits.cols, hits.head, hits.rows) = (vec![], Rect::default(), Rect::default());
}

/// A drill-down, in place of the list: its name, its rows with the opened row as their total and
/// its paths.
fn detail(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  let d = app.open.as_ref().unwrap();
  let r = &d.row;
  let title = Line::from(vec![
    Text::styled(format!("  {}", r.label), TEXT.add_modifier(Modifier::BOLD)),
    dim(format!(" · {}", app.span.label())),
  ]);
  f.render_widget(Paragraph::new(title), area);
  let body = Rect { y: area.y + 2, height: area.height.saturating_sub(2), ..area };
  let below = if d.paths.is_empty() { 0 } else { 2 };
  let room = Rect { height: body.height.saturating_sub(below).max(3), ..body };
  let used = table(f, app, &d.rows, r, d.sessions, room, app.osel, hits);
  let y = body.y + used;
  if !d.paths.is_empty() && y + 2 <= area.bottom() {
    let paths = Line::from(vec![dim("  Paths "), Text::styled(d.paths.join(", "), TEXT)]);
    f.render_widget(Paragraph::new(paths), Rect::new(area.x, y + 1, area.width, 1));
  }
}

/// The keys in a rounded box centered over `area`, wrapped by words when narrow.
fn help(f: &mut Frame, area: Rect) {
  let keys = [
    ("1 2 3", "projects / models / timeline"),
    ("← →", "period: today, week, month, 30 days, all; wraps around"),
    ("↑ ↓", "select a row or a day; wraps around"),
    ("PgUp PgDn", "a page up / down"),
    ("Home End", "the first / last row"),
    ("enter", "open: project → models, model → projects, day → projects"),
    ("esc ⌫ q", "back"),
    ("s", "sort by the next column; on the timeline: bars of time / tokens / cost"),
    ("S", "reverse the sort"),
    ("p", "provider: both / claude / codex"),
    ("/", "filter by name"),
    ("mouse", "click a row or bar to select it, again to open it; a header sorts, again reverses"),
    ("?", "this help"),
    ("q", "quit"),
  ];
  const KEYS: usize = 11;
  let widest = keys.iter().map(|x| x.1.chars().count()).max().unwrap_or(0) + KEYS + 2;
  let width = (widest as u16 + 2).min(area.width);
  let room = (width as usize).saturating_sub(2 + KEYS + 2).max(10);
  let mut lines = vec![];
  for (k, d) in keys {
    let mut first = true;
    let mut line = String::new();
    let mut flush = |line: &mut String, first: &mut bool| {
      let key = if *first { format!(" {k:<10}") } else { " ".repeat(KEYS) };
      let text = Text::styled(std::mem::take(line), TEXT);
      lines.push(Line::from(vec![Text::styled(key, Style::new().fg(Color::Cyan)), text]));
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

/// The hints that fit in `width`, whole ones only: the least needed go first, help and quit last.
fn footer(app: &App, width: u16) -> Line<'static> {
  // Each hint with its rank: a narrow footer drops the lowest first.
  let hints: &[(&str, &str, u8)] = match () {
    _ if app.typing => &[("enter", "keep", 1), ("esc", "clear", 1)],
    // The box itself says how to close it.
    _ if app.help => &[],
    _ if app.open.is_some() => &[
      ("↑↓", "select", 2),
      ("s", "sort", 1),
      ("esc", "back", 7),
      ("←→", "period", 0),
      ("?", "help", 8),
    ],
    _ if app.tab == Tab::Timeline => &[
      ("↑↓", "day", 6),
      ("enter", "open", 5),
      ("s", "bars", 4),
      ("←→", "period", 3),
      ("p", "provider", 1),
      ("1-3", "view", 0),
      ("?", "help", 8),
      ("q", "quit", 9),
    ],
    _ => &[
      ("↑↓", "select", 6),
      ("enter", "open", 5),
      ("s", "sort", 4),
      ("←→", "period", 3),
      ("p", "provider", 1),
      ("1-3", "view", 0),
      ("/", "filter", 2),
      ("?", "help", 8),
      ("q", "quit", 9),
    ],
  };
  let size = |x: &(&str, &str, u8)| x.0.chars().count() + 1 + x.1.len() + 2;
  let mut keep: Vec<_> = hints.to_vec();
  while 1 + keep.iter().map(size).sum::<usize>() > width as usize && !keep.is_empty() {
    let low = (0..keep.len()).min_by_key(|i| keep[*i].2).unwrap();
    keep.remove(low);
  }
  let mut spans = vec![Text::raw(" ")];
  for (k, d, _) in keep {
    spans.push(Text::styled(k, Style::new().fg(Color::Cyan)));
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
