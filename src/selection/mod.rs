//! Selecting and copying message text with the mouse. A message is any ID
//! with the text its body rows show, so nothing here knows about Matrix.

use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use crossterm::execute;
use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use std::io::stdout;
use std::ops::Range;

#[cfg(test)]
pub(crate) mod tests;

/// Put the text on the clipboard, with OSC 52. Only the clipboard: libghostty
/// drops a sequence that names more than one destination, such as `cp`.
pub fn copy(text: &str) -> std::io::Result<()> {
    execute!(stdout(), CopyToClipboard::to_clipboard_from(text))
}

/// What a mouse event asks the app to do.
#[derive(Debug, PartialEq)]
pub enum Action {
    Copy(String),
}

/// Where `part` starts in `base`, if it's a slice of it rather than a copy.
pub fn offset_in(base: &str, part: &str) -> Option<usize> {
    let start = (part.as_ptr() as usize).checked_sub(base.as_ptr() as usize)?;
    (start + part.len() <= base.len()).then_some(start)
}

/// Some of a message's text on a row: the column it starts at, and its bytes
/// in the text.
#[derive(Debug)]
pub struct Run {
    pub column: u16,
    pub range: Range<usize>,
}

/// A row that shows some of a message's text.
pub struct BodyRow<'a, Id> {
    pub message: Id,
    pub text: &'a str,
    pub runs: Vec<Run>,
}

impl<'a, Id> BodyRow<'a, Id> {
    /// The row these spans draw, if any of them are slices of the text.
    /// Every span counts towards the columns, so gutters and bullets push
    /// the text along as they do on screen.
    pub fn new(message: Id, text: &'a str, spans: &[Span]) -> Option<Self> {
        let mut column = 0u16;
        let mut runs = vec![];

        for span in spans {
            if let Some(start) = offset_in(text, &span.content)
                && !span.content.is_empty()
            {
                runs.push(Run {
                    column,
                    range: start..start + span.content.len(),
                });
            }

            column = column.saturating_add(span.width() as u16);
        }

        (!runs.is_empty()).then_some(Self {
            message,
            text,
            runs,
        })
    }
}

/// The body text on screen as of the last draw: which cells show which
/// bytes of which message's text.
pub struct TextMap<Id> {
    /// The list's area.
    pub area: Rect,
    rows: Vec<Row<Id>>,
    texts: Vec<Text<Id>>,
}

/// A visible message's text, and the bytes of it drawn on any of its rows,
/// on screen or not.
struct Text<Id> {
    message: Id,
    text: String,
    drawn: Vec<Range<usize>>,
}

struct Row<Id> {
    y: u16,
    message: Id,
    cells: Vec<Cell>,
}

/// A grapheme on screen: its first column, how many columns it takes, and
/// its bytes in the text.
struct Cell {
    x: u16,
    width: u16,
    range: Range<usize>,
}

impl<Id: Clone + PartialEq> TextMap<Id> {
    pub fn new(area: Rect) -> Self {
        Self {
            area,
            rows: vec![],
            texts: vec![],
        }
    }

    /// Add a row drawn at screen row `y`, with its first span at column `x`.
    /// Graphemes are laid out as ratatui draws them, and those past the
    /// area's edge are left out.
    pub fn add_row(&mut self, x: u16, y: u16, row: &BodyRow<Id>) {
        let mut cells = vec![];

        for run in &row.runs {
            let mut x = x.saturating_add(run.column);
            let span = Span::raw(&row.text[run.range.clone()]);

            for grapheme in span.styled_graphemes(Style::default()) {
                let width = grapheme.symbol.cell_width();

                if x.saturating_add(width) > self.area.right() {
                    break;
                }

                if width > 0
                    && let Some(start) = offset_in(row.text, grapheme.symbol)
                {
                    cells.push(Cell {
                        x,
                        width,
                        range: start..start + grapheme.symbol.len(),
                    });
                }

                x += width;
            }
        }

        if cells.is_empty() {
            return;
        }

        if self.text(&row.message).is_none() {
            self.texts.push(Text {
                message: row.message.clone(),
                text: row.text.to_string(),
                drawn: vec![],
            });
        }

        self.add_drawn(row);

        self.rows.push(Row {
            y,
            message: row.message.clone(),
            cells,
        });
    }

    /// Note what a row of a visible message draws, when the row itself is
    /// off screen, so widen() doesn't take its text for hidden markup.
    pub fn add_drawn(&mut self, row: &BodyRow<Id>) {
        if let Some(text) = self.texts.iter_mut().find(|t| t.message == row.message) {
            text.drawn
                .extend(row.runs.iter().map(|run| run.range.clone()));
        }
    }

    /// The message's text.
    pub fn text(&self, message: &Id) -> Option<&str> {
        self.texts
            .iter()
            .find(|t| &t.message == message)
            .map(|t| t.text.as_str())
    }

