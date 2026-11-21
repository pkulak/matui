use crate::consumed;
use crate::widgets::EventResult::Ignored;
use crate::widgets::{EventResult, Focusable};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use std::cell::Cell;
use std::collections::HashSet;

struct Autocomplete {
    trigger: char,
    entries: Vec<(String, String)>,
    selected: usize,
}

struct ActiveMatches<'a> {
    start: usize,
    end: usize,
    query: &'a str,
    entries: Vec<(&'a str, &'a str)>,
}

struct Completion<'a> {
    start: usize,
    end: usize,
    ghost: String,
    value: &'a str,
}

pub struct TextInput {
    title: String,
    pub value: String,
    pub focused: bool,
    password: bool,
    cursor: usize,
    autocomplete: Option<Autocomplete>,

    // state that needs to be modified by the widget and the struct
    left: Cell<usize>,
    /// Where the text was drawn last, for clicks.
    area: Cell<Rect>,
}

impl Focusable for &mut TextInput {
    fn focused(&self) -> bool {
        self.focused
    }

    fn focus(&mut self) {
        self.focused = true;
    }

    fn defocus(&mut self) {
        self.focused = false;
    }
}

/// Convert a char index into its corresponding byte offset within `s`.
/// Returns `s.len()` if `char_idx` is past the end.
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

impl TextInput {
    pub fn new(title: String, focused: bool, password: bool) -> TextInput {
        Self {
            title,
            value: String::new(),
            focused,
            password,
            cursor: 0,
            autocomplete: None,
            left: Cell::new(0),
            area: Cell::new(Rect::default()),
        }
    }

    pub fn with_autocomplete(mut self, trigger: char, entries: Vec<(String, String)>) -> Self {
        self.autocomplete = Some(Autocomplete {
            trigger,
            entries,
            selected: 0,
        });
        self
    }

    pub fn widget(&self) -> TextInputWidget<'_> {
        TextInputWidget { textinput: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        if !self.focused {
            return Ignored;
        }

        if input.modifiers != KeyModifiers::SHIFT && input.modifiers != KeyModifiers::NONE {
            return Ignored;
        }

        match input.code {
            KeyCode::Char(c) => {
                self.append_char(c);
                consumed!()
            }
            KeyCode::Backspace => {
                self.backspace();
                consumed!()
            }
            KeyCode::Left => {
                self.move_left();
                consumed!()
            }
            KeyCode::Right => {
                if self.cursor == self.value.chars().count() && self.accept_completion() {
                    consumed!()
                } else {
                    self.move_right();
                    consumed!()
                }
            }
            KeyCode::Tab => {
                if self.accept_completion() {
                    consumed!()
                } else {
                    Ignored
                }
            }
            KeyCode::Down => {
                if self.cycle_completion(1) {
                    consumed!()
                } else {
                    Ignored
                }
            }
            KeyCode::Up => {
                if self.cycle_completion(-1) {
                    consumed!()
                } else {
                    Ignored
                }
            }
            _ => Ignored,
        }
    }

    pub fn paste_event(&mut self, value: &str) -> EventResult {
        if !self.focused {
            return Ignored;
        }

        let value = value.replace("\r\n", "\n");
        let value: String = value
            .chars()
            .map(|c| if matches!(c, '\r' | '\n') { ' ' } else { c })
            .collect();
        let byte_pos = char_to_byte(&self.value, self.cursor);

        self.value.insert_str(byte_pos, &value);
        self.cursor += value.chars().count();
        self.reset_autocomplete();

        consumed!()
    }

    /// A click on the text moves the cursor there.
    pub fn mouse_event(&mut self, event: &MouseEvent) {
        let area = self.area.get();

        if !self.focused
            || event.kind != MouseEventKind::Down(MouseButton::Left)
            || !area.contains(Position::new(event.column, event.row))
        {
            return;
        }

        let clicked = self.left.get() + usize::from(event.column - area.x);
        self.cursor = clicked.min(self.value.chars().count());
        self.reset_autocomplete();
    }

    pub fn value(&self) -> String {
        self.value.clone()
    }

    fn append_char(&mut self, ch: char) {
        let byte_pos = char_to_byte(&self.value, self.cursor);
        self.value.insert(byte_pos, ch);
        self.cursor += 1;
        self.reset_autocomplete();
    }

