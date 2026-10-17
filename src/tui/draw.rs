// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

//! The app in boxes as macmon's (see `boxes`): the header, then the list or the chart over it.
//! Colors from the terminal's own palette, the same for an entity on every screen: names and counts
//! in the default foreground, time yellow, cost green; the picked period, tab and choice cyan; dark
//! gray only for borders, headers, separators, hints and the other choices. The selected row is one
//! reversed band in the text color, as macmon's. Tables keep an equal gutter left (the `▶` column)
//! and right.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as Text};
use ratatui::widgets::{
  Block, BorderType, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row as TableRow, Table,
  TableState,
};

use super::boxes::{self, Title, Titles};
use super::{App, Click, Col, Hits, Row, Sort, Span, Tab};
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
/// The selected row, as macmon's: one reversed band in the text color, no cell's own colors on
/// it, readable on light and dark themes.
const SELECTED: Style = Style::new().fg(Color::Reset).add_modifier(Modifier::REVERSED);
const VERSION: &str = concat!("cctrail v", env!("CARGO_PKG_VERSION"));
/// The room on the right of a table and around the chart.
const GUTTER: u16 = 2;
/// The selected row's mark, a blank cell from the box's left border.
const MARKER: &str = " ▶ ";
const MARKER_WIDTH: u16 = 3;
/// Fewest cells for the names before columns give way.
const NAME_MIN: usize = 12;
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
pub fn cut_spans(spans: Vec<Text<'static>>, width: usize) -> Vec<Text<'static>> {
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
  // Each list keeps its first visible row from the last frame.
  let view = (app.tab, app.open.is_some());
  let offset = app.hits.borrow().offset_of(view);
  let mut hits = Hits { view: Some(view), offset, ..Default::default() };
  let area = f.area();
  if area.height < 8 || area.width < 40 {
    too_small(f, area, &mut hits);
    return *app.hits.borrow_mut() = hits;
  }
  // Boxes as macmon's: the header with the views, periods and provider, then the list (or the
  // chart over the list) with its own title and the keys on its bottom border.
  let [head, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas::<2>(area);
  header(f, app, head, &mut hits);
  match &app.open {
    Some(_) => detail(f, app, body, &mut hits),
    None if app.tab == Tab::Timeline => timeline(f, app, body, &mut hits),
    None if app.tab == Tab::Swears => {
      let inner = list_box(f, app, body, &mut hits);
      swears(f, app, inner, &mut hits);
    }
    None => {
      let inner = list_box(f, app, body, &mut hits);
      let sessions = app.tab == Tab::Projects;
      let name = if app.tab == Tab::Models { "Model" } else { "Project" };
      let rows = (name, &app.list[..], 0);
      table(f, app, rows, &app.total, sessions, inner, Some(app.sel()), &mut hits);
    }
  }
  // The help is a box over the whole screen, as macmon's.
  if app.help {
    hits.help_max = help(f, area, app.help_top);
  }
  *app.hits.borrow_mut() = hits;
}

/// A segmented choice: the picked one in the selection color, the others gray, each clickable.
fn choices<T: Copy + PartialEq>(
  items: impl IntoIterator<Item = (T, String)>,
  picked: T,
  click: impl Fn(T) -> Click,
) -> Title {
  let mut title = Title::default();
  for (i, (x, label)) in items.into_iter().enumerate() {
    if i > 0 {
      title = title.push(Text::raw("  "), None);
    }
    title = title.push(Text::styled(label, if x == picked { KEY } else { DIM }), Some(click(x)));
  }
  title
}

/// The header box: the views and their keys on its top border, the version right of them (the
/// first to go when narrow); inside, the periods left and the provider right. All clickable.
fn header(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  let mut tabs = Title::default();
  let tab_names = [
    (Tab::Projects, "Projects"),
    (Tab::Models, "Models"),
    (Tab::Timeline, "Timeline"),
    (Tab::Swears, "Swears"),
  ];
  for (i, (tab, name)) in tab_names.into_iter().enumerate() {
    if i > 0 {
      tabs = tabs.push(Text::raw("  "), None);
    }
    let style = if tab == app.tab { KEY } else { DIM };
    tabs = tabs.push(Text::styled(name, style), Some(Click::Tab(tab)));
    tabs = tabs.push(Text::raw(" "), Some(Click::Tab(tab)));
    tabs = tabs.push(boxes::key((i + 1).to_string()), Some(Click::Tab(tab)));
  }
  let version = Title::new([Text::styled(VERSION, TEXT)]);
  let titles = Titles { left: vec![tabs], right: Some(version) };
  let (inner, clicks) = boxes::draw_box(f, area, titles);
  hits.clicks.extend(clicks);
  // The periods and the provider, a blank cell from the borders.
  let line = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
  let periods = choices(Span::ALL.map(|x| (x, x.label().to_string())), app.span, Click::Period);
  let mut x = line.x;
  let mut spans = vec![];
  for (span, click) in periods.0 {
    let w = span.width() as u16;
    if let Some(click) = click.filter(|_| x + w <= line.right()) {
      hits.clicks.push((Rect::new(x, line.y, w, 1), click));
    }
    x += w;
    spans.push(span);
  }
  f.render_widget(Line::from(spans), line);
  let provider = app.prov.map_or("claude+codex+pi+fx", |x| x.name());
  let w = provider.len() as u16;
  if x + 2 + w <= line.right() {
    let at = Rect::new(line.right() - w, line.y, w, 1);
    hits.clicks.push((at, Click::Provider));
    f.render_widget(Text::styled(provider, TEXT), at);
  }
}

/// The list's own name with its rows' count (`3/42` with a filter), the filter as macmon's (`/
/// filter`, or what is typed), `sort`; the row action on the right. In a drill-down, the path,
/// a click on which goes back.
fn list_titles(app: &App, width: u16, name: &str) -> Titles {
  let heading = |x: &str| Text::styled(x.to_string(), TEXT.add_modifier(Modifier::BOLD));
  let sort = Title::new(boxes::hint("s", "sort"));
  if let Some(d) = &app.open {
    let path = Title::default()
      .push(Text::styled(d.row.label.clone(), TEXT), Some(Click::Tab(app.tab)))
      .push(dim(" › "), Some(Click::Tab(app.tab)))
      .push(heading(name), None)
      .push(Text::styled(format!(" {}", d.rows().len()), TEXT), None);
    return Titles { left: vec![path, sort], right: None };
  }
  let shown = app.list.len();
  let count = match app.filter.is_empty() && !app.typing {
    true => format!(" {}", app.all),
    false => format!(" {shown:>w$}/{}", app.all, w = app.all.to_string().len()),
  };
  let title = Title::new([heading(name), Text::styled(count, TEXT)]);
  let filter = match app.filter.is_empty() && !app.typing {
    true => Title::new(boxes::hint("/", "filter")),
    false => {
      let room = width.saturating_sub(title.width() + 10) as usize;
      let text = app.filter.chars().rev().take(room.max(4)).collect::<Vec<_>>();
      let text: String = text.into_iter().rev().collect();
      let mut spans = vec![heading("/"), Text::styled(text, TEXT)];
      if app.typing {
        spans.push(Text::styled("▏", TEXT));
      }
      Title::new(spans)
    }
  };
  let left = if app.typing { vec![title, filter] } else { vec![title, filter, sort] };
  let opens = !app.typing && !app.list.is_empty() && app.tab != Tab::Swears;
  let open = opens.then(|| Title::new(boxes::hint("→", "open")));
  Titles { left, right: open }
}

/// Draws the list box with its titles, the keys and the note on its bottom border; returns the
/// area inside.
fn list_box(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) -> Rect {
  let name = match app.tab {
    Tab::Projects => "projects",
    Tab::Models | Tab::Swears => "models",
    Tab::Timeline if app.span.gran() == crate::metrics::Granularity::Week => "weeks",
    Tab::Timeline => "days",
  };
  boxed(f, app, area, list_titles(app, area.width, name), hits)
}

/// A list box with `titles`: draws it, its keys and note, and returns the table's area.
fn boxed(f: &mut Frame, app: &App, area: Rect, titles: Titles, hits: &mut Hits) -> Rect {
  let (inner, clicks) = boxes::draw_box(f, area, titles);
  hits.clicks.extend(clicks);
  boxes::render_bottom_border(f, area, hints(app), note(app));
  inner
}

/// The keys on the list's bottom border, those to keep longest first: the global ones only, the
/// filter and the row's keys are on its title.
fn hints(app: &App) -> Vec<Vec<Text<'static>>> {
  use boxes::hint;
  match () {
    _ if app.typing => vec![hint("Enter", "keep"), hint("Esc", "clear")],
    // The box itself says how to close it.
    _ if app.help => vec![],
    _ if app.open.is_some() => vec![hint("?", "help"), hint("←", "back"), hint("t", "period")],
    _ => vec![hint("?", "help"), hint("q", "quit"), hint("t", "period"), hint("p", "provider")],
  }
}

/// The note on the bottom border: a project's paths in its drill-down, else the tokens on models
/// without a price.
fn note(app: &App) -> Vec<Text<'static>> {
  if let Some(d) = app.open.as_ref().filter(|x| !x.paths.is_empty()) {
    return vec![dim("paths "), Text::styled(d.paths.join(", "), TEXT)];
  }
  if app.tab == Tab::Swears {
    return vec![dim(SWEARS)];
  }
  if app.unpriced.is_empty() {
    return vec![];
  }
  let t: u64 = app.unpriced.iter().fold(0, |a, x| a.saturating_add(x.1));
  let names: Vec<&str> = app.unpriced.iter().map(|x| x.0.as_str()).collect();
  vec![dim("unpriced "), Text::styled(format!("{} {}", toks(t), names.join(", ")), TEXT)]
}

