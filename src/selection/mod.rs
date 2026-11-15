//! Selecting and copying message text with the mouse, and clicking links and
//! pictures. A message is any ID with the text its body rows show, so nothing
//! here knows about Matrix.

use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use crossterm::execute;
use linkify::{LinkFinder, LinkKind};
use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use std::io::stdout;
use std::ops::Range;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
pub(crate) mod tests;

/// Put the text on the clipboard, with OSC 52. Only the clipboard: libghostty
/// drops a sequence that names more than one destination, such as `cp`.
pub fn copy(text: &str) -> std::io::Result<()> {
    execute!(stdout(), CopyToClipboard::to_clipboard_from(text))
}

/// What a mouse event asks the app to do.
#[derive(Debug, PartialEq)]
pub enum Action<Id> {
    Copy(String),
    Open(String),
    /// Open the message whose picture was clicked.
    OpenPicture(Id),
}

/// Where the text's http and https URLs are.
pub fn links(text: &str) -> Vec<Range<usize>> {
    LinkFinder::new()
        .kinds(&[LinkKind::Url])
        .links(text)
        .filter(|link| {
            link.as_str().split_once("://").is_some_and(|(scheme, _)| {
                scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
            })
        })
        .map(|link| link.start()..link.end())
        .collect()
}

/// Characters that join the words on either side into one for a double
/// click, as in iTerm2 and Pi, so a path or `kebab-case` is one word.
const JOINERS: [&str; 7] = ["/", "-", "+", "\\", "~", "_", "."];

/// The word around the byte at `at`. Words are Unicode's, joined by
/// JOINERS. Off a word, it's the run of spaces or the punctuation there.
fn word(text: &str, at: usize) -> Range<usize> {
    struct Segment {
        range: Range<usize>,
        joiner: bool,
        wordlike: bool,
    }

    let segments: Vec<_> = text
        .split_word_bound_indices()
        .map(|(start, s)| {
            let joiner = JOINERS.contains(&s);

            Segment {
                range: start..start + s.len(),
                joiner,
                wordlike: joiner || s.chars().any(char::is_alphanumeric),
            }
        })
        .collect();

    let joins = |left: &Segment, right: &Segment| {
        left.wordlike && right.wordlike && (left.joiner || right.joiner)
    };

    let Some(clicked) = segments.iter().position(|s| s.range.contains(&at)) else {
        return at..at;
    };

    let mut first = clicked;
    while first > 0 && joins(&segments[first - 1], &segments[first]) {
        first -= 1;
    }

    let mut last = clicked;
    while last + 1 < segments.len() && joins(&segments[last], &segments[last + 1]) {
        last += 1;
    }

    segments[first].range.start..segments[last].range.end
}

/// The line around the byte at `at`, without its line break.
fn line(text: &str, at: usize) -> Range<usize> {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let end = text[at..].find('\n').map_or(text.len(), |i| at + i);

    start..end
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
    /// Where the text's links are, from links().
    pub links: &'a [Range<usize>],
    pub runs: Vec<Run>,
}

impl<'a, Id> BodyRow<'a, Id> {
    /// The row these spans draw, if any of them are slices of the text.
    /// Every span counts towards the columns, so gutters and bullets push
    /// the text along as they do on screen.
    pub fn new(
        message: Id,
        text: &'a str,
        links: &'a [Range<usize>],
        spans: &[Span],
    ) -> Option<Self> {
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
            links,
            runs,
        })
    }
}

/// The body text on screen as of the last draw: which cells show which
/// bytes of which message's text, and where its pictures are.
pub struct TextMap<Id> {
    /// The list's area.
    pub area: Rect,
    rows: Vec<Row<Id>>,
    texts: Vec<Text<Id>>,
    pictures: Vec<(Rect, Id)>,
}