    fn move_right(&mut self) {
        if self.cursor < self.value.chars().count() {
            self.cursor += 1;
        }
        self.reset_autocomplete();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 || self.value.is_empty() {
            return;
        }

        let end_byte = char_to_byte(&self.value, self.cursor);
        let start_byte = char_to_byte(&self.value, self.cursor - 1);
        self.value.replace_range(start_byte..end_byte, "");
        self.cursor -= 1;
        self.reset_autocomplete();

        let left = self.left.get();

        if left > 0 {
            self.left.replace(left - 1);
        }
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
        self.reset_autocomplete();

        let left = self.left.get();

        if self.cursor < left {
            self.left.replace(self.cursor);
        }
    }

    fn reset_autocomplete(&mut self) {
        if let Some(ac) = &mut self.autocomplete {
            ac.selected = 0;
        }
    }

    fn active_matches(&self) -> Option<ActiveMatches<'_>> {
        if !self.focused || self.password {
            return None;
        }

        let ac = self.autocomplete.as_ref()?;
        let cursor_byte = char_to_byte(&self.value, self.cursor);

        // only complete at the end of a token
        if self.value[cursor_byte..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || ":_".contains(c))
        {
            return None;
        }

        let before = &self.value[..cursor_byte];
        let trigger_byte = before.rfind(ac.trigger)?;

        // trigger must start a token (same boundary rule as mentions.rs)
        if before[..trigger_byte]
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || "._=/+-".contains(c) || c == ac.trigger)
        {
            return None;
        }

        let query = &before[trigger_byte + ac.trigger.len_utf8()..];
        let query_lower = query.to_lowercase();

        let mut seen = HashSet::new();
        let entries: Vec<(&str, &str)> = ac
            .entries
            .iter()
            .filter(|(label, _)| label.to_lowercase().starts_with(&query_lower))
            .filter(|(_, value)| seen.insert(value.as_str()))
            .map(|(label, value)| (label.as_str(), value.as_str()))
            .collect();

        if entries.is_empty() {
            return None;
        }

        Some(ActiveMatches {
            start: trigger_byte,
            end: cursor_byte,
            query,
            entries,
        })
    }

    fn active_completion(&self) -> Option<Completion<'_>> {
        let ac = self.autocomplete.as_ref()?;
        let matches = self.active_matches()?;
        let (label, value) = matches.entries[ac.selected % matches.entries.len()];

        let suffix = &label[char_to_byte(label, matches.query.chars().count())..];
        let ghost = if suffix.is_empty() {
            format!(" → {value}")
        } else {
            suffix.to_string()
        };

        Some(Completion {
            start: matches.start,
            end: matches.end,
            ghost,
            value,
        })
    }

    fn accept_completion(&mut self) -> bool {
        let Some(completion) = self.active_completion() else {
            return false;
        };

        let start = completion.start;
        let end = completion.end;
        let mut replacement = completion.value.to_string();

        let add_space = self.value[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_whitespace() && !c.is_ascii_punctuation());

        if add_space {
            replacement.push(' ');
        }

        let start_chars = self.value[..start].chars().count();
        self.value.replace_range(start..end, &replacement);
        self.cursor = start_chars + replacement.chars().count();

        if self.cursor < self.left.get() {
            self.left.replace(self.cursor);
        }

        self.reset_autocomplete();
        true
    }

    fn cycle_completion(&mut self, delta: isize) -> bool {
        let Some(matches) = self.active_matches() else {
            return false;
        };

        let len = matches.entries.len();
        let ac = self.autocomplete.as_mut().unwrap();
        let cur = ac.selected % len;
        ac.selected = (cur as isize + delta).rem_euclid(len as isize) as usize;
        true
    }

    fn display_value(&self) -> String {
        if self.password {
            "*".repeat(self.value.chars().count())
        } else {
            self.value.clone()
        }
    }
}

pub struct TextInputWidget<'a> {
    textinput: &'a TextInput,
}

