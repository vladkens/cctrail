// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

//! The box frame, as macmon's: a rounded border with titles fitted on the top border (the items
//! of a title can be clicked), a note and the key hints on the bottom border, a scrollbar on the
//! right border and dividers across.

use std::ops::Range;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Scrollbar, ScrollbarOrientation, ScrollbarState};

use super::Click;

/// Borders, separators and inactive choices.
pub const BORDER: Style = Style::new().fg(Color::DarkGray);
/// Between the key hints on a bottom border.
const SEPARATOR: &str = " | ";
/// Fewest cells worth a note on the left of a bottom border; with less room it is left out.
const NOTE_MIN: u16 = 8;

fn width(spans: &[Span]) -> u16 {
  spans.iter().map(Span::width).sum::<usize>().try_into().unwrap_or(u16::MAX)
}

/// The key of a hint or a choice: cyan, which no value uses.
pub fn key(text: impl Into<String>) -> Span<'static> {
  Span::styled(text.into(), Style::new().fg(Color::Cyan))
}

/// The words of a hint: the default color, faint, so it reads the same on light and dark themes.
pub fn faint(text: impl Into<String>) -> Span<'static> {
  Span::styled(text.into(), Style::new().fg(Color::Reset).add_modifier(Modifier::DIM))
}

/// A key hint, as macmon's: a letter key in its word stays in it (`quit`), any other key comes
/// before it (`/ filter`); with no word only the key shows (`?`).
pub fn hint(k: &'static str, label: &str) -> Vec<Span<'static>> {
  let letter = k.as_bytes().first().filter(|c| k.len() == 1 && c.is_ascii_alphabetic());
  if let Some(i) = letter.and_then(|c| label.bytes().position(|b| b == *c)) {
    return vec![faint(&label[..i]), key(&label[i..=i]), faint(&label[i + 1..])];
  }
  if label.is_empty() {
    return vec![key(k)];
  }
  vec![key(k), faint(format!(" {label}"))]
}

/// A title on a border: its spans, each maybe something a click picks.
#[derive(Clone, Default)]
pub struct Title(pub Vec<(Span<'static>, Option<Click>)>);

impl Title {
  pub fn new(spans: impl IntoIterator<Item = Span<'static>>) -> Self {
    Self(spans.into_iter().map(|x| (x, None)).collect())
  }

  pub fn push(mut self, span: Span<'static>, click: Option<Click>) -> Self {
    self.0.push((span, click));
    self
  }

  fn width(&self) -> u16 {
    width(&self.0.iter().map(|x| x.0.clone()).collect::<Vec<_>>())
  }
}

/// Titles on the top border of a box: left titles and an optional right one. When the border is
/// too short, the first left title is cut, while later left titles and then the right one are
/// dropped, so titles never overlap.
#[derive(Clone, Default)]
pub struct Titles {
  pub left: Vec<Title>,
  pub right: Option<Title>,
}

impl Titles {
  /// Whether every title fits uncut on the top border of a box `width` cells wide.
  fn fits(&self, width: u16) -> bool {
    let left: Vec<u16> = self.left.iter().map(|x| x.width() + 2).collect();
    let right = self.right.as_ref().map(|x| x.width() + 2);
    let slots = place_titles(width, &left, right);
    slots.left.iter().map(|&(_, w)| w).eq(left.iter().copied())
      && slots.right.is_some() == right.is_some()
  }

  /// Draws the titles over the top border of `area`, row `y`; returns where their clickable items
  /// went.
  fn render(self, area: Rect, y: u16, buf: &mut Buffer) -> Vec<(Rect, Click)> {
    let left: Vec<u16> = self.left.iter().map(|x| x.width() + 2).collect();
    let right = self.right.as_ref().map(|x| x.width() + 2);
    let slots = place_titles(area.width, &left, right);
    let mut placed: Vec<(Title, u16, u16)> =
      self.left.into_iter().zip(slots.left).map(|(t, (x, w))| (t, x, w)).collect();
    if let (Some(title), Some(x)) = (self.right, slots.right) {
      placed.push((title, x, right.unwrap()));
    }
    let mut clicks = vec![];
    for (title, x, room) in placed {
      let mut spans = vec![Span::raw(" ")];
      let mut at = area.x + x + 1;
      for (span, click) in title.0 {
        let w = width(std::slice::from_ref(&span));
        if let Some(click) = click.filter(|_| at + w <= area.x + x + room) {
          clicks.push((Rect::new(at, y, w, 1), click));
        }
        at += w;
        spans.push(span);
      }
      spans.push(Span::raw(" "));
      let line = Line::from(spans).style(Style::new().fg(Color::Reset));
      buf.set_line(area.x + x, y, &line, room);
    }
    clicks
  }
}

/// The first of `variants` (longest first) whose titles fit uncut on the top border of a box
/// `width` cells wide; when none does, the last one, cut to fit.
pub fn fit_titles(width: u16, variants: Vec<Titles>) -> Titles {
  let mut variants = variants.into_iter().peekable();
  while let Some(titles) = variants.next() {
    if variants.peek().is_none() || titles.fits(width) {
      return titles;
    }
  }
  Titles::default()
}

/// Positions of titles on a border, as offsets from the box's left edge.
#[derive(Debug, Default, PartialEq, Eq)]
struct TitleSlots {
  /// `(x, visible width)` of the left titles that fit.
  left: Vec<(u16, u16)>,
  right: Option<u16>,
}

/// Fits titles of the given widths on a border `width` cells wide. Titles keep at least one border
/// cell between each other and next to the corners. The first left title is cut to fit; the
/// others are placed in full or dropped: left ones first, then the right one.
fn place_titles(width: u16, left: &[u16], right: Option<u16>) -> TitleSlots {
  let (left, free) = place_left(width, left);
  let right = right.filter(|&title| title > 0 && free.start.saturating_add(title) <= free.end);
  TitleSlots { left, right: right.map(|title| free.end - title) }
}

/// The left titles of `place_titles`: their `(x, visible width)` and the cells still free after
/// them, `[start, end)`.
fn place_left(width: u16, left: &[u16]) -> (Vec<(u16, u16)>, Range<u16>) {
  let mut slots = vec![];
  let mut free = 2..width.saturating_sub(2);
  for (i, &title) in left.iter().enumerate() {
    let room = free.end.saturating_sub(free.start);
    if room == 0 || (i > 0 && title > room) {
      break;
    }
    let title = title.min(room);
    slots.push((free.start, title));
    free.start = free.start.saturating_add(title).saturating_add(1);
  }
  (slots, free)
}

/// Draws a rounded box with `titles` on the top border. Returns the area inside the borders and
/// where the titles' clickable items went.
pub fn draw_box(f: &mut Frame, area: Rect, titles: Titles) -> (Rect, Vec<(Rect, Click)>) {
  let block = Block::bordered().border_type(BorderType::Rounded).border_style(BORDER);
  let inner = block.inner(area);
  f.render_widget(block, area);
  (inner, titles.render(area, area.y, f.buffer_mut()))
}

/// Draws a divider across box `area` at row `y`, `├──┤`, with `titles` on it as on the top border.
pub fn draw_divider(f: &mut Frame, area: Rect, y: u16, titles: Titles) -> Vec<(Rect, Click)> {
  let line = format!("├{}┤", "─".repeat(usize::from(area.width.saturating_sub(2))));
  f.buffer_mut().set_string(area.x, y, line, BORDER);
  titles.render(area, y, f.buffer_mut())
}

/// Draws a scrollbar over column `x`, the right border, beside `rows`, the cells of a list of
/// `len` rows scrolled to `offset`, when they don't fit: `↑` and `↓` at the ends, the thumb
/// between them. Fewer than 3 rows have only the arrows where there is more.
pub fn draw_scrollbar(f: &mut Frame, x: u16, rows: Rect, len: usize, offset: usize) {
  let page = usize::from(rows.height);
  if len <= page || page == 0 {
    return;
  }
  let bar = Rect { x, width: 1, ..rows };
  if page < 3 {
    let (up, down) = (offset > 0, offset + page < len);
    let buf = f.buffer_mut();
    let (top, bottom) = (rows.y, rows.bottom() - 1);
    match page {
      1 => {
        _ = buf[(x, top)].set_symbol(if up && down {
          "↕"
        } else if up {
          "↑"
        } else {
          "↓"
        })
      }
      _ => {
        buf[(x, top)].set_symbol(if up { "↑" } else { "│" });
        buf[(x, bottom)].set_symbol(if down { "↓" } else { "│" });
      }
    }
    return;
  }
  // Scroll positions, not rows, so the thumb reaches the bottom at the last one.
  let mut state =
    ScrollbarState::new(len - page + 1).position(offset).viewport_content_length(page);
  let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
    .begin_symbol(Some("↑"))
    .end_symbol(Some("↓"))
    .track_symbol(None)
    .thumb_symbol("┃")
    .style(BORDER)
    .thumb_style(Style::new().fg(Color::Reset));
  f.render_stateful_widget(scrollbar, bar, &mut state);
}

/// `items` joined by a gray ` | `.
fn join(items: impl IntoIterator<Item = Vec<Span<'static>>>) -> Vec<Span<'static>> {
  let mut spans = vec![];
  for (i, item) in items.into_iter().enumerate() {
    if i > 0 {
      spans.push(Span::styled(SEPARATOR, BORDER));
    }
    spans.extend(item);
  }
  spans
}

/// `spans` with a blank cell at both ends.
fn padded(spans: Vec<Span<'static>>) -> Line<'static> {
  let mut line = vec![Span::raw(" ")];
  line.extend(spans);
  line.push(Span::raw(" "));
  Line::from(line)
}

/// Number of leading `items` (by width) that fit in `room` cells joined by ` | `, with a blank
/// cell at both ends.
fn fit_joined(room: u16, items: &[u16]) -> usize {
  let mut used = 2;
  for (i, &item) in items.iter().enumerate() {
    let gap = if i == 0 { 0 } else { SEPARATOR.len() as u32 };
    used += gap + u32::from(item);
    if used > u32::from(room) {
      return i;
    }
  }
  items.len()
}

/// Draws `hints` (right-aligned) and a note (left) over the bottom border of box `area`. Like
/// titles, they keep a border cell next to the corners and between each other. The hints come
/// first and drop from the end when the border is too short, so the first ones stay longest. The
/// note gets the cells left, cut with `…` when longer, or left out with fewer than `NOTE_MIN`.
pub fn render_bottom_border(
  f: &mut Frame,
  area: Rect,
  hints: Vec<Vec<Span<'static>>>,
  note: Vec<Span<'static>>,
) {
  let y = area.bottom() - 1;
  let mut free = area.width.saturating_sub(4);
  let widths: Vec<u16> = hints.iter().map(|x| width(x)).collect();
  let shown = fit_joined(free, &widths);
  if shown > 0 {
    let line = padded(join(hints.into_iter().take(shown)));
    let w = width(&line.spans);
    f.buffer_mut().set_line(area.right() - 2 - w, y, &line, w);
    free = free.saturating_sub(w + 1);
  }
  let room = free.saturating_sub(2);
  if room >= NOTE_MIN && !note.is_empty() {
    let spans = super::draw::cut_spans(note, usize::from(room));
    f.buffer_mut().set_line(area.x + 2, y, &padded(spans), room + 2);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn places_titles_like_macmon() {
    // Two left titles and a right one on a 40-cell border: one border cell between them.
    let slots = place_titles(40, &[10, 8], Some(6));
    assert_eq!(slots, TitleSlots { left: vec![(2, 10), (13, 8)], right: Some(32) });
    // Too short: the right one goes first, then later left ones; the first one is cut.
    assert_eq!(place_titles(24, &[10, 8], Some(6)).right, None);
    assert_eq!(place_titles(12, &[10, 8], None).left, vec![(2, 8)]);
    assert_eq!(fit_joined(20, &[1, 4, 4]), 3);
    assert_eq!(fit_joined(12, &[1, 4, 4]), 2);
  }

  #[test]
  fn hints_and_short_scrollbars_like_macmon() {
    let text = |spans: Vec<Span>| spans.iter().map(|x| x.content.to_string()).collect::<Vec<_>>();
    assert_eq!(text(hint("q", "quit")), ["", "q", "uit"]);
    assert_eq!(text(hint("/", "filter")), ["/", " filter"]);
    assert_eq!(text(hint("?", "")), ["?"]);
    // Two rows of more: only the arrows where there is more.
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(3, 4)).unwrap();
    term.draw(|f| draw_scrollbar(f, 2, Rect::new(0, 1, 2, 2), 5, 1)).unwrap();
    let buf = term.backend().buffer();
    assert_eq!((buf[(2, 1)].symbol(), buf[(2, 2)].symbol()), ("↑", "↓"));
  }
}
