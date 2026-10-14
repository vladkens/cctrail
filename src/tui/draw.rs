// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

//! Colors from the terminal's own palette, as in macmon: the default foreground, dim gray for
//! secondary text, cyan for keys. The selected row is one even dark gray bar across its width.

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
/// The selected row: a background only, so every cell keeps its own text color.
const SELECTED: Style = Style::new().bg(Color::DarkGray);

fn dim(s: impl Into<String>) -> Text<'static> {
  Text::styled(s.into(), DIM)
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
  f.render_widget(header(app), head);
  f.render_widget(tab_line(app), tabs);
  if app.help {
    f.render_widget(help(), body);
  } else if app.tab == Tab::Timeline {
    timeline(f, app, &app.list, body);
  } else {
    table(f, app, &app.list, app.tab == Tab::Projects, body, Some(app.sel()));
  }
  // A drill-down is a box over the list, as macmon's overlays.
  if app.open.is_some() && !app.help {
    detail(f, app, body);
  }
  f.render_widget(footer(app), foot);
}

fn header(app: &App) -> Line<'static> {
  let mut spans = vec![Text::styled(" cctrail  ", BOLD)];
  for s in Span::ALL {
    let style = if s == app.span { KEY } else { DIM };
    spans.push(Text::styled(format!(" {} ", s.label()), style));
  }
  let prov = app.prov.map_or("claude+codex", |x| x.name());
  let t = &app.total;
  spans.push(dim(format!("   {prov}  ")));
  spans.push(Text::raw(format!(
    "{} · {} · {}",
    duration(t.active.unwrap_or(0)),
    tokens(t.tokens),
    cost(t.cost)
  )));
  Line::from(spans)
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
  spans.push(Text::styled(app.metric.label(), Style::new().fg(Color::Yellow)));
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
  format!("{pct:>4}%")
}

fn table(f: &mut Frame, app: &App, rows: &[Row], sessions: bool, area: Rect, sel: Option<usize>) {
  // Models have no time of their own: their share follows tokens when the metric is time.
  let m = app.metric;
  let sum: f64 = rows.iter().map(|x| m.of(x)).sum();
  let lines = rows.iter().enumerate().map(|(i, x)| {
    // Dim text would vanish on the selected row's gray.
    let style = if x.dim && sel != Some(i) { DIM } else { Style::new() };
    TableRow::new(vec![
      Cell::from(x.label.clone()).style(style),
      Cell::from(x.active.map_or("–".into(), duration)),
      Cell::from(tokens(x.tokens)),
      Cell::from(cost(x.cost)),
      Cell::from(x.sessions.map_or(String::new(), |x| x.to_string())),
      Cell::from(share(m.of(x), sum)),
    ])
  });
  let head = ["Name", "Time", "Tokens", "Cost", if sessions { "Sessions" } else { "" }, "Share"];
  let widths = [
    Constraint::Fill(1),
    Constraint::Length(12),
    Constraint::Length(9),
    Constraint::Length(11),
    Constraint::Length(8),
    Constraint::Length(6),
  ];
  let t = Table::new(lines, widths)
    .header(TableRow::new(head.map(|x| Cell::from(x).style(DIM))))
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
    Metric::Time => duration(x.active.unwrap_or(0)),
    Metric::Tokens => tokens(x.tokens),
    Metric::Cost => cost(x.cost),
  };
  let lines = rows.iter().enumerate().map(|(i, x)| {
    let full = if max > 0.0 { (m.of(x) / max * width as f64).round() as usize } else { 0 };
    let empty = m.of(x) == 0.0;
    TableRow::new(vec![
      Cell::from(x.label.clone()).style(if empty && i != app.sel() { DIM } else { Style::new() }),
      Cell::from(if empty { "–".into() } else { value(x) }),
      Cell::from(Text::styled("█".repeat(full), Style::new().fg(Color::Cyan))),
    ])
  });
  let widths = [Constraint::Length(16), Constraint::Length(12), Constraint::Fill(1)];
  let t = Table::new(lines, widths)
    .header(TableRow::new(["Day", m.label(), ""].map(|x| Cell::from(x).style(DIM))))
    .row_highlight_style(SELECTED)
    .highlight_symbol("▶ ");
  let mut state = TableState::default().with_selected(Some(app.sel()));
  f.render_stateful_widget(t, area, &mut state);
}

fn detail(f: &mut Frame, app: &App, area: Rect) {
  let d = app.open.as_ref().unwrap();
  let r = &d.row;
  let mut title =
    vec![Text::styled(format!(" {} ", r.label), BOLD), dim(format!("· {}", app.span.label()))];
  if let Some(a) = r.active.filter(|_| app.tab != Tab::Models) {
    title.push(dim(format!(" · {}", duration(a))));
  }
  title.push(dim(format!(" · {} · {}", tokens(r.tokens), cost(r.cost))));
  if let Some(n) = r.sessions {
    title.push(dim(format!(" · {n} sessions ")));
  }
  let spark = if d.days.is_empty() { 0 } else { 4 };
  let paths = if d.paths.is_empty() { 0 } else { 2 };
  // A box over the list: as tall as its rows need, inside the body.
  let height = (d.rows.len() as u16 + 1 + spark + paths + 2).max(8).min(area.height);
  let width = area.width.saturating_sub(6).max(area.width.min(60));
  let rect = area.centered(Constraint::Length(width), Constraint::Length(height));
  f.render_widget(Clear, rect);
  let block = Block::new().borders(Borders::ALL).border_style(DIM).title(Line::from(title));
  let inner = block.inner(rect);
  f.render_widget(block, rect);
  let [rows, days, foot] =
    Layout::vertical([Constraint::Min(2), Constraint::Length(spark), Constraint::Length(paths)])
      .areas(inner);
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
    f.render_widget(
      Paragraph::new(Line::from(dim(format!(" {} by {gran} since {first}", m.label())))),
      label,
    );
    f.render_widget(
      Sparkline::default().data(&values[skip..]).style(Style::new().fg(Color::Green)),
      line,
    );
  }
  if paths > 0 {
    f.render_widget(
      Paragraph::new(vec![
        Line::default(),
        Line::from(dim(format!(" Paths: {}", d.paths.join(", ")))),
      ]),
      foot,
    );
  }
}

fn help() -> Paragraph<'static> {
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
  let mut lines = vec![Line::from(Text::styled(" Keys", BOLD)), Line::default()];
  lines.extend(
    keys.map(|(k, d)| Line::from(vec![Text::styled(format!("   {k:<8}"), KEY), Text::raw(d)])),
  );
  Paragraph::new(lines)
}

fn footer(app: &App) -> Line<'static> {
  let hints: &[(&str, &str)] = match () {
    _ if app.typing => &[("enter", "keep"), ("esc", "clear")],
    _ if app.open.is_some() || app.help => {
      &[("esc", "back"), ("m", "metric"), ("←→", "period"), ("?", "help"), ("q", "quit")]
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