impl TextInputWidget<'_> {
    fn set_left(&self, left: usize) {
        self.textinput.left.replace(left);
    }

    fn adjust_window(&self, size: usize) {
        let left = self.textinput.left.get();
        let char_count = self.textinput.value.chars().count();

        // we fit entirely
        if char_count <= size {
            self.set_left(0);
            return;
        }

        // scroll left
        if self.textinput.cursor >= left + size {
            self.set_left(self.textinput.cursor - size + 1);
            return;
        }

        // scroll right
        if left >= char_count - size {
            self.set_left(char_count - size + 1);
        }
    }

    fn line(&self) -> Line<'static> {
        let left = self.textinput.left.get();
        let value = self.textinput.display_value();
        let visible = &value[char_to_byte(&value, left)..];

        if !self.textinput.focused || self.textinput.cursor < left {
            return Line::from(visible.to_string());
        }

        let cursor_byte = char_to_byte(visible, self.textinput.cursor - left);
        let cursor_style = Style::default().add_modifier(Modifier::REVERSED);

        if let Some(completion) = self.textinput.active_completion() {
            let ghost_style = Style::default().fg(Color::DarkGray);
            let mut chars = completion.ghost.chars();
            let first = chars
                .next()
                .map(|c| c.to_string())
                .unwrap_or_else(|| " ".to_string());
            let rest: String = chars.collect();

            return Line::from(vec![
                Span::raw(visible[..cursor_byte].to_string()),
                Span::styled(first, ghost_style.add_modifier(Modifier::REVERSED)),
                Span::styled(rest, ghost_style),
                Span::raw(visible[cursor_byte..].to_string()),
            ]);
        }

        if let Some(ch) = visible[cursor_byte..].chars().next() {
            let next_byte = cursor_byte + ch.len_utf8();
            Line::from(vec![
                Span::raw(visible[..cursor_byte].to_string()),
                Span::styled(visible[cursor_byte..next_byte].to_string(), cursor_style),
                Span::raw(visible[next_byte..].to_string()),
            ])
        } else {
            Line::from(vec![
                Span::raw(visible.to_string()),
                Span::styled(" ", cursor_style),
            ])
        }
    }
}

