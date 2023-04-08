use crate::widgets::Action::Typing;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::{EventResult, Focusable, KeyEventing};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::cell::Cell;
use tui::buffer::Buffer;
use tui::layout::{Constraint, Layout, Rect};
use tui::style::{Color, Style};
use tui::widgets::{Block, Borders, Paragraph, Widget};

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

impl KeyEventing for &mut TextInput {
    fn input(&mut self, input: &KeyEvent) -> EventResult {
        if !self.focused {
            return Ignored;
        }

        if input.modifiers != KeyModifiers::SHIFT && input.modifiers != KeyModifiers::NONE {
            return Ignored;
        }

        match input.code {
            KeyCode::Char(c) => {
                self.append_char(c);
                Consumed(Typing)
            }
            KeyCode::Backspace => {
                self.backspace();
                Consumed(Typing)
            }
            KeyCode::Left => {
                self.move_left();
                Consumed(Typing)
            }
            KeyCode::Right => {
                self.move_right();
                Consumed(Typing)
            }
            _ => Ignored,
        }
    }
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

    pub fn widget(&self) -> TextInputWidget {
        TextInputWidget {
            textinput: self,
            left: &self.left,
        }
    }

    pub fn value(&self) -> String {
        self.value.clone()
    }

    fn append_char(&mut self, ch: char) {
        if self.cursor == self.value.len() {
            self.value.push(ch);
        } else {
            self.value.insert(self.cursor, ch);
        }

        self.cursor += 1;
    }

    fn move_right(&mut self) {
        if self.cursor < self.value.len() {
            self.cursor += 1;
        }
    }

    fn backspace(&mut self) {
        if self.cursor == 0 || self.value.is_empty() {
            return;
        }

        self.value.replace_range(self.cursor - 1..self.cursor, "");
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
        let mut value = if self.password {
            "*".repeat(self.value.len())
        } else {
            self.value.clone()
        };

        if self.focused {
            if self.cursor >= self.value.len() {
                value.push('█');
            } else {
                value.replace_range(self.cursor..self.cursor + 1, "█");
            }
        }

        value
    }
}

pub struct TextInputWidget<'a> {
    textinput: &'a TextInput,
    left: &'a Cell<usize>,
}

impl TextInputWidget<'_> {
    fn set_left(&self, left: usize) {
        self.left.replace(left);
    }

    fn adjust_window(&self, size: usize) {
        let left = self.left.get();

        // we fit entirely
        if self.textinput.value.len() <= size {
            self.set_left(0);
            return;
        }

        // scroll left
        if self.textinput.cursor >= left + size {
            self.set_left(self.textinput.cursor - size + 1);
            return;
        }

        // scroll right
        if left >= self.textinput.value.len() - size {
            self.set_left(self.textinput.value.len() - size + 1);
        }
    }

    fn adjusted_value(&self) -> String {
        let left = self.left.get();
        let value = self.textinput.display_value();

        if left == 0 {
            return value;
        }

        value[left..].to_string()
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
    use tui::buffer::Buffer;
    use tui::layout::Rect;
    use tui::widgets::Widget;

    use crate::widgets::textinput::TextInput;
    use crate::widgets::KeyEventing;

    #[test]
    fn it_accepts_input() {
        let mut input = &mut TextInput::new("Test".to_string(), true, false);

        // type out a string
        for c in "Hello World".chars() {
            input.input(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        assert_eq!(input.value(), "Hello World");

        // edit it
        for _ in 0..6 {
            input.input(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }

        for _ in 0..5 {
            input.input(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }

        for c in "Goodbye".chars() {
            input.input(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        assert_eq!(input.value(), "Goodbye World");
    }

    #[test]
    fn it_renders_correctly() {
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);

        let mut input = &mut TextInput::new("Test".to_string(), true, false);

        // do an initial render
        input.widget().render(area, &mut buf);

        assert_eq!(get_line(&buf, 1), "│█       │");

        // type out a long string
        for c in "Hello, world, this is me typing some things.".chars() {
            input.input(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        input.widget().render(area, &mut buf);
        assert_eq!(get_line(&buf, 1), "│things.█│");

        // arrow backwards a bit
        for _ in 0..3 {
            input.input(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }

        let mut buf = Buffer::empty(area);
        input.widget().render(area, &mut buf);
        assert_eq!(get_line(&buf, 1), "│thin█s. │");

        // delete
        input.input(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        input.widget().render(area, &mut buf);
        assert_eq!(get_line(&buf, 1), "│ thi█s. │");

        // resize larger
        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);

        input.widget().render(area, &mut buf);
        assert_eq!(get_line(&buf, 1), "│yping some thi█s. │");
    }

    fn get_line(buf: &Buffer, line: usize) -> String {
        let width = buf.area.width as usize;

        buf.content()[(line * width)..((line + 1) * width)]
            .into_iter()
            .map(|c| c.symbol.clone())
            .collect()
    }
}
