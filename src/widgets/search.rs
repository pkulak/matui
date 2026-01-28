use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use crate::app::App;
use crate::consumed;
use crate::event::Event;
use crate::handler::MatuiEvent;
use crate::widgets::textinput::TextInput;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::{get_margin, EventResult};

pub struct Search {
    input: TextInput,
}

impl Default for Search {
    fn default() -> Self {
        let input = TextInput::new("/".to_string(), true, false);

        Self { input }
    }
}

impl Search {
    pub fn widget(&self) -> SearchWidget {
        SearchWidget { search: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        if let Consumed(_) = self.input.key_event(input) {
            App::get_sender()
                .send(Event::Matui(MatuiEvent::Search(
                    self.input.value.to_lowercase(),
                )))
                .unwrap();

            return consumed!();
        }

        match input.code {
            KeyCode::Esc => {
                App::get_sender()
                    .send(Event::Matui(MatuiEvent::Search("".to_string())))
                    .unwrap();

                EventResult::Consumed(Box::new(move |app| {
                    app.close_popup();
                }))
            }
            KeyCode::Enter => EventResult::Consumed(Box::new(move |app| {
                app.close_popup();
            })),
            _ => Ignored,
        }
    }
}

pub struct SearchWidget<'a> {
    pub search: &'a Search,
}

impl Widget for SearchWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .horizontal_margin(get_margin(area.width, 60))
            .vertical_margin(get_margin(area.height, 10))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(8)
            .vertical_margin(3)
            .constraints(
                [
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Percentage(100),
                ]
                .as_ref(),
            )
            .split(area);

        let block = Block::default()
            .title("Search")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Reset));

        block.render(area, buf);

        self.search.input.widget().render(splits[0], buf);

        Paragraph::new("Esc to cancel, Enter to submit").render(splits[1], buf);
    }
}
