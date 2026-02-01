use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

use crate::consumed;
use crate::widgets::button::Button;
use crate::widgets::textinput::TextInput;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::{focus_next, focus_prev, get_margin, EventResult, Focusable};

pub struct Signin {
    pub id: TextInput,
    pub password: TextInput,
    submit: Button,
}

impl Default for Signin {
    fn default() -> Self {
        let id = TextInput::new("Matrix ID".to_string(), true, false);
        let password = TextInput::new("Password".to_string(), false, true);

        let submit = Button::new("Submit".to_string(), false);

        Self {
            id,
            password,
            submit,
        }
    }
}

impl Signin {
    fn focus_order(&mut self) -> Vec<Box<dyn Focusable + '_>> {
        vec![
            Box::new(&mut self.id),
            Box::new(&mut self.password),
            Box::new(&mut self.submit),
        ]
    }

    pub fn widget(&self) -> SigninWidget<'_> {
        SigninWidget { signin: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        if let Consumed(_) = self.id.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.password.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.submit.key_event(input) {
            let id = self.id.value();
            let password = self.password.value();

            return EventResult::Consumed(Box::new(move |app| {
                app.matrix.login(id.as_str(), password.as_str());
                app.close_popup();
            }));
        }

        match input.code {
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => focus_next(self.focus_order()),
            KeyCode::BackTab | KeyCode::Up => focus_prev(self.focus_order()),
            _ => Ignored,
        }
    }
}

pub struct SigninWidget<'a> {
    pub signin: &'a Signin,
}

impl Widget for SigninWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .horizontal_margin(get_margin(area.width, 60))
            .vertical_margin(get_margin(area.height, 18))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(8)
            .vertical_margin(3)
            .constraints(
                [
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Percentage(100),
                ]
                .as_ref(),
            )
            .split(area);

        let block = Block::default()
            .title("Sign In")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Reset));

        block.render(area, buf);
        self.signin.id.widget().render(splits[0], buf);
        self.signin.password.widget().render(splits[2], buf);

        // pop the submit button on the right side
        let area = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(splits[4])[1];

        self.signin.submit.widget().render(area, buf);
    }
}