/// The header with the sort arrow on the sorted column, as macmon's: `↓` most first, `↑` least
/// first (names A to Z).
fn head_label(sort: Sort, col: Col, label: &str) -> String {
  match sort {
    (c, most) if c == col => format!("{label} {}", if most { "↓" } else { "↑" }),
    _ => label.to_string(),
  }
}

/// A column: its header, what a click on it sorts by, its width, and whether it is sorted.
struct Column {
  head: String,
  col: Col,
  width: Constraint,
}

/// The header of the sorted column in the text color, the others gray, all bold, as macmon's.
fn head_style(head: &str) -> Style {
  let sorted = head.ends_with('↓') || head.ends_with('↑');
  Style::new().fg(if sorted { Color::Reset } else { Color::DarkGray }).add_modifier(Modifier::BOLD)
}

/// Lays out rows under a header, the `total` row right under them, both gutters and a scrollbar
/// in the right one when the rows don't fit, and records where the header columns and rows went
/// (`first`: the index of its first row among all the tables' rows). Returns the height used.
#[allow(clippy::too_many_arguments)]
fn grid(
  f: &mut Frame,
  columns: &[Column],
  lines: Vec<TableRow<'static>>,
  total: TableRow<'static>,
  area: Rect,
  sel: Option<usize>,
  first: usize,
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
  hits.heads.push(Rect { height: 1, ..table });
  let rows = Rect { y: table.y + 1, height: height.saturating_sub(1), ..table };
  // The first table keeps its scrolling from the last frame, so the selection moves on screen as
  // little as it can.
  let kept = if hits.rows.is_empty() { hits.offset } else { 0 };
  let head = columns.iter().enumerate().map(|(i, c)| match i {
    0 => Cell::from(c.head.clone()).style(head_style(&c.head)),
    _ => num(c.head.clone(), head_style(&c.head)),
  });
  let t = Table::new(lines, widths.clone())
    .header(TableRow::new(head))
    .row_highlight_style(SELECTED)
    .highlight_symbol(MARKER)
    .highlight_spacing(HighlightSpacing::Always);
  let offset = kept.min(n.saturating_sub(usize::from(height.saturating_sub(1))));
  let mut state = TableState::default().with_offset(offset).with_selected(sel);
  f.render_stateful_widget(t, table, &mut state);
  if hits.rows.is_empty() {
    hits.offset = state.offset();
  }
  hits.rows.push((rows, first + state.offset()));
  boxes::draw_scrollbar(f, area.right(), rows, n, state.offset());
  if foot.height > 0 {
    // Unselected, but with the same `▶ ` column, so its cells line up with the rows'.
    let t = Table::new([total.style(BOLD)], widths)
      .highlight_symbol(MARKER)
      .highlight_spacing(HighlightSpacing::Always);
    f.render_widget(t, foot);
  }
  height + foot.height
}

