use crate::app::App;
use crate::event::{Event, EventHandler};
use crate::handler::Batch;
use crate::matrix::matrix::Matrix;
use crate::spawn::get_text;
use crate::widgets::message::{Message, Reaction, ReactionEvent};
use crate::widgets::react::React;
use crate::widgets::EventResult::Consumed;
use crate::widgets::{get_margin, EventResult};
use anyhow::bail;
use crossterm::event::{KeyCode, KeyEvent};
use log::info;
use matrix_sdk::room::{Joined, RoomMember};
use ruma::events::AnyTimelineEvent;
use sorted_vec::SortedVec;
use std::cell::Cell;
use std::cmp::Ordering;
use std::ops::Deref;
use tui::buffer::Buffer;
use tui::layout::{Constraint, Corner, Direction, Layout, Rect};
use tui::widgets::{List, ListItem, ListState, StatefulWidget, Widget};

use super::Action;

pub struct Chat {
    matrix: Matrix,
    pub room: Option<Joined>,
    events: SortedVec<OrderedEvent>,
    messages: Vec<Message>,
    members: Vec<RoomMember>,
    react: Option<React>,
    list_state: Cell<ListState>,
    next_cursor: Option<String>,
    fetching: Cell<bool>,
}

impl Chat {
    pub fn new(matrix: Matrix) -> Self {
        Self {
            matrix,
            room: None,
            events: SortedVec::new(),
            messages: vec![],
            members: vec![],
            react: None,
            list_state: Cell::new(ListState::default()),
            next_cursor: None,
            fetching: Cell::new(false),
        }
    }

    pub fn set_room(&mut self, room: Joined) {
        self.matrix.fetch_messages(room.clone(), None);
        self.matrix.fetch_room_members(room.clone());
        self.fetching.set(true);
        self.room = Some(room);
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
                        self.matrix.send_reaction(
                            self.room.clone().unwrap(),
                            message.id.clone(),
                            reaction,
                        )
                    }
                    return Ok(Consumed(Action::Typing));
                }
                Consumed(Action::RemoveReaction(reaction)) => {
                    self.react = None;

                    if let Some(event) = self.my_selected_reaction_event(reaction) {
                        self.matrix
                            .redact_event(self.room.clone().unwrap(), event.id.clone())
                    }
                }
                Consumed(_) => return Ok(Consumed(Action::Typing)),
                _ => {}
            }
        }

        match input.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.previous();
                return Ok(Consumed(Action::Typing));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.next();
                self.try_fetch_previous();
                return Ok(Consumed(Action::Typing));
            }
            KeyCode::Char('o') | KeyCode::Enter => {
                if let Some(message) = &self.selected_message() {
                    message.open(self.matrix.clone())
                }
                return Ok(Consumed(Action::Typing));
            }
            KeyCode::Char('i') => {
                handler.park();
                let result = get_text();
                handler.unpark();

                // make sure we redraw the whole app when we come back
                App::get_sender().send(Event::Redraw)?;

                if let Ok(input) = result {
                    if let Some(input) = input {
                        self.matrix
                            .send_text_message(self.room.clone().unwrap(), input);
                        return Ok(Consumed(Action::Typing));
                    } else {
                        bail!("Ignoring blank message.")
                    }
                } else {
                    bail!("Couldn't read from editor.")
                }
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
                return Ok(Consumed(Action::Typing));
            }
            _ => return Ok(EventResult::Ignored),
        };
    }

    pub fn timeline_event(&mut self, event: AnyTimelineEvent) {
        if self.room.is_none() || event.room_id() != self.room.as_ref().unwrap().room_id() {
            return;
        }

        self.events.push(OrderedEvent::new(event));
        self.messages = make_message_list(&self.events, &self.members);
    }

    pub fn batch_event(&mut self, batch: Batch) {
        if self.room.is_none() || batch.room.room_id() != self.room.as_ref().unwrap().room_id() {
            return;
        }

        self.next_cursor = batch.cursor;

        for event in batch.events {
            self.events.push(OrderedEvent::new(event));
        }

        let reset = self.messages.is_empty();

        self.messages = make_message_list(&self.events, &self.members);
        self.fetching.set(false);

        if reset {
            let mut state = self.list_state.take();
            state.select(Some(0));
            self.list_state.set(state);
        }
    }

    pub fn room_members_event(&mut self, room: Joined, members: Vec<RoomMember>) {
        if self.room.is_none() || self.room.as_ref().unwrap().room_id() != room.room_id() {
            return;
        }

        self.members = members;
        self.messages = make_message_list(&self.events, &self.members);
    }

    fn try_fetch_previous(&self) {
        if self.next_cursor.is_none() || self.room.is_none() || self.fetching.get() {
            return;
        }

        let state = self.list_state.take();
        let buffer = self.messages.len() - state.selected().unwrap_or_default();
        self.list_state.set(state);

        if buffer < 25 {
            self.matrix.fetch_messages(
                self.room.as_ref().unwrap().clone(),
                self.next_cursor.clone(),
            );
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

        self.messages.get(selected).and_then(|m| Some(m.clone()))
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

        return None;
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
        let area = Layout::default()
            .direction(Direction::Horizontal)
            .horizontal_margin(get_margin(area.width, 80))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

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

        StatefulWidget::render(list, area, buf, &mut list_state);
        self.chat.list_state.set(list_state);

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