    /// The range, moved out at each end over characters that aren't drawn,
    /// such as markdown's, up to a line break. A selection that starts on a
    /// list item's first word takes its `* `, and one across bold text
    /// takes both `**`. Too much is easier to fix after pasting than too
    /// little. Markdown hides only punctuation and spaces, so a letter or
    /// digit stops it too, such as in lines past `message_line_limit`.
    pub fn widen(&self, message: &Id, range: Range<usize>) -> Range<usize> {
        let Some(text) = self.texts.iter().find(|t| &t.message == message) else {
            return range;
        };

        let hidden = |i: usize, c: char| {
            c != '\n' && !c.is_alphanumeric() && !text.drawn.iter().any(|r| r.contains(&i))
        };

        let start = text.text[..range.start]
            .char_indices()
            .rev()
            .take_while(|&(i, c)| hidden(i, c))
            .last()
            .map_or(range.start, |(i, _)| i);

        let end = text.text[range.end..]
            .char_indices()
            .take_while(|&(i, c)| hidden(range.end + i, c))
            .last()
            .map_or(range.end, |(i, c)| range.end + i + c.len_utf8());

        start..end
    }

    /// The message and grapheme under the pointer, if it's on body text.
    pub fn hit(&self, x: u16, y: u16) -> Option<(Id, Range<usize>)> {
        self.rows.iter().filter(|r| r.y == y).find_map(|row| {
            let cell = row
                .cells
                .iter()
                .find(|c| (c.x..c.x + c.width).contains(&x))?;
            Some((row.message.clone(), cell.range.clone()))
        })
    }

    /// The message's grapheme closest to the pointer. On one of its rows,
    /// that's the grapheme under the pointer, clamped to the row's text.
    /// Off them, it's the start of the next row down, or the end of the last
    /// row when there's none below.
    pub fn nearest(&self, message: &Id, x: u16, y: u16) -> Option<Range<usize>> {
        let rows = || self.rows.iter().filter(|r| &r.message == message);

        let cell = if let Some(row) = rows().find(|r| r.y == y) {
            row.cells
                .iter()
                .find(|c| x < c.x + c.width)
                .or(row.cells.last())
        } else if let Some(below) = rows().filter(|r| r.y > y).min_by_key(|r| r.y) {
            below.cells.first()
        } else {
            rows().max_by_key(|r| r.y)?.cells.last()
        };

        cell.map(|c| c.range.clone())
    }

    /// The screen cells showing these bytes of the message's text.
    pub fn cells(&self, message: &Id, range: Range<usize>) -> impl Iterator<Item = Rect> {
        self.rows
            .iter()
            .filter(move |r| &r.message == message)
            .flat_map(move |row| {
                let range = range.clone();

                row.cells
                    .iter()
                    .filter(move |c| c.range.start < range.end && range.start < c.range.end)
                    .map(|c| Rect::new(c.x, row.y, c.width, 1))
            })
    }
}

/// Selecting text in one message with the left button.
#[derive(Debug, Default, PartialEq)]
pub enum Selection<Id> {
    #[default]
    Idle,
    /// From the grapheme pressed on, the anchor, to the one the drag is at,
    /// the head, and whether the pointer has moved since the press.
    Dragging {
        message: Id,
        anchor: Range<usize>,
        head: Range<usize>,
        moved: bool,
    },
    Selected {
        message: Id,
        range: Range<usize>,
    },
}

impl<Id: Clone + PartialEq> Selection<Id> {
    /// A press, drag or release of the left button, against the last draw.
    /// A selection is a message and a range of its text, so it stays on that
    /// text however the screen changes between events.
    pub fn mouse_event(&mut self, map: &TextMap<Id>, event: &MouseEvent) -> Option<Action> {
        let (x, y) = (event.column, event.row);

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                *self = match map.hit(x, y) {
                    Some((message, range)) => Selection::Dragging {
                        message,
                        anchor: range.clone(),
                        head: range,
                        moved: false,
                    },
                    None => Selection::Idle,
                };
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Selection::Dragging {
                    message,
                    head,
                    moved,
                    ..
                } = self
                {
                    // stays in the message, however far the pointer goes
                    if let Some(range) = map.nearest(message, x, y) {
                        *head = range;
                    }

                    *moved = true;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => return self.release(map),
            _ => {}
        }

        None
    }

    // after a drag, select and copy from the earlier end to the later one,
    // both included; a click selects nothing
    fn release(&mut self, map: &TextMap<Id>) -> Option<Action> {
        let Selection::Dragging { moved, .. } = self else {
            return None;
        };

        if !*moved {
            self.clear();
            return None;
        }

        let (message, range) = self.highlighted()?;
        let range = map.widen(message, range);
        let message = message.clone();

        let text = map
            .text(&message)
            .and_then(|text| text.get(range.clone()))
            .map(str::to_string);

        *self = Selection::Selected { message, range };
        text.map(Action::Copy)
    }

    pub fn clear(&mut self) {
        *self = Selection::Idle;
    }

    /// The message and the bytes of its text to highlight.
    pub fn highlighted(&self) -> Option<(&Id, Range<usize>)> {
        match self {
            Selection::Idle => None,
            Selection::Dragging {
                message,
                anchor,
                head,
                ..
            } => Some((
                message,
                anchor.start.min(head.start)..anchor.end.max(head.end),
            )),
            Selection::Selected { message, range } => Some((message, range.clone())),
        }
    }
}

/// Reverse the colours of the highlighted text, over what the list drew.
pub fn paint<Id: Clone + PartialEq>(
    buf: &mut Buffer,
    map: &TextMap<Id>,
    selection: &Selection<Id>,
) {
    if let Some((message, range)) = selection.highlighted() {
        for cell in map.cells(message, range) {
            buf.set_style(cell, Modifier::REVERSED);
        }
    }
}