/// A visible message's text, its links, and the bytes of it drawn on any of
/// its rows, on screen or not.
struct Text<Id> {
    message: Id,
    text: String,
    links: Vec<Range<usize>>,
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
            pictures: vec![],
        }
    }

    /// Add the cells a message's picture covers on screen.
    pub fn add_picture(&mut self, area: Rect, message: Id) {
        self.pictures.push((area, message));
    }

    /// The message whose picture is under the pointer, if any.
    pub fn picture(&self, x: u16, y: u16) -> Option<Id> {
        self.pictures
            .iter()
            .find(|(area, _)| area.contains(Position::new(x, y)))
            .map(|(_, message)| message.clone())
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
                links: row.links.to_vec(),
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

    /// The link the byte at `at` of the message's text is in, if any.
    pub fn link(&self, message: &Id, at: usize) -> Option<&str> {
        let text = self.texts.iter().find(|t| &t.message == message)?;
        let link = text.links.iter().find(|l| l.contains(&at))?;
        text.text.get(link.clone())
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

    /// The unit of the message's text around a grapheme of it.
    fn around(&self, message: &Id, grapheme: Range<usize>, unit: Unit) -> Range<usize> {
        match (unit, self.text(message)) {
            (Unit::Word, Some(text)) => word(text, grapheme.start),
            (Unit::Line, Some(text)) => line(text, grapheme.start),
            _ => grapheme,
        }
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

/// How soon a press on the same word has to follow the last to be another
/// click of it.
const MULTI_CLICK: Duration = Duration::from_millis(500);

/// What a press and drag select: graphemes after one click, words after a
/// double click and lines after a triple click.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Unit {
    Grapheme,
    Word,
    Line,
}

/// Selecting text in one message with the left button, or clicking a
/// picture.
#[derive(Debug, PartialEq)]
pub struct Selection<Id> {
    state: State<Id>,
    /// The last press on body text, to count clicks.
    last_press: Option<Press<Id>>,
}

impl<Id> Default for Selection<Id> {
    fn default() -> Self {
        Self {
            state: State::Idle,
            last_press: None,
        }
    }
}

/// When a press on body text was, the word it was on, and how many clicks
/// it made.
#[derive(Debug, PartialEq)]
struct Press<Id> {
    at: Instant,
    message: Id,
    word: Range<usize>,
    clicks: u8,
}

#[derive(Debug, Default, PartialEq)]
enum State<Id> {
    #[default]
    Idle,
    /// Pressed on a message's picture, and not moved since.
    Picture(Id),
    /// From the unit pressed on, the anchor, to the one the drag is at, the
    /// head, and whether the pointer has moved since the press.
    Dragging {
        message: Id,
        anchor: Range<usize>,
        head: Range<usize>,
        moved: bool,
        unit: Unit,
    },
    Selected {
        message: Id,
        range: Range<usize>,
    },
}

impl<Id: Clone + PartialEq> Selection<Id> {
    /// A press, drag or release of the left button, against the last draw.
    /// A selection is a message and a range of its text, so it stays on that
    /// text however the screen changes between events. `now` is when the
    /// event came, to tell a double click from two clicks.
    pub fn mouse_event(
        &mut self,
        map: &TextMap<Id>,
        event: &MouseEvent,
        now: Instant,
    ) -> Option<Action<Id>> {
        let (x, y) = (event.column, event.row);

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.press(map, x, y, now),
            // a drag off a picture isn't a click on it
            MouseEventKind::Drag(MouseButton::Left) if matches!(self.state, State::Picture(_)) => {
                self.clear();
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                // a drag isn't a click, so the next press is a first click
                self.last_press = None;

                if let State::Dragging {
                    message,
                    head,
                    moved,
                    unit,
                    ..
                } = &mut self.state
                {
                    // stays in the message, however far the pointer goes
                    if let Some(grapheme) = map.nearest(message, x, y) {
                        *head = map.around(message, grapheme, *unit);
                    }

                    *moved = true;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => return self.release(map),
            _ => {}
        }

        None
    }

    // a press on the same word as the last, soon after it, is another click
    // of it: the second selects the word, the third its line, and the
    // fourth is a first click again; a drag from there goes a word or line
    // at a time, always keeping the one pressed on
    fn press(&mut self, map: &TextMap<Id>, x: u16, y: u16, now: Instant) {
        let Some((message, grapheme)) = map.hit(x, y) else {
            self.last_press = None;
            self.state = map.picture(x, y).map_or(State::Idle, State::Picture);
            return;
        };

        let word = map.around(&message, grapheme.clone(), Unit::Word);

        let clicks = match &self.last_press {
            Some(last)
                if last.message == message
                    && last.word == word
                    && now.duration_since(last.at) <= MULTI_CLICK =>
            {
                last.clicks % 3 + 1
            }
            _ => 1,
        };

        let unit = match clicks {
            2 => Unit::Word,
            3 => Unit::Line,
            _ => Unit::Grapheme,
        };
        let anchor = map.around(&message, grapheme, unit);

        self.last_press = Some(Press {
            at: now,
            message: message.clone(),
            word,
            clicks,
        });
        self.state = State::Dragging {
            message,
            head: anchor.clone(),
            anchor,
            moved: false,
            unit,
        };
    }

    // after a drag, a double click or a triple click, select and copy from
    // the earlier end to the later one, both included; a single click
    // selects nothing, and opens a link or picture it's on
    fn release(&mut self, map: &TextMap<Id>) -> Option<Action<Id>> {
        if let State::Picture(message) = &self.state {
            let message = message.clone();
            self.state = State::Idle;
            return Some(Action::OpenPicture(message));
        }

        let State::Dragging {
            message,
            anchor,
            moved,
            unit,
            ..
        } = &self.state
        else {
            return None;
        };

        if !*moved && *unit == Unit::Grapheme {
            let link = map.link(message, anchor.start).map(str::to_string);
            self.state = State::Idle;
            return link.map(Action::Open);
        }

        let (message, range) = self.highlighted()?;
        let range = map.widen(message, range);
        let message = message.clone();

        let text = map
            .text(&message)
            .and_then(|text| text.get(range.clone()))
            .map(str::to_string);

        self.state = State::Selected { message, range };
        text.map(Action::Copy)
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The message and the bytes of its text to highlight.
    pub fn highlighted(&self) -> Option<(&Id, Range<usize>)> {
        match &self.state {
            State::Idle | State::Picture(_) => None,
            State::Dragging {
                message,
                anchor,
                head,
                ..
            } => Some((
                message,
                anchor.start.min(head.start)..anchor.end.max(head.end),
            )),
            State::Selected { message, range } => Some((message, range.clone())),
        }
    }
}

/// Underline links and reverse the colours of the highlighted text, over
/// what the list drew.
pub fn paint<Id: Clone + PartialEq>(
    buf: &mut Buffer,
    map: &TextMap<Id>,
    selection: &Selection<Id>,
) {
    for text in &map.texts {
        for link in &text.links {
            for cell in map.cells(&text.message, link.clone()) {
                buf.set_style(cell, Modifier::UNDERLINED);
            }
        }
    }

    if let Some((message, range)) = selection.highlighted() {
        for cell in map.cells(message, range) {
            buf.set_style(cell, Modifier::REVERSED);
        }
    }
}
