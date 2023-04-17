use crate::app::App;
use crate::event::{Event, EventHandler};
use crate::handler::Batch;
use crate::matrix::matrix::Matrix;
use crate::matrix::roomcache::DecoratedRoom;
use crate::settings::is_muted;
use crate::spawn::{get_file_path, get_text};
use crate::widgets::message::{Message, Reaction, ReactionEvent};
use crate::widgets::react::React;
use crate::widgets::EventResult::Consumed;
use crate::widgets::{get_margin, EventResult};
use crate::{pretty_list, truncate, KeyCombo};
use anyhow::bail;
use crossterm::event::{KeyCode, KeyEvent};
use log::info;
use matrix_sdk::room::{Joined, RoomMember};
use ruma::events::room::member::MembershipState;
use ruma::events::room::message::MessageType::Text;
use ruma::events::AnyTimelineEvent;
use ruma::OwnedEventId;
use sorted_vec::SortedVec;
use std::cell::Cell;
use std::cmp::Ordering;
use std::ops::Deref;
use std::sync::mpsc::{channel, TryRecvError};
use std::thread;
use std::time::Duration;
use tui::buffer::Buffer;
use tui::layout::{Alignment, Constraint, Corner, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::widgets::{
    Block, BorderType, Borders, List, ListItem, ListState, Paragraph, StatefulWidget, Widget,
};

use super::confirm::{Confirm, ConfirmResult};
use super::Action;

pub struct Chat {
    matrix: Matrix,
    pub room: DecoratedRoom,
    events: SortedVec<OrderedEvent>,
    messages: Vec<Message>,
    read_to: Option<OwnedEventId>,
    members: Vec<RoomMember>,
    react: Option<React>,
    list_state: Cell<ListState>,
    next_cursor: Option<String>,
    fetching: Cell<bool>,
    focus: bool,
    delete_combo: KeyCombo,
}

impl Chat {
    pub fn new(matrix: Matrix, room: Joined) -> Self {
        matrix.fetch_messages(room.clone(), None);
        matrix.fetch_room_members(room.clone());

        Self {
            matrix: matrix.clone(),
            room: matrix.wrap_room(&room).unwrap(),
            events: SortedVec::new(),
            messages: vec![],
            read_to: None,
            members: vec![],
            react: None,
            list_state: Cell::new(ListState::default()),
            next_cursor: None,
            fetching: Cell::new(true),
            focus: true,
            delete_combo: KeyCombo::new(vec!['d', 'd']),
        }
    }

    fn muted(&self) -> bool {
        is_muted(self.room.room_id())
    }

    fn set_fully_read(&mut self) {
        if !self.focus {
            return;
        }

        let read_to = self.messages.first().map(|m| m.id.clone());

        if read_to == self.read_to {
            return;
        }

        if let Some(id) = read_to.clone() {
            self.matrix.read_to(self.room(), id);
            self.read_to = read_to;
        }
    }

    fn display_full(&self) -> String {
        let mut ret = format!("{} ({})\n\n", self.room.name, self.room.room_id());

        ret.push_str("# Members\n\n");

        for m in &self.members {
            ret.push_str(&format!(
                "* {} ({})\n",
                m.display_name().unwrap_or(m.user_id().as_str()),
                m.user_id()
            ));
        }

        ret
    }

    pub fn room(&self) -> Joined {
        self.room.inner()
    }

    fn pretty_members(&self) -> String {
        let mut names: Vec<&str> = self
            .members
            .iter()
            .filter(|m| m.membership() == &MembershipState::Join)
            .map(|m| {
                m.display_name()
                    .or_else(|| Some(m.user_id().localpart()))
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
            })
            .collect();

        names.sort();
        names.dedup();

        pretty_list(names.into_iter().take(10).collect())
    }

    pub fn input(
        &mut self,
        handler: &EventHandler,
        input: &KeyEvent,
    ) -> anyhow::Result<EventResult> {
        // give our reaction window first dibs
        if let Some(react) = &mut self.react {
            match react.input(input) {
                Consumed(Action::Exit) => {
                    self.react = None;
                    return Ok(Consumed(Action::Typing));
                }
                Consumed(Action::SelectReaction(reaction)) => {
                    self.react = None;

                    if let Some(message) = self.selected_message() {
                        self.matrix
                            .send_reaction(self.room(), message.id.clone(), reaction)
                    }
                    return Ok(Consumed(Action::Typing));
                }
                Consumed(Action::RemoveReaction(reaction)) => {
                    self.react = None;

                    if let Some(event) = self.my_selected_reaction_event(reaction) {
                        self.matrix.redact_event(self.room(), event.id)
                    }
                }
                Consumed(_) => return Ok(Consumed(Action::Typing)),
                _ => {}
            }
        }

        // then look for key combos
        if let KeyCode::Char(c) = input.code {
            if self.delete_combo.record(c) {
                let message = match self.selected_message() {
                    Some(m) => m,
                    None => return Ok(EventResult::Ignored),
                };

                let preview = truncate(message.display().to_string(), 16);
                let warning = format!("Are you sure you want to delete \"{}\"", preview);

                let confirm = Confirm::new(
                    "Delete Message".to_string(),
                    warning,
                    "Yes".to_string(),
                    ConfirmResult::RedactEvent(self.room(), message.id.clone()),
                    "No".to_string(),
                    ConfirmResult::Cancel,
                );

                return Ok(Consumed(Action::ShowConfirmation(confirm)));
            }
        }

        match input.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.previous();
                Ok(Consumed(Action::Typing))
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.next();
                self.try_fetch_previous();
                Ok(Consumed(Action::Typing))
            }
            KeyCode::Enter => {
                if let Some(message) = &self.selected_message() {
                    message.open(self.matrix.clone())
                }
                Ok(Consumed(Action::Typing))
            }
            KeyCode::Char('c') => {
                let message = match self.selected_message() {
                    Some(m) => m,
                    None => return Ok(EventResult::Ignored),
                };

                if matches!(message.body, Text(_)) {
                    handler.park();
                    let result = get_text(Some(message.display()));
                    handler.unpark();

                    // make sure we redraw the whole app when we come back
                    App::get_sender().send(Event::Redraw)?;

                    if let Ok(edit) = result {
                        if let Some(edit) = edit {
                            self.matrix
                                .replace_event(self.room(), message.id.clone(), edit);

                            return Ok(Consumed(Action::Typing));
                        } else {
                            bail!("Ignoring blank message.")
                        }
                    } else {
                        bail!("Couldn't read from editor.")
                    }
                }

                Ok(Consumed(Action::Typing))
            }
            KeyCode::Char('i') => {
                // start a thread to hammer out typing notifications
                let matrix = self.matrix.clone();
                let room = self.room();
                let (send, recv) = channel();

                thread::spawn(move || {
                    while let Err(TryRecvError::Empty) = recv.try_recv() {
                        matrix.typing_notification(room.clone(), true);
                        thread::sleep(Duration::from_millis(1000));
                    }
                });

                handler.park();
                let result = get_text(None);
                handler.unpark();

                // stop typing
                send.send(())?;
                self.matrix.typing_notification(self.room(), false);

                App::get_sender().send(Event::Redraw)?;

                if let Ok(input) = result {
                    if let Some(input) = input {
                        self.matrix.send_text_message(self.room(), input);
                        Ok(Consumed(Action::Typing))
                    } else {
                        bail!("Ignoring blank message.")
                    }
                } else {
                    bail!("Couldn't read from editor.")
                }
            }
            KeyCode::Char('v') => {
                let message = match self.selected_message() {
                    Some(m) => m,
                    None => return Ok(EventResult::Ignored),
                };

                handler.park();
                get_text(Some(&message.display_full()))?;
                handler.unpark();

                App::get_sender().send(Event::Redraw)?;
                Ok(EventResult::Consumed(Action::Typing))
            }
            KeyCode::Char('V') => {
                handler.park();
                get_text(Some(&self.display_full()))?;
                handler.unpark();

                App::get_sender().send(Event::Redraw)?;
                Ok(EventResult::Consumed(Action::Typing))
            }
            KeyCode::Char('r') => {
                self.react = Some(React::new(
                    self.selected_reactions()
                        .into_iter()
                        .map(|r| r.body)
                        .collect(),
                    self.my_selected_reactions()
                        .into_iter()
                        .map(|r| r.body)
                        .collect(),
                ));
                Ok(Consumed(Action::Typing))
            }
            KeyCode::Char('u') => {
                let path = get_file_path()?;

                App::get_sender().send(Event::Redraw)?;

                if path.is_none() {
                    return Ok(EventResult::Ignored);
                }

                self.matrix.send_attachement(self.room(), path.unwrap());

                Ok(Consumed(Action::Typing))
            }
            _ => Ok(EventResult::Ignored),
        }
    }

    pub fn focus_event(&mut self) {
        self.focus = true;
        self.set_fully_read();
    }

    pub fn blur_event(&mut self) {
        self.focus = false;
    }

    pub fn timeline_event(&mut self, event: AnyTimelineEvent) {
        if event.room_id() != self.room.room_id() {
            return;
        }

        self.events.push(OrderedEvent::new(event));
        self.messages = make_message_list(&self.events, &self.members);
        self.set_fully_read();
    }

    pub fn batch_event(&mut self, batch: Batch) {
        if batch.room.room_id() != self.room.room_id() {
            return;
        }

        self.next_cursor = batch.cursor;

        for event in batch.events {
            self.events.push(OrderedEvent::new(event));
        }

        let reset = self.messages.is_empty();

        self.messages = make_message_list(&self.events, &self.members);
        self.fetching.set(false);
        self.set_fully_read();

        if reset {
            let mut state = self.list_state.take();
            state.select(Some(0));
            self.list_state.set(state);
        }

        self.try_fetch_previous();
    }

    pub fn room_members_event(&mut self, room: Joined, members: Vec<RoomMember>) {
        if self.room.room_id() != room.room_id() {
            return;
        }

        self.members = members;
        self.messages = make_message_list(&self.events, &self.members);
    }

    fn try_fetch_previous(&self) {
        if self.next_cursor.is_none() || self.fetching.get() {
            return;
        }

        let state = self.list_state.take();
        let buffer = self.messages.len() - state.selected().unwrap_or_default();
        self.list_state.set(state);

        if buffer < 25 {
            self.matrix
                .fetch_messages(self.room(), self.next_cursor.clone());
            self.fetching.set(true);
            info!("fetching more events...")
        }
    }

    fn next(&self) {
        let mut state = self.list_state.take();

        let i = match state.selected() {
            Some(i) => {
                if i >= &self.messages.len() - 1 {
                    &self.messages.len() - 1
                } else {
                    i + 1
                }
            }
            None => 0,
        };

        state.select(Some(i));
        self.list_state.set(state);
    }

    fn previous(&self) {
        let mut state = self.list_state.take();

        let i = match state.selected() {
            Some(i) => {
                if i == 0 {
                    0
                } else {
                    i - 1
                }
            }
            None => 0,
        };

        state.select(Some(i));
        self.list_state.set(state);
    }

    // the message currently selected by the UI
    fn selected_message(&self) -> Option<&Message> {
        if self.messages.is_empty() {
            return None;
        }

        let state = self.list_state.take();
        let selected = state.selected().unwrap_or_default();
        self.list_state.set(state);

        self.messages.get(selected)
    }

    // the reactions on the currently selected message
    fn selected_reactions(&self) -> Vec<Reaction> {
        match self.selected_message() {
            Some(message) => message.reactions.clone(),
            None => vec![],
        }
    }

    // the reactions belonging to the current user on the selected message
    fn my_selected_reactions(&self) -> Vec<Reaction> {
        let me = self.matrix.me();
        let mut ret = self.selected_reactions();

        ret.retain(|r| {
            for e in &r.events {
                if e.sender_id == me {
                    return true;
                }
            }
            false
        });

        ret
    }

    // the exact reaction event on the selected message
    fn my_selected_reaction_event(&self, body: String) -> Option<ReactionEvent> {
        let me = self.matrix.me();

        for reaction in self.selected_reactions() {
            if reaction.body != body {
                break;
            }

            for event in reaction.events {
                if event.sender_id == me {
                    return Some(event);
                }
            }
        }

        None
    }

    pub fn widget(&self) -> ChatWidget {
        ChatWidget { chat: self }
    }
}

