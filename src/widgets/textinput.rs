use crate::consumed;
use crate::widgets::EventResult::Ignored;
use crate::widgets::{EventResult, Focusable};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use std::cell::Cell;

pub struct TextInput {
    title: String,
    pub value: String,
    pub focused: bool,
    password: bool,
    cursor: usize,

    // state that needs to be modified by the widget and the struct
    left: Cell<usize>,
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
            left: Cell::new(0),
        }
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
                self.move_right();
                consumed!()
            }
            _ => Ignored,
        }
    }
    pub fn value(&self) -> String {
        self.value.clone()
    }

    fn append_char(&mut self, ch: char) {
        let byte_pos = char_to_byte(&self.value, self.cursor);
        self.value.insert(byte_pos, ch);
        self.cursor += 1;
    }

    fn move_right(&mut self) {
        if self.cursor < self.value.chars().count() {
            self.cursor += 1;
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 || self.value.is_empty() {
            return;
        }

        let end_byte = char_to_byte(&self.value, self.cursor);
        let start_byte = char_to_byte(&self.value, self.cursor - 1);
        self.value.replace_range(start_byte..end_byte, "");
        self.cursor -= 1;

        let left = self.left.get();

        if left > 0 {
            self.left.replace(left - 1);
        }
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }

        let left = self.left.get();

        if self.cursor < left {
            self.left.replace(self.cursor);
        }
    }

    fn display_value(&self) -> String {
        let char_count = self.value.chars().count();
        let mut value = if self.password {
            "*".repeat(char_count)
        } else {
            self.value.clone()
        };

        if self.focused {
            if self.cursor >= char_count {
                value.push('█');
            } else {
                let byte_start = char_to_byte(&value, self.cursor);
                let ch_len = value[byte_start..].chars().next().unwrap().len_utf8();
                value.replace_range(byte_start..byte_start + ch_len, "█");
            }
        }

        value
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

    fn adjusted_value(&self) -> String {
        let left = self.textinput.left.get();
        let value = self.textinput.display_value();

        if left == 0 {
            return value;
        }

        let byte_pos = char_to_byte(&value, left);
        value[byte_pos..].to_string()
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
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        self.adjust_window(area.width as usize);

        Paragraph::new(self.adjusted_value())
            .style(Style::default().fg(color))
            .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

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
    fn it_renders_correctly() {
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);

        let mut input = TextInput::new("Test".to_string(), true, false);

        // do an initial render
        input.widget().render(area, &mut buf);

        assert_eq!(get_line(&buf, 1), "│█       │");

        // type out a long string
        for c in "Hello, world, this is me typing some things.".chars() {
            input.key_event(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        input.widget().render(area, &mut buf);
        assert_eq!(get_line(&buf, 1), "│things.█│");

        // arrow backwards a bit
        for _ in 0..3 {
            input.key_event(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }

        let mut buf = Buffer::empty(area);
        input.widget().render(area, &mut buf);
        assert_eq!(get_line(&buf, 1), "│thin█s. │");

        // delete
        input.key_event(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        input.widget().render(area, &mut buf);
        assert_eq!(get_line(&buf, 1), "│ thi█s. │");

        // resize larger
        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);

        input.widget().render(area, &mut buf);
        assert_eq!(get_line(&buf, 1), "│yping some thi█s. │");
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

    fn get_line(buf: &Buffer, line: usize) -> String {
        let width = buf.area.width as usize;

        buf.content()[(line * width)..((line + 1) * width)]
            .iter()
            .map(|c| c.symbol())
            .collect()
    }
}