impl Widget for TextInputWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let color = if self.textinput.focused {
            Color::LightGreen
        } else {
            Color::DarkGray
        };

        Block::default()
            .title(self.textinput.title.as_str())
            .borders(Borders::ALL)
            .style(Style::default().fg(color))
            .render(area, buf);

        let area = Layout::default()
            .horizontal_margin(1)
            .vertical_margin(1)
            .constraints([Constraint::Percentage(100)])
            .split(area)[0];

        self.textinput.area.set(area);
        self.adjust_window(area.width as usize);

        Paragraph::new(self.line())
            .style(Style::default().fg(color))
            .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};
    use ratatui::widgets::Widget;

    use crate::selection::tests::press;
    use crate::widgets::EventResult;
    use crate::widgets::textinput::TextInput;

    #[test]
    fn it_accepts_input() {
        let mut input = TextInput::new("Test".to_string(), true, false);

        // type out a string
        for c in "Hello World".chars() {
            input.key_event(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        assert_eq!(input.value(), "Hello World");

        // edit it
        for _ in 0..6 {
            input.key_event(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }

        for _ in 0..5 {
            input.key_event(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }

        for c in "Goodbye".chars() {
            input.key_event(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        assert_eq!(input.value(), "Goodbye World");
    }

    #[test]
    fn it_pastes_at_the_cursor() {
        let mut input = TextInput::new("Test".to_string(), true, false);

        for c in "HelloWorld".chars() {
            input.key_event(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        for _ in 0..5 {
            input.key_event(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }

        input.paste_event(" brave\r\nnew ");

        assert_eq!(input.value(), "Hello brave new World");
    }

    #[test]
    fn it_moves_the_cursor_to_a_click() {
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        let mut input = TextInput::new("Test".to_string(), true, false);

        for c in "Hello".chars() {
            input.key_event(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        input.widget().render(area, &mut buf);

        // on the second letter
        input.mouse_event(&press((2, 1)));
        input.key_event(&KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE));
        assert_eq!(input.value(), "HXello");

        // past the end, and on the border, which does nothing
        input.mouse_event(&press((8, 1)));
        input.mouse_event(&press((1, 0)));
        input.key_event(&KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(input.value(), "HXello!");

        // scrolled, so the first column shows "things."
        for c in " this is me typing some things.".chars() {
            input.key_event(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        input.widget().render(area, &mut buf);
        input.mouse_event(&press((1, 1)));
        input.key_event(&KeyEvent::new(KeyCode::Char('_'), KeyModifiers::NONE));
        assert!(input.value().ends_with("some _things."));
    }

    #[test]
    fn it_renders_correctly() {
        let area = Rect::new(0, 0, 10, 3);
        let mut input = TextInput::new("Test".to_string(), true, false);

        // do an initial render
        let buf = render(&input, area);
        assert_eq!(get_line(&buf, 1), "│        │");
        assert_eq!(reversed_cells(&buf, 1), vec![(1, " ")]);

        // type out a long string
        for c in "Hello, world, this is me typing some things.".chars() {
            input.key_event(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        let buf = render(&input, area);
        assert_eq!(get_line(&buf, 1), "│things. │");
        assert_eq!(reversed_cells(&buf, 1), vec![(8, " ")]);

        // arrow backwards a bit
        for _ in 0..3 {
            input.key_event(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }

        let buf = render(&input, area);
        assert_eq!(get_line(&buf, 1), "│things. │");
        assert_eq!(reversed_cells(&buf, 1), vec![(5, "g")]);

        // delete
        input.key_event(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        let buf = render(&input, area);
        assert_eq!(get_line(&buf, 1), "│ thigs. │");
        assert_eq!(reversed_cells(&buf, 1), vec![(5, "g")]);

        // resize larger
        let area = Rect::new(0, 0, 20, 3);
        let buf = render(&input, area);
        assert_eq!(get_line(&buf, 1), "│yping some thigs. │");
        assert_eq!(reversed_cells(&buf, 1), vec![(15, "g")]);

        // unfocused hides the cursor
        input.focused = false;
        let buf = render(&input, area);
        assert_eq!(get_line(&buf, 1), "│yping some thigs. │");
        assert!(reversed_cells(&buf, 1).is_empty());
    }

    #[test]
    fn it_handles_unicode() {
        let mut input = TextInput::new("Test".to_string(), true, false);

        // Type one character of each UTF-8 byte width:
        //   'A'  = 1 byte  (U+0041)
        //   'é'  = 2 bytes (U+00E9)
        //   '中' = 3 bytes (U+4E2D)
        //   '😀' = 4 bytes (U+1F600)
        for c in "Aé中😀".chars() {
            input.key_event(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(input.value(), "Aé中😀");

        // Backspace removes the 4-byte emoji
        input.key_event(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.value(), "Aé中");

        // Move left past '中', then backspace removes 'é'
        input.key_event(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        input.key_event(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.value(), "A中");

        // Insert '😀' before '中'
        input.key_event(&KeyEvent::new(KeyCode::Char('😀'), KeyModifiers::NONE));
        assert_eq!(input.value(), "A😀中");

        // Move right past '中', then backspace removes '中'
        input.key_event(&KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        input.key_event(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.value(), "A😀");

        // Render should not panic
        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);
        input.widget().render(area, &mut buf);
    }

    #[test]
    fn it_autocompletes_and_renders_ghost_text() {
        let area = Rect::new(0, 0, 26, 3);
        let mut input = TextInput::new("Test".to_string(), true, false).with_autocomplete(
            '@',
            vec![
                ("Greg".to_string(), "@greg:kulak.us".to_string()),
                ("Gwen".to_string(), "@gwen:kulak.us".to_string()),
                ("Phil Kulak".to_string(), "@pkulak:kulak.us".to_string()),
                ("pkulak".to_string(), "@pkulak:kulak.us".to_string()),
            ],
        );

        // bare '@' suggests the first entry immediately
        type_str(&mut input, "@");
        let buf = render(&input, area);
        assert_eq!(get_line(&buf, 1), "│@Greg                   │");
        assert_eq!(reversed_cells(&buf, 1), vec![(2, "G")]);
        assert_eq!(buf[(2, 1)].fg, Color::DarkGray);
        assert_eq!(buf[(3, 1)].fg, Color::DarkGray);

        // typing narrows it down, keeping user's typed casing
        type_str(&mut input, "gw");
        let buf = render(&input, area);
        assert_eq!(get_line(&buf, 1), "│@gwen                   │");
        assert_eq!(reversed_cells(&buf, 1), vec![(4, "e")]);
        assert_eq!(buf[(4, 1)].fg, Color::DarkGray);
        assert_eq!(buf[(5, 1)].fg, Color::DarkGray);

        // finishing the full label shows the target value hint
        type_str(&mut input, "en");
        let buf = render(&input, area);
        assert_eq!(get_line(&buf, 1), "│@gwen → @gwen:kulak.us  │");
        assert_eq!(reversed_cells(&buf, 1), vec![(6, " ")]);
        assert_eq!(buf[(6, 1)].fg, Color::DarkGray);
        assert_eq!(buf[(7, 1)].fg, Color::DarkGray);

        // Right at end-of-line accepts and adds a trailing space
        input.key_event(&KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(input.value(), "@gwen:kulak.us ");

        // multi-word labels match across spaces; Tab accepts
        type_str(&mut input, "@phil k");
        let buf = render(&input, Rect::new(0, 0, 40, 3));
        assert!(get_line(&buf, 1).contains("@phil kulak"));
        input.key_event(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(input.value(), "@gwen:kulak.us @pkulak:kulak.us ");
    }

    #[test]
    fn it_cycles_and_deduplicates_completions() {
        let area = Rect::new(0, 0, 20, 3);
        let mut input = TextInput::new("Test".to_string(), true, false).with_autocomplete(
            '@',
            vec![
                ("Greg".to_string(), "@greg:kulak.us".to_string()),
                ("Gwen".to_string(), "@gwen:kulak.us".to_string()),
                ("Phil Kulak".to_string(), "@pkulak:kulak.us".to_string()),
                ("pkulak".to_string(), "@pkulak:kulak.us".to_string()),
            ],
        );

        // bare '@' deduplicates Phil Kulak / pkulak so cycling visits 3 unique users
        type_str(&mut input, "@");
        assert_eq!(get_line(&render(&input, area), 1), "│@Greg             │");

        input.key_event(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(get_line(&render(&input, area), 1), "│@Gwen             │");

        input.key_event(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(get_line(&render(&input, area), 1), "│@Phil Kulak       │");

        // wraps back to Greg (does not show pkulak separately)
        input.key_event(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(get_line(&render(&input, area), 1), "│@Greg             │");

        // Up wraps backwards
        input.key_event(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(get_line(&render(&input, area), 1), "│@Phil Kulak       │");

        // typing the localpart still finds pkulak
        type_str(&mut input, "pk");
        assert_eq!(get_line(&render(&input, area), 1), "│@pkulak           │");
        input.key_event(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(input.value(), "@pkulak:kulak.us ");
    }

    #[test]
    fn it_completes_mid_line_and_respects_boundaries() {
        let area = Rect::new(0, 0, 25, 3);
        let mut input = TextInput::new("Test".to_string(), true, false).with_autocomplete(
            '@',
            vec![("Gwen".to_string(), "@gwen:kulak.us".to_string())],
        );

        // no suggestion when glued to a word like an email address
        type_str(&mut input, "bob@g");
        assert!(matches!(
            input.key_event(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            EventResult::Ignored
        ));
        assert_eq!(input.value(), "bob@g");

        // mid-line before punctuation: ghost text inserts visually, Right moves cursor, Tab accepts without space
        let mut input = TextInput::new("Test".to_string(), true, false).with_autocomplete(
            '@',
            vec![("Gwen".to_string(), "@gwen:kulak.us".to_string())],
        );
        type_str(&mut input, "hey (@g), hi");
        for _ in 0..5 {
            input.key_event(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }

        let buf = render(&input, area);
        assert_eq!(get_line(&buf, 1), "│hey (@gwen), hi        │");
        assert_eq!(reversed_cells(&buf, 1), vec![(8, "w")]);

        // Right mid-line moves onto ')' instead of accepting
        input.key_event(&KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(input.value(), "hey (@g), hi");

        // Move back before ')' and accept with Tab (no trailing space before punctuation)
        input.key_event(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        input.key_event(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(input.value(), "hey (@gwen:kulak.us), hi");

        // Cursor before ':' inside an already-completed ID does not trigger
        for _ in 0..14 {
            input.key_event(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }
        assert!(matches!(
            input.key_event(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            EventResult::Ignored
        ));
    }

    fn type_str(input: &mut TextInput, s: &str) {
        for c in s.chars() {
            input.key_event(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    fn render(input: &TextInput, area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        input.widget().render(area, &mut buf);
        buf
    }

    fn get_line(buf: &Buffer, line: usize) -> String {
        let width = buf.area.width as usize;

        buf.content()[(line * width)..((line + 1) * width)]
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn reversed_cells(buf: &Buffer, line: usize) -> Vec<(usize, &str)> {
        let width = buf.area.width as usize;

        buf.content()[(line * width)..((line + 1) * width)]
            .iter()
            .enumerate()
            .filter(|(_, c)| c.modifier.contains(Modifier::REVERSED))
            .map(|(x, c)| (x, c.symbol()))
            .collect()
    }
}