// a good PR would be to add Ord to AnyTimelineEvent
pub struct OrderedEvent {
    inner: AnyTimelineEvent,
}

impl OrderedEvent {
    pub fn new(inner: AnyTimelineEvent) -> OrderedEvent {
        OrderedEvent { inner }
    }
}

impl Deref for OrderedEvent {
    type Target = AnyTimelineEvent;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Ord for OrderedEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        self.origin_server_ts().cmp(&other.origin_server_ts())
    }
}

impl PartialOrd for OrderedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.origin_server_ts()
            .partial_cmp(&other.origin_server_ts())
    }
}

impl PartialEq for OrderedEvent {
    fn eq(&self, other: &Self) -> bool {
        self.event_id().eq(other.event_id())
    }
}

impl Eq for OrderedEvent {}

pub struct ChatWidget<'a> {
    pub chat: &'a Chat,
}

impl Widget for ChatWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 12 {
            return;
        }

        buf.set_style(area, Style::default().bg(Color::Black));

        let area = Layout::default()
            .direction(Direction::Horizontal)
            .horizontal_margin(get_margin(area.width, 80))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Percentage(100)].as_ref())
            .split(area);

        let mut header_text = self.chat.room.name.to_string();

        if self.chat.muted() {
            header_text.push_str(" (muted)")
        }

        // render the header
        let header = Block::default()
            .title(truncate(header_text, (splits[0].width - 8).into()))
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(Color::Black))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        header.render(splits[0], buf);

        let p_area = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(2)
            .vertical_margin(1)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(splits[0])[0];

        Paragraph::new(self.chat.pretty_members())
            .style(Style::default().fg(Color::Magenta))
            .render(p_area, buf);

        // chat messages
        let items: Vec<ListItem> = self
            .chat
            .messages
            .iter()
            .map(|m| m.to_list_item((area.width - 2) as usize))
            .collect();

        let mut list_state = self.chat.list_state.take();

        let list = List::new(items)
            .highlight_symbol("> ")
            .start_corner(Corner::BottomLeft);

        StatefulWidget::render(list, splits[1], buf, &mut list_state);
        self.chat.list_state.set(list_state);

        // reaction window
        if let Some(react) = self.chat.react.as_ref() {
            react.widget().render(area, buf)
        }
    }
}

fn make_message_list(
    timeline: &SortedVec<OrderedEvent>,
    members: &Vec<RoomMember>,
) -> Vec<Message> {
    let mut messages = vec![];
    let mut modifiers = vec![];

    // split everything into either a starting message, or something that
    // modifies an existing message
    for event in timeline.iter() {
        if let Some(message) = Message::try_from(event) {
            messages.push(message);
        } else {
            modifiers.push(event);
        }
    }

    // now apply all the modifiers to the message list
    for event in modifiers {
        Message::merge_into_message_list(&mut messages, event)
    }

    // update senders to friendly names
    messages.iter_mut().for_each(|m| m.update_senders(members));

    // merge all the reactions
    for m in messages.iter_mut() {
        m.reactions = Reaction::merge(&mut m.reactions);
    }

    // our message list is reversed because we start at the bottom of the
    // window and move up, like any good chat
    messages.reverse();

    messages
}
