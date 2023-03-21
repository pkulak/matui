use std::sync::mpsc::{channel, Receiver};

use crossterm::event::{KeyCode, KeyEvent};
use tui::buffer::Buffer;
use tui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::widgets::{Block, BorderType, Borders, Widget};

use crate::matrix::Matrix;
use crate::widgets::button::{Button, ButtonEvent};
use crate::widgets::textinput::TextInput;
use crate::widgets::{focus_next, focus_prev, get_margin, send, Focusable, KeyEventing};

pub struct Signin {
    matrix: Matrix,
    id: TextInput,
    password: TextInput,
    submit: Button,
    button_recv: Receiver<ButtonEvent>,
}

impl Signin {
    pub fn new(matrix: Matrix) -> Self {
        let id = TextInput::new("Matrix ID".to_string(), true, false);
        let password = TextInput::new("Password".to_string(), false, true);

        let (button_send, button_recv) = channel();

        let submit = Button::new("Submit".to_string(), false, Some(button_send));

        Self {
            matrix,
            id,
            password,
            submit,
            button_recv,
        }
    }

    pub fn widget(&self) -> SigninWidget {
        SigninWidget { signin: self }
    }

    pub fn input(&mut self, input: &KeyEvent) {
        if send(self.event_order(), input) {
            return;
        }

        match input.code {
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => focus_next(self.focus_order()),
            KeyCode::BackTab | KeyCode::Up => focus_prev(self.focus_order()),
            _ => {}
        };
    }

    pub fn tick(&mut self) {
        // once we get a button click, sign in and show the progress bar
        if self.button_recv.try_recv().is_ok() {
            self.matrix
                .login(self.id.value().as_str(), self.password.value().as_str());
        };
    }

    fn focus_order(&mut self) -> Vec<Box<dyn Focusable + '_>> {
        vec![
            Box::new(&mut self.id),
            Box::new(&mut self.password),
            Box::new(&mut self.submit),
        ]
    }

    fn event_order(&mut self) -> Vec<Box<dyn KeyEventing + '_>> {
        vec![
            Box::new(&mut self.id),
            Box::new(&mut self.password),
            Box::new(&mut self.submit),
        ]
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
            .style(Style::default().bg(Color::Black));

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