/// The column rectangles of a table in `area`, after the ` ▶ ` column.
fn layout(widths: &[Constraint], area: Rect) -> std::rc::Rc<[Rect]> {
  Layout::horizontal(widths.to_vec()).spacing(1).split(Rect {
    x: area.x + MARKER_WIDTH,
    width: area.width.saturating_sub(MARKER_WIDTH),
    ..area
  })
}

fn too_small(f: &mut Frame, area: Rect, hits: &mut Hits) {
  hits.rows = vec![];
  f.render_widget(Paragraph::new(dim("  The terminal is too small")), area);
}

fn empty(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  hits.rows = vec![];
  let text = match app.filter.is_empty() || !app.list.is_empty() {
    true => "No activity in this period".to_string(),
    false => format!("No match for {:?}", app.filter),
  };
  f.render_widget(Paragraph::new(dim(format!("  {text}"))), area);
}

/// A table of `rows` under the header `name`, the first of them `first` among all the tables'
/// rows, with `total` as its last row.
#[allow(clippy::too_many_arguments)]
fn table(
  f: &mut Frame,
  app: &App,
  (name, rows, first): (&str, &[Row], usize),
  total: &Row,
  sessions: bool,
  area: Rect,
  sel: Option<usize>,
  hits: &mut Hits,
) -> u16 {
  if rows.is_empty() {
    empty(f, app, area, hits);
    return 1;
  }
  let sort = app.sort();
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
  // Columns go when the names would get too few cells, as macmon's: Sessions first, then Share,
  // Tokens, Cost; never the sorted one, so a number is never cut.
  let name_width = |columns: &[Column]| {
    let mut widths: Vec<Constraint> = columns.iter().map(|x| x.width).collect();
    widths.push(Constraint::Length(GUTTER - 1));
    layout(&widths, area)[0].width as usize
  };
  for col in [Col::Sessions, Col::Share, Col::Tokens, Col::Cost] {
    if name_width(&columns) >= NAME_MIN {
      break;
    }
    if col != sort.0 {
      columns.retain(|x| x.col != col);
    }
  }
  let name_width = name_width(&columns);

  let of = sort.0.share();
  let share = shares(&rows.iter().map(|x| of.of(x)).collect::<Vec<_>>());
  let cells = |x: &Row, label: &str, share: &str| {
    let dash = || num("–".into(), NONE);
    // Nothing in it (a day, or a period's total): dashes, not zeros.
    let blank = x.tokens == 0 && x.active.unwrap_or(0) == 0;
    let cell = |col: Col| match col {
      Col::Name => Cell::from(cut(label, name_width)).style(TEXT),
      _ if blank => dash(),
      Col::Time => x.active.map_or_else(dash, |t| num(duration(t), TIME)),
      Col::Tokens => num(toks(x.tokens), TEXT),
      Col::Cost if x.unpriced => dash(),
      Col::Cost => num(money(x.cost), COST),
      Col::Sessions => num(x.sessions.map_or(String::new(), |x| x.to_string()), TEXT),
      // An unknown cost has no share of the cost either.
      Col::Share if x.unpriced && of == Col::Cost => dash(),
      _ => num(share.to_string(), TEXT),
    };
    TableRow::new(columns.iter().map(|c| cell(c.col)).collect::<Vec<_>>())
  };
  let lines = rows.iter().zip(&share).map(|(x, s)| cells(x, &x.label, s)).collect();
  let all = if rows.iter().any(|x| of.of(x) > 0.0) { "100%" } else { "" };
  let total = cells(total, "total", all);
  grid(f, &columns, lines, total, area, sel, first, hits)
}

