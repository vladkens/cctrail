// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

//! Colors from the terminal's own palette, as the text report: names cyan, time yellow, cost green,
//! counts in the default color; dark gray only for headers, separators, hints and unconfigured
//! directories. The selected row is one even dark gray bar across its width.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as Text};
use ratatui::widgets::{
  Block, Borders, Cell, Clear, Paragraph, Row as TableRow, Sparkline, Table, TableState,
};

use super::{App, Metric, Row, Span, Tab};
use crate::output::{cost, duration, tokens};

const DIM: Style = Style::new().fg(Color::DarkGray);
const KEY: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);
const NAME: Style = Style::new().fg(Color::Cyan);
const TIME: Style = Style::new().fg(Color::Yellow);
const COST: Style = Style::new().fg(Color::Green);
/// The selected row: a background only, so every cell keeps its own text color.
const SELECTED: Style = Style::new().bg(Color::DarkGray);
const VERSION: &str = concat!("cctrail v", env!("CARGO_PKG_VERSION"));

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
  let [head, tabs, body, foot] = Layout::vertical([
    Constraint::Length(1),
    Constraint::Length(2),
    Constraint::Min(1),
    Constraint::Length(1),
  ])
  .areas(f.area());
  header(f, app, head);
  f.render_widget(tab_line(app), tabs);
  match &app.open {
    Some(_) => detail(f, app, body),
    None if app.tab == Tab::Timeline => timeline(f, app, &app.list, body),
    None => table(f, app, &app.list, app.tab == Tab::Projects, body, Some(app.sel())),
  }
  f.render_widget(footer(app), foot);
  // The help is a box over the whole screen, as macmon's.
  if app.help {
    help(f, f.area());
  }
}

