use crate::close;
use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Widget, Table, Row};

use crate::widgets::get_margin;

use super::EventResult;

pub struct Help;

impl Help {
    pub fn new() -> Self {
        Self
    }

    pub fn widget(&self) -> HelpWidget {
        HelpWidget
    }

    pub fn key_event(&mut self, _: &KeyEvent) -> EventResult {
        // no matter what, close
        close!()
    }
}

pub struct HelpWidget;

impl Widget for HelpWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .direction(Direction::Horizontal)
            .vertical_margin(get_margin(area.height, 20))
            .horizontal_margin(get_margin(area.width, 70))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        // Render the main block
        let block = Block::default()
            .title("Help")
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(Color::Black))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        block.render(area, buf);

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .vertical_margin(2)
            .horizontal_margin(2)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area);

        let area = Layout::default()
            .horizontal_margin(1)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(splits[0])[0];

        Table::new(vec![
              Row::new(vec!["Space", "Show the room switcher"]),
              Row::new(vec!["j*", "Select one line down."]),
              Row::new(vec!["k*", "Select one line up."]),
              Row::new(vec!["i", "Create a new message using the external editor."]),
              Row::new(vec!["Enter", "Open the selected message (images, videos, urls, etc)."]),
              Row::new(vec!["s", "Save the selected message (images and videos)."]),
              Row::new(vec!["c", "Edit the selected message in the external editor."]),
              Row::new(vec!["r", "React to the selected message."]),
              Row::new(vec!["R", "Reply to the selected message."]),
              Row::new(vec!["v", "View the selected message in the external editor."]),
              Row::new(vec!["V", "View the current room in the external editor."]),
              Row::new(vec!["u", "Upload a file."]),
              Row::new(vec!["?", "Show this helper."]),
              Row::new(vec!["", "* arrow keys are fine too."]),
          ])
          .style(Style::default().fg(Color::White))
          .header(
              Row::new(vec!["Key", "Description"])
                  .style(Style::default().fg(Color::Green))
                  .bottom_margin(1)
          )
          .widths(&[Constraint::Length(6), Constraint::Percentage(90)])
          .column_spacing(1)
          .render(area, buf)
    }
}