/// What the swears tab's numbers are, on its bottom border and in the help.
const SWEARS: &str = "% dir: messages scolding the model · % any: messages with any swearing · \
  Score: % dir adjusted for few messages";

/// The swears tab: per model the prompts to it, the words typed, the percent swearing at it, the
/// percent with any swearing and the score; shares of too few prompts dash.
fn swears(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  if app.list.is_empty() {
    return empty(f, app, area, hits);
  }
  let sort = app.sort();
  let column = |col, label, width: u16| Column {
    head: head_label(sort, col, label),
    col,
    width: Constraint::Length(width + MARK),
  };
  let mut columns = vec![
    Column {
      head: head_label(sort, Col::Name, "Model"),
      col: Col::Name,
      width: Constraint::Fill(1),
    },
    column(Col::Prompts, "Msgs", 6),
    column(Col::Words, "Words", 7),
    column(Col::Directed, "% dir", 6),
    column(Col::Any, "% any", 6),
    column(Col::Score, "Score", 6),
  ];
  // Columns go when the names would get too few cells, never the sorted one.
  let name_width = |columns: &[Column]| {
    let mut widths: Vec<Constraint> = columns.iter().map(|x| x.width).collect();
    widths.push(Constraint::Length(GUTTER - 1));
    layout(&widths, area)[0].width as usize
  };
  for col in [Col::Any, Col::Words, Col::Prompts, Col::Directed] {
    if name_width(&columns) >= NAME_MIN {
      break;
    }
    if col != sort.0 {
      columns.retain(|x| x.col != col);
    }
  }
  let name_width = name_width(&columns);
  let cells = |x: &Row| {
    let rate = |v: Option<f64>, unit: &str| match v {
      Some(v) => num(format!("{v:.1}{unit}"), TEXT),
      None => num("–".into(), NONE),
    };
    let cell = |col: Col| match col {
      Col::Prompts => num(toks(x.prompts), TEXT),
      Col::Words => num(toks(x.words), TEXT),
      Col::Directed => rate(x.directed_share(), "%"),
      Col::Any => rate(x.any_share(), "%"),
      Col::Score => rate(x.score, ""),
      _ => Cell::from(cut(&x.label, name_width)).style(TEXT),
    };
    TableRow::new(columns.iter().map(|c| cell(c.col)).collect::<Vec<_>>())
  };
  let lines = app.list.iter().map(cells).collect();
  grid(f, &columns, lines, cells(&app.total), area, Some(app.sel()), 0, hits);
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

/// The models' colors in the timeline's bars, the largest first: mid tones of the 256-color
/// palette, readable on dark and light themes alike.
const MODELS: [Color; 4] =
  [Color::Indexed(33), Color::Indexed(170), Color::Indexed(36), Color::Indexed(172)];
/// The rest of the models together.
const OTHER: Color = Color::Indexed(245);

/// The timeline: the chart over the top half, the list of its days (weeks for all of the
/// history) under it, the chart marking the selected one.
fn timeline(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  // Room for three rows of bars over three rows of days.
  if area.height < 14 {
    return too_small(f, area, hits);
  }
  let high = area.height / 2;
  chart(f, app, Rect { height: high, ..area }, hits);
  let list = Rect { y: area.y + high, height: area.height - high, ..area };
  let inner = list_box(f, app, list, hits);
  let week = app.span.gran() == crate::metrics::Granularity::Week;
  let rows = (if week { "Week" } else { "Day" }, &app.list[..], 0);
  table(f, app, rows, &app.total, true, inner, Some(app.sel()), hits);
}

/// The model's name without its provider, for the legend.
fn short(key: &str) -> &str {
  let name = key.split_once('/').map_or(key, |x| x.1);
  name.strip_prefix("claude-").unwrap_or(name)
}

/// The chart box: bars of what `app.bars` picks, a bar per day stacked by model, the dates under
/// some bars; on its top border the choice of time, tokens or cost, the largest value and the
/// legend.
fn chart(f: &mut Frame, app: &App, outer: Rect, hits: &mut Hits) {
  let col = app.bars;
  let week = app.span.gran() == crate::metrics::Granularity::Week;
  let mut days: Vec<(usize, &Row)> = app.list.iter().enumerate().collect();
  days.sort_by_key(|x| x.1.day);
  let max = days.iter().map(|x| col.of(x.1)).fold(0.0, f64::max);
  // The choice: clickable, the picked one in the selection color, as the periods.
  let names = [(Col::Time, "Time"), (Col::Tokens, "Tokens"), (Col::Cost, "Cost")];
  let choice = choices(names.map(|(c, n)| (c, n.to_string())), col, Click::Bars);
  let by = dim(if week { "by week" } else { "by day" });
  let titles = |info: Vec<Text<'static>>, legend: Option<Title>| Titles {
    left: vec![choice.clone(), Title::new(info)],
    right: legend,
  };
  if max == 0.0 {
    let (inner, clicks) = boxes::draw_box(f, outer, titles(vec![by], None));
    hits.clicks.extend(clicks);
    return f.render_widget(Paragraph::new(dim(" No activity in this period")), inner);
  }
  let top = days.iter().max_by(|a, b| col.of(a.1).total_cmp(&col.of(b.1))).unwrap().1;
  let (text, style) = value(col, top);
  let largest = Text::styled(text, style);

  // The models in colors: the largest by all-time tokens keep theirs on every screen; as many as
  // the legend can name, the rest together in `other`. When narrow, the title gives up `by day`,
  // then the word `max`, for the legend; with no room for one model, plain bars and no legend.
  let models = app.data.bucket_models(app.span, app.prov);
  let rank = app.data.model_rank();
  let present: std::collections::HashSet<&str> =
    models.values().flatten().filter(|x| col.of(x) > 0.0).map(|x| x.key.as_str()).collect();
  let ranked: Vec<(usize, &str)> = rank
    .iter()
    .take(MODELS.len())
    .enumerate()
    .filter(|x| present.contains(x.1.as_str()))
    .map(|(i, k)| (i, k.as_str()))
    .collect();
  let infos = [
    vec![by.clone(), dim(" · max "), largest.clone()],
    vec![dim("max "), largest.clone()],
    vec![largest.clone()],
  ];
  let fitted = infos.iter().find_map(|info| {
    (1..=ranked.len()).rev().find_map(|count| {
      let (colored, legend) = legend(&ranked[..count], present.len());
      let titles = titles(info.clone(), Some(legend));
      titles.fits(outer.width).then_some((titles, colored))
    })
  });
  let (titles, colored) = fitted.unwrap_or_else(|| (titles(infos[0].clone(), None), vec![]));
  let (area, clicks) = boxes::draw_box(f, outer, titles);
  hits.clicks.extend(clicks);

  // Inside: the bars, the axis, the dates.
  let height = area.height.saturating_sub(2);
  let bars = Rect { height, ..area };
  let x0 = area.x + GUTTER;
  let room = usize::from(area.right().saturating_sub(GUTTER + x0));
  // Bars of one width, at most seven cells, and gaps of one width, as fill the room best, the
  // newest at the right edge; the columns left over stay left of the first bar.
  let n = days.len();
  let shown = room.div_ceil(2).min(n).max(1);
  let fit = |gap: usize| {
    let width = ((room + gap) / shown).saturating_sub(gap).clamp(1, 7);
    (shown * (width + gap) - gap, width, gap)
  };
  let (_, width, gap) =
    [fit(1), fit(2)].into_iter().filter(|x| x.0 <= room).max().unwrap_or(fit(1));
  let pitch = width + gap;
  let left = (room + gap).saturating_sub(shown * pitch);
  let bar = |i: usize| (left + i * pitch, width);
  let sel = days.iter().position(|x| x.0 == app.sel()).unwrap_or(n - 1);
  let first = n.saturating_sub(shown).min(sel);
  let empty: Vec<Row> = vec![];
  let buf = f.buffer_mut();
  for (i, (index, row)) in days.iter().enumerate().skip(first).take(shown) {
    let (dx, width) = bar(i - first);
    let x = x0 + dx as u16;
    let v = col.of(row);
    let eighths = match v > 0.0 {
      true => ((v / max * height as f64 * 8.0).round() as usize).max(1),
      false => 0,
    };
    // The cells of each model, by rank from the bottom, `other` on top; the selected bar whole
    // in the text color, so it stands out on light and dark themes.
    let parts = row.day.and_then(|d| models.get(&d)).unwrap_or(&empty);
    let mut stack: Vec<(Color, f64)> = colored.iter().map(|x| (MODELS[x.0], 0.0)).collect();
    stack.push((OTHER, 0.0));
    for p in parts {
      let at = colored.iter().position(|x| x.1 == p.key).unwrap_or(colored.len());
      stack[at].1 += col.of(p);
    }
    let sum: f64 = stack.iter().map(|x| x.1).sum();
    let cells = eighths.div_ceil(8);
    let counts = match sum > 0.0 {
      true => {
        let parts: Vec<f64> = stack.iter().map(|x| x.1 / sum * cells as f64).collect();
        largest_remainder(&parts, cells as i64)
      }
      false => vec![0; stack.len()],
    };
    let mut colors: Vec<Color> = vec![];
    for (k, c) in counts.iter().enumerate() {
      colors.extend(std::iter::repeat_n(stack[k].0, *c as usize));
    }
    for line in 0..height as usize {
      let y = bars.bottom() - 1 - line as u16;
      let c = colors.get(line).copied().unwrap_or(OTHER);
      for w in 0..width as u16 {
        buf[(x + w, y)].set_symbol(bar_cell(eighths, line)).set_style(Style::new().fg(c));
      }
    }
    // The selected bar keeps its models' colors, marked by a `▼` over its top: in the row above
    // it, on the top row over its top cell when it reaches there.
    if i == sel {
      let top = (eighths.div_ceil(8) as u16).min(height.saturating_sub(1));
      let y = bars.bottom() - 1 - top;
      let mark = Style::new().fg(Color::Cyan);
      buf[(x + width as u16 / 2, y)].set_symbol("▼").set_style(mark);
    }
    hits.bars.push((Rect::new(x, bars.y, width as u16, height + 2), *index));
  }
  // The axis, the selected slot marked on it even when its day is empty.
  let base = bars.bottom();
  buf.set_string(x0, base, "─".repeat(room), DIM);
  let (dx, width) = bar(sel - first);
  buf.set_string(x0 + dx as u16, base, "━".repeat(width), Style::new().fg(Color::Cyan));
  // Dates under their bars every few bars, counted back from the newest, all shifted as far as
  // the newest must be to stay on screen, so the gaps are even; the selected one's in the
  // selection color when it is among them.
  let date = |x: &Row| {
    x.day
      .map_or(String::new(), |d| if week { d.to_string() } else { d.strftime("%m-%d").to_string() })
  };
  let wide: usize = if week { 10 } else { 5 };
  let last = first + shown - 1;
  let centred = |i: usize| {
    let (dx, width) = bar(i - first);
    (dx + width / 2).saturating_sub(wide / 2)
  };
  let pushed = centred(last).saturating_sub(room.saturating_sub(wide));
  // Where a label goes, if it fits there whole: not cut at the left edge.
  let place = |i: usize| {
    let (dx, width) = bar(i - first);
    (dx + width / 2).checked_sub(wide / 2 + pushed)
  };
  let every = (wide + 2).div_ceil(pitch);
  // Each two cells clear of the one drawn after it, by where they are drawn.
  let mut marks: Vec<(usize, usize)> = vec![];
  for i in (first..=last).rev().step_by(every) {
    let Some(x) = place(i) else { continue };
    if marks.last().is_none_or(|(next, _)| x + wide + 2 <= *next) {
      marks.push((x, i));
    }
  }
  marks.sort();
  for (x, i) in marks {
    let style = if i == sel { Style::new().fg(Color::Cyan) } else { DIM };
    buf.set_stringn(x0 + x as u16, base + 1, date(days[i].1), wide, style);
  }
}