/// Periods and totals on the left, `cctrail vX` on the right; the version gives way first.
fn header(f: &mut Frame, app: &App, area: Rect) {
  let mut spans = vec![Text::styled(" cctrail  ", BOLD)];
  for s in Span::ALL {
    let style = if s == app.span { KEY } else { DIM };
    spans.push(Text::styled(format!(" {} ", s.label()), style));
  }
  let prov = app.prov.map_or("claude+codex", |x| x.name());
  let t = &app.total;
  spans.push(dim(format!("   {prov}  ")));
  spans.push(Text::styled(duration(t.active.unwrap_or(0)), TIME));
  spans.push(dim(" · "));
  spans.push(Text::raw(tokens(t.tokens)));
  spans.push(dim(" · "));
  spans.push(Text::styled(cost(t.cost), COST));
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

fn tab_line(app: &App) -> Paragraph<'static> {
  let mut spans = vec![Text::raw(" ")];
  for (i, (tab, name)) in
    [(Tab::Projects, "Projects"), (Tab::Models, "Models"), (Tab::Timeline, "Timeline")]
      .into_iter()
      .enumerate()
  {
    let label = format!("{} {name}", i + 1);
    spans.push(if tab == app.tab {
      Text::styled(format!("[{label}]"), BOLD)
    } else {
      dim(format!(" {label} "))
    });
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
  Paragraph::new(vec![Line::from(spans), Line::from(dim("─".repeat(200)))])
}

/// The percent of the column total.
fn share(value: f64, sum: f64) -> String {
  let pct = if sum > 0.0 { (value / sum * 100.0).round() } else { 0.0 };
  format!("{pct}%")
}

fn table(f: &mut Frame, app: &App, rows: &[Row], sessions: bool, area: Rect, sel: Option<usize>) {
  let m = app.metric;
  let sum: f64 = rows.iter().map(|x| m.of(x)).sum();
  let lines = rows.iter().enumerate().map(|(i, x)| {
    // Unconfigured directories and `other` are dim, but not on the selected row's gray.
    let name = if x.dim && sel != Some(i) { DIM } else { NAME };
    TableRow::new(vec![
      Cell::from(x.label.clone()).style(name),
      num(x.active.map_or("–".into(), duration), TIME),
      num(tokens(x.tokens), Style::new()),
      num(cost(x.cost), COST),
      num(x.sessions.map_or(String::new(), |x| x.to_string()), Style::new()),
      num(share(m.of(x), sum), Style::new()),
    ])
  });
  let head = ["Name", "Time", "Tokens", "Cost", if sessions { "Sessions" } else { "" }, "Share"];
  let head = head.into_iter().enumerate().map(|(i, x)| match i {
    0 => Cell::from(x).style(DIM),
    _ => num(x.to_string(), DIM),
  });
  let widths = [
    Constraint::Fill(1),
    Constraint::Length(12),
    Constraint::Length(9),
    Constraint::Length(11),
    Constraint::Length(8),
    Constraint::Length(6),
  ];
  let t = Table::new(lines, widths)
    .header(TableRow::new(head))
    .row_highlight_style(SELECTED)
    .highlight_symbol("▶ ");
  let mut state = TableState::default().with_selected(sel);
  f.render_stateful_widget(t, area, &mut state);
}

fn timeline(f: &mut Frame, app: &App, rows: &[Row], area: Rect) {
  let m = app.metric;
  let max = rows.iter().map(|x| m.of(x)).fold(0.0, f64::max);
  let width = area.width.saturating_sub(34) as usize;
  let value = |x: &Row| match m {
    Metric::Time => (duration(x.active.unwrap_or(0)), TIME),
    Metric::Tokens => (tokens(x.tokens), Style::new()),
    Metric::Cost => (cost(x.cost), COST),
  };
  let lines = rows.iter().enumerate().map(|(i, x)| {
    let full = if max > 0.0 { (m.of(x) / max * width as f64).round() as usize } else { 0 };
    let empty = m.of(x) == 0.0;
    let (text, style) = if empty { ("–".to_string(), DIM) } else { value(x) };
    TableRow::new(vec![
      Cell::from(x.label.clone()).style(if empty && i != app.sel() { DIM } else { NAME }),
      num(text, style),
      Cell::from(Text::styled("█".repeat(full), NAME)),
    ])
  });
  let widths = [Constraint::Length(16), Constraint::Length(12), Constraint::Fill(1)];
  let head = [Cell::from("Day").style(DIM), num(m.label().to_string(), DIM), Cell::from("")];
  let t = Table::new(lines, widths)
    .header(TableRow::new(head))
    .row_highlight_style(SELECTED)
    .highlight_symbol("▶ ");
  let mut state = TableState::default().with_selected(Some(app.sel()));
  f.render_stateful_widget(t, area, &mut state);
}

/// A drill-down, in place of the list: its totals, its rows, the metric per day and its paths.
fn detail(f: &mut Frame, app: &App, area: Rect) {
  let d = app.open.as_ref().unwrap();
  let r = &d.row;
  let mut title = vec![Text::styled(format!(" {}", r.label), NAME.add_modifier(Modifier::BOLD))];
  title.push(dim(format!(" · {}", app.span.label())));
  if let Some(a) = r.active.filter(|_| app.tab != Tab::Models) {
    title.push(dim(" · "));
    title.push(Text::styled(duration(a), TIME));
  }
  title.push(dim(" · "));
  title.push(Text::raw(tokens(r.tokens)));
  title.push(dim(" · "));
  title.push(Text::styled(cost(r.cost), COST));
  if let Some(n) = r.sessions {
    title.push(dim(format!(" · {n} sessions")));
  }
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
  table(f, app, &d.rows, app.tab == Tab::Timeline, rows, Some(app.osel));
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
    let skip = values.len().saturating_sub(line.width as usize);
    let first = d.days.get(skip).map_or(String::new(), |x| x.key.clone());
    let caption = format!(" {} by {gran} since {first}", m.label());
    f.render_widget(Paragraph::new(Line::from(dim(caption))), label);
    let style = Style::new().fg(Color::Green);
    f.render_widget(Sparkline::default().data(&values[skip..]).style(style), line);
  }
  if paths > 0 {
    let line = Line::from(dim(format!(" Paths: {}", d.paths.join(", "))));
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
    ("esc", "back"),
    ("m", "metric for shares, sorting and bars: time / tokens / cost"),
    ("p", "provider: both / claude / codex"),
    ("/", "filter by name"),
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