/// The legend for `shown` models (their rank, their key) out of `present` ones: their names in
/// full, `other` after them when any is left out. Returns the models that get their color and the
/// legend.
fn legend<'a>(shown: &[(usize, &'a str)], present: usize) -> (Vec<(usize, &'a str)>, Title) {
  let mut title = Title::default();
  for (n, (i, key)) in shown.iter().enumerate() {
    let gap = if n > 0 { "  " } else { "" };
    title = title.push(Text::raw(gap), None);
    title = title.push(Text::styled("■ ", Style::new().fg(MODELS[*i])), None);
    title = title.push(Text::styled(short(key).to_string(), TEXT), None);
  }
  if present > shown.len() {
    title = title.push(Text::styled("  ■ ", Style::new().fg(OTHER)), None);
    title = title.push(Text::styled("other", TEXT), None);
  }
  (shown.to_vec(), title)
}

/// A drill-down, in place of the list, in its box with the path as its title: its tables one
/// under another, a blank line between them, each with the opened row as its total; the selection
/// counts through all of them.
fn detail(f: &mut Frame, app: &App, area: Rect, hits: &mut Hits) {
  let d = app.open.as_ref().unwrap();
  let name = ["models", "usage", "projects", "models"][app.tab as usize];
  let area = boxed(f, app, area, list_titles(app, area.width, name), hits);
  // The tables from the first that lets the selected one fit under it, so the selection is in
  // view; arrows on the right border where tables are out of it.
  let starts: Vec<usize> =
    d.parts.iter().scan(0, |n, x| Some(std::mem::replace(n, *n + x.1.len()))).collect();
  let picked = starts.iter().rposition(|x| *x <= app.osel).unwrap_or(0);
  let high = |part: &[(&str, Vec<Row>)]| part.iter().map(|x| x.1.len() as u16 + 3).sum::<u16>();
  let from =
    (0..=picked).find(|i| high(&d.parts[*i..=picked]) <= area.height + 1).unwrap_or(picked);
  let mut y = area.y;
  let mut last = from;
  for (i, (name, rows)) in d.parts.iter().enumerate().skip(from) {
    let room = Rect { y, height: area.bottom().saturating_sub(y), ..area };
    if room.height < 3 {
      break;
    }
    let first = starts[i];
    let sel = app.osel.checked_sub(first).filter(|x| *x < rows.len());
    y += table(f, app, (name, rows, first), &d.row, d.sessions, room, sel, hits) + 1;
    last = i;
  }
  let buf = f.buffer_mut();
  if from > 0 {
    buf[(area.right(), area.y)].set_symbol("↑");
  }
  if last + 1 < d.parts.len() {
    buf[(area.right(), area.bottom() - 1)].set_symbol("↓");
  }
}

/// The keys in a rounded box centered over `area`, wrapped by words when narrow, from line `top`
/// when they don't fit, with a scrollbar then. Returns how far it can scroll.
fn help(f: &mut Frame, area: Rect, top: u16) -> u16 {
  let keys = [
    ("1 2 3 4", "projects / models / timeline / swears"),
    ("t T", "period: today, week, month, 30 days, all; wraps around"),
    ("↑ ↓", "select a row or a day; wraps around"),
    ("→ enter", "open: project → models, model → speed and effort, day → projects"),
    ("← esc ⌫", "back"),
    ("PgUp PgDn", "a page up / down"),
    ("Home End", "the first / last row"),
    ("s", "sort by the next column; on the timeline: bars of time / tokens / cost"),
    ("S", "reverse the sort"),
    ("p", "provider: all / claude / codex / pi / fx"),
    ("/", "filter by name"),
    (
      "mouse",
      "click a row, bar or choice to select it, a row or bar again to open it; a header sorts, again reverses; the wheel moves 3 rows",
    ),
    ("swears", SWEARS),
    ("?", "this help"),
    ("q", "back; quits from a list"),
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
  let shown = rect.height.saturating_sub(2);
  let max = (lines.len() as u16).saturating_sub(shown);
  let top = top.min(max);
  let len = lines.len();
  f.render_widget(Paragraph::new(lines).block(block).scroll((top, 0)), rect);
  let rows = Rect { y: rect.y + 1, height: shown, ..rect };
  boxes::draw_scrollbar(f, rect.right() - 1, rows, len, usize::from(top));
  max
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
