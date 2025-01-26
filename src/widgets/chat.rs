use crate::app::{App, Popup};
use crate::event::{Event, EventHandler};
use crate::handler::Batch;
use crate::matrix::matrix::Matrix;
use crate::matrix::roomcache::DecoratedRoom;
use crate::settings::is_muted;
use crate::spawn::{get_file_paths, get_text};
use crate::widgets::message::{Message, Reaction, ReactionEvent};
use crate::widgets::react::React;
use crate::widgets::react::ReactResult;
use crate::widgets::EventResult::Consumed;
use crate::widgets::{get_margin, EventResult};
use crate::{consumed, limit_list, pretty_list, truncate, KeyCombo};
use anyhow::bail;
use crossterm::event::{KeyCode, KeyEvent};
use log::info;
use matrix_sdk::room::{Room, RoomMember};
use once_cell::sync::OnceCell;
use ruma::events::receipt::ReceiptEventContent;
use ruma::events::room::message::MessageType::Text;
use ruma::events::AnyTimelineEvent;
use ruma::{OwnedEventId, OwnedUserId};
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::ops::Deref;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{
    Block, BorderType, Borders, List, ListDirection, ListItem, ListState, Paragraph,
    StatefulWidget, Widget,
};

use super::confirm::{Confirm, ConfirmBehavior};
use super::message::MergeResult;
use super::receipts::Receipts;

pub struct Chat {
    matrix: Matrix,
    room: DecoratedRoom,
    events: BTreeSet<OrderedEvent>,
    receipts: Receipts,
    messages: Vec<Message>,
    read_to: Option<OwnedEventId>,
    react: Option<React>,
    typing: Option<String>,
    list_state: Cell<ListState>,
    next_cursor: Option<String>,
    fetching: Cell<bool>,
    width: Cell<usize>,
    total_list_items: Cell<usize>,
    focus: bool,
    delete_combo: KeyCombo,

    members: Vec<RoomMember>,
    pretty_members: OnceCell<String>,
    in_flight: Vec<OwnedUserId>,
}

impl Chat {
    pub fn try_new(matrix: Matrix, room: Room) -> Option<Self> {
        let decorated_room = match matrix.wrap_room(&room) {
            Some(r) => r,
            None => return None,
        };

        matrix.fetch_messages(room, None);

        Some(Self {
            matrix: matrix.clone(),
            room: decorated_room,
            events: BTreeSet::new(),
            receipts: Receipts::new(matrix.me()),
            messages: vec![],
            read_to: None,
            react: None,
            typing: None,
            list_state: Cell::new(ListState::default()),
            next_cursor: None,
            fetching: Cell::new(true),
            width: Cell::new(80),
            total_list_items: Cell::new(0),
            focus: true,
            delete_combo: KeyCombo::new(vec!['d', 'd']),
            members: vec![],
            pretty_members: OnceCell::new(),
            in_flight: vec![],
        })
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        self.widget().render(area, buf);
    }

    pub fn widget(&self) -> ChatWidget {
        ChatWidget { chat: self }
    }

    pub fn key_event(
        &mut self,
        input: &KeyEvent,
        handler: &EventHandler,
    ) -> anyhow::Result<EventResult> {
        // give our reaction window first dibs
        if let Some(react) = &mut self.react {
            match react.key_event(input) {
                ReactResult::Exit => {
                    self.react = None;
                    return Ok(consumed!());
                }
                ReactResult::SelectReaction(reaction) => {
                    self.react = None;

                    if let Some(message) = self.selected_reply() {
                        self.matrix
                            .send_reaction(self.room(), message.id.clone(), reaction)
                    }

                    return Ok(consumed!());
                }
                ReactResult::RemoveReaction(reaction) => {
                    self.react = None;

                    if let Some(event) = self.my_selected_reaction_event(reaction) {
                        self.matrix.redact_event(self.room(), event.id)
                    }

                    return Ok(consumed!());
                }
                ReactResult::Consumed => return Ok(consumed!()),
                ReactResult::Ignored => {}
            }
        }

        // then look for key combos
        if let KeyCode::Char(c) = input.code {
            if self.delete_combo.record(c) {
                let message = match self.selected_reply() {
                    Some(m) => m,
                    None => return Ok(EventResult::Ignored),
                };

                let preview = truncate(message.display().to_string(), 16);
                let warning = format!("Are you sure you want to delete \"{}\"", preview);

                let confirm = Confirm::new(
                    "Delete Message".to_string(),
                    warning,
                    "Yes".to_string(),
                    "No".to_string(),
                    ConfirmBehavior::DeleteMessage(self.room(), message.id.clone()),
                );

                return Ok(Consumed(Box::new(|app| {
                    app.set_popup(Popup::Confirm(confirm))
                })));
            }
        }

        match input.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.previous();
                Ok(consumed!())
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.next();
                self.try_fetch_previous();
                Ok(consumed!())
            }
            KeyCode::Enter => {
                if let Some(message) = &self.selected_reply() {
                    message.open(self.matrix.clone())
                }
                Ok(consumed!())
            }
            KeyCode::Char('s') => {
                if let Some(message) = &self.selected_reply() {
                    message.save(self.matrix.clone())
                }
                Ok(consumed!())
            }
            KeyCode::Char('c') => {
                let message = match self.selected_reply() {
                    Some(m) => m,
                    None => return Ok(EventResult::Ignored),
                };

                if matches!(message.body, Text(_)) {
                    handler.park();

                    let result = get_text(
                        Some(&message.display()),
                        Some(&format!(
                            "<!-- Edit your message above to change it in {}. -->",
                            self.room.name
                        )),
                    );

                    handler.unpark();

                    // make sure we redraw the whole app when we come back
                    App::get_sender().send(Event::Redraw)?;

                    if let Ok(edit) = result {
                        if let Some(edit) = edit {
                            self.matrix.replace_event(
                                self.room(),
                                message.id.clone(),
                                edit,
                                message.in_reply_to.clone(),
                            );

                            return Ok(consumed!());
                        } else {
                            bail!("Ignoring blank message.")
                        }
                    } else {
                        bail!("Couldn't read from editor.")
                    }
                }

                Ok(consumed!())
            }
            KeyCode::Char('i') => {
                let send = self.matrix.begin_typing(self.room());

                handler.park();
                let result = get_text(
                    None,
                    Some(&format!(
                        "<!-- Type a new message above to send to {}. -->",
                        self.room.name
                    )),
                );
                handler.unpark();

                self.matrix.end_typing(self.room(), send);
                App::get_sender().send(Event::Redraw)?;

                if let Ok(input) = result {
                    if let Some(input) = input {
                        self.matrix.send_text_message(self.room(), input);
                        Ok(consumed!())
                    } else {
                        bail!("Ignoring blank message.")
                    }
                } else {
                    bail!("Couldn't read from editor.")
                }
            }
            KeyCode::Char('R') => {
                let message = match self.selected_reply() {
                    Some(m) => m,
                    None => return Ok(consumed!()),
                };

                let wrap_options = textwrap::Options::new(60)
                    .initial_indent("  ")
                    .subsequent_indent("  ");

                let body = textwrap::wrap(&message.display(), &wrap_options).join("\n");

                let send = self.matrix.begin_typing(self.room());

                handler.park();
                let result = get_text(None, Some(&REPLY_TEMPLATE.replace("{}", &body)));
                handler.unpark();

                self.matrix.end_typing(self.room(), send);
                App::get_sender().send(Event::Redraw)?;

                if let Ok(input) = result {
                    if let Some(input) = input {
                        self.matrix
                            .send_reply(self.room(), input, message.id.clone());
                        Ok(consumed!())
                    } else {
                        bail!("Ignoring blank message.")
                    }
                } else {
                    bail!("Couldn't read from editor.")
                }
            }
            KeyCode::Char('v') => {
                let message = match self.selected_reply() {
                    Some(m) => m,
                    None => return Ok(EventResult::Ignored),
                };

                handler.park();
                get_text(Some(&message.display_full()), None)?;
                handler.unpark();

                App::get_sender().send(Event::Redraw)?;
                Ok(consumed!())
            }
            KeyCode::Char('V') => {
                handler.park();
                get_text(Some(&self.display_full()), None)?;
                handler.unpark();

                App::get_sender().send(Event::Redraw)?;
                Ok(consumed!())
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
                Ok(consumed!())
            }
            KeyCode::Char('u') => {
                let paths = get_file_paths()?;

                App::get_sender().send(Event::Redraw)?;

                if paths.is_empty() {
                    return Ok(EventResult::Ignored);
                }

                self.matrix.send_attachements(self.room(), paths);

                Ok(consumed!())
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

        self.check_event_sender(&event);
        self.events.insert(OrderedEvent::new(event));
        self.messages = make_message_list(&self.events, &self.members, &self.receipts);
        self.pretty_members = OnceCell::new();
        self.set_fully_read();
    }

    pub fn typing_event(&mut self, room: Room, ids: Vec<OwnedUserId>) {
        if room.room_id() != self.room.room_id() {
            return;
        }

        let typing: Vec<&RoomMember> = self
            .members
            .iter()
            .filter(|m| ids.iter().any(|id| m.user_id() == id))
            .collect();

        if typing.is_empty() {
            self.typing = None;
            return;
        }

        let suffix = if typing.len() > 1 {
            " are typing."
        } else {
            " is typing."
        };

        let total = typing.len();

        let iter = typing
            .into_iter()
            .map(|m| m.display_name().unwrap_or(m.user_id().as_str()))
            .map(|n| n.to_string());

        self.typing = Some(format!(
            "{}{}",
            pretty_list(limit_list(iter, 3, total, None)),
            suffix
        ));
    }

    pub fn receipt_event(&mut self, room: &Room, content: &ReceiptEventContent) {
        if room.room_id() == self.room.room_id() {
            self.receipts.apply_event(content);
            self.messages = make_message_list(&self.events, &self.members, &self.receipts);
            self.pretty_members = OnceCell::new();
            let me = self.matrix.me();

            // make sure we fetch any users we don't know about
            for id in Receipts::get_senders(content) {
                self.check_sender(id);

                // if it's us, that's essentially a room visit (clear notifications)
                if id == &me {
                    self.matrix.room_visit_event(room.clone());
                    info!("room viewed based on receipt");
                }
            }
        }
    }

    pub fn batch_event(&mut self, batch: Batch) {
        if batch.room.room_id() != self.room.room_id() {
            return;
        }

        self.next_cursor = batch.cursor;
        let previous_count = self.messages.len();

        for event in batch.events {
            self.check_event_sender(&event);
            self.events.insert(OrderedEvent::new(event));
        }

        let reset = self.messages.is_empty();

        self.messages = make_message_list(&self.events, &self.members, &self.receipts);
        self.pretty_members = OnceCell::new();
        self.fetching.set(false);
        self.set_fully_read();

        if reset {
            let mut state = self.list_state.take();
            state.select(Some(0));
            self.list_state.set(state);
        }

        if self.messages.len() > previous_count {
            self.try_fetch_previous();
        } else {
            info!("refusing to fetch more messages without making progress");
        }
    }

    fn check_event_sender(&mut self, event: &AnyTimelineEvent) {
        self.check_sender(&event.sender().to_owned());
    }

    fn check_sender(&mut self, user_id: &OwnedUserId) {
        // if we already know about them
        if self.members.iter().any(|m| m.user_id() == user_id) {
            return;
        }

        // or the request is in flight
        if self.in_flight.iter().any(|i| i == user_id) {
            return;
        }

        // otherwise, record them as in flight and fetch
        self.in_flight.push(user_id.clone());
        self.matrix.fetch_room_member(self.room(), user_id.clone());

        info!("fetching {}", user_id);
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

    pub fn room(&self) -> Room {
        self.room.inner()
    }

    fn pretty_members(&self) -> &str {
        self.pretty_members.get_or_init(|| {
            let mut members: Vec<&RoomMember> = vec![];

            // first grab folks who have sent read receipts
            let mut receipts = self.receipts.get_all();

            while let Some(receipt) = receipts.pop() {
                if let Some(member) = self.members.iter().find(|m| m.user_id() == receipt.user_id) {
                    members.push(member);
                }
            }

            // then walk all the events backwards, until we have a decent number
            for event in self.events.iter().rev() {
                if members.iter().any(|m| m.user_id() == event.sender()) {
                    continue;
                }

                if let Some(member) = self.members.iter().find(|m| m.user_id() == event.sender()) {
                    members.push(member);
                }

                if members.len() > 5 || members.len() == self.members.len() {
                    break;
                }
            }

            let iter = members.iter().map(|m| {
                m.display_name()
                    .unwrap_or_else(|| m.user_id().localpart())
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_string()
            });

            pretty_list(limit_list(iter, 5, self.members.len(), Some("at least")))
        })
    }

    pub fn room_member_event(&mut self, room: Room, member: RoomMember) {
        if self.room.room_id() != room.room_id() {
            return;
        }

        self.in_flight.retain(|id| id != member.user_id());
        self.members.push(member);
        self.pretty_members = OnceCell::new();
        self.messages = make_message_list(&self.events, &self.members, &self.receipts);
    }

    fn try_fetch_previous(&self) {
        if self.next_cursor.is_none() || self.fetching.get() {
            return;
        }

        let state = self.list_state.take();
        let buffer = self.total_list_items.get() - state.selected().unwrap_or_default();
        self.list_state.set(state);

        if buffer < 100 {
            self.matrix
                .fetch_messages(self.room(), self.next_cursor.clone());
            self.fetching.set(true);
            info!("fetching more events...")
        }
    }

    fn next(&self) {
        let mut state = self.list_state.take();

        let mut i = match state.selected() {
            Some(i) => {
                if i >= &self.total_list_items.get() - 1 {
                    &self.total_list_items.get() - 1
                } else {
                    i + 1
                }
            }
            None => 0,
        };

        if self.invalid_selection(i) {
            i += 1;
        }

        state.select(Some(i));
        self.list_state.set(state);
    }

    fn previous(&self) {
        let mut state = self.list_state.take();

        let mut i = match state.selected() {
            Some(i) => {
                if i == 0 {
                    0
                } else {
                    i - 1
                }
            }
            None => 0,
        };

        if self.invalid_selection(i) {
            i -= 1;
        }

        state.select(Some(i));
        self.list_state.set(state);
    }

    // the message (or reply) currently selected by the UI
    fn selected_reply(&self) -> Option<&Message> {
        if self.messages.is_empty() {
            return None;
        }

        let state = self.list_state.take();
        let selected = state.selected().unwrap_or_default();
        self.list_state.set(state);

        // count message heights until we overrun the counter
        let mut counter = 0;

        for m in &self.messages {
            let flattened = m.flatten();

            for (index, message) in flattened.iter().rev().enumerate() {
                counter += message.height(self.width.get(), index < flattened.len() - 1);

                if counter > selected {
                    return Some(message);
                }
            }
        }

        // otherwise, return the last reply on the last message
        if let Some(last) = self.messages.last() {
            return last.flatten().last().copied();
        }

        None
    }

    // is the given selection in the middle of two messages?
    fn invalid_selection(&self, selected: usize) -> bool {
        let mut counter = 0;

        for m in &self.messages {
            let flattened = m.flatten();

            for (index, message) in flattened.iter().rev().enumerate() {
                counter += message.height(self.width.get(), index < flattened.len() - 1);

                if counter > selected {
                    return counter == selected + 1;
                }
            }
        }

        false
    }

    // the reactions on the currently selected message
    fn selected_reactions(&self) -> Vec<Reaction> {
        match self.selected_reply() {
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
                if e.sender.id == me {
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
                if event.sender.id == me {
                    return Some(event);
                }
            }
        }

        None
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
        // equal IDs are always equal
        if self.event_id() == other.event_id() {
            return Ordering::Equal;
        }

        // if the timestamps are the same, use the id
        if self.origin_server_ts() == other.origin_server_ts() {
            return self.event_id().cmp(other.event_id());
        }

        // otherwise, us the timestamp
        self.origin_server_ts().cmp(&other.origin_server_ts())
    }
}

impl PartialOrd for OrderedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
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
            .vertical_margin(1)
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

        let (p_content, p_color) = if self.chat.typing.is_some() {
            (self.chat.typing.as_ref().unwrap().as_str(), Color::Yellow)
        } else {
            (self.chat.pretty_members(), Color::Magenta)
        };

        Paragraph::new(p_content)
            .style(Style::default().fg(p_color))
            .render(p_area, buf);

        // chat messages
        let items: Vec<ListItem> = self
            .chat
            .messages
            .iter()
            .flat_map(|m| m.to_list_items((area.width - 2) as usize))
            .collect();

        // make sure we save our last render width and total items
        self.chat.width.set((area.width - 2).into());
        self.chat.total_list_items.set(items.len());

        let mut list_state = self.chat.list_state.take();

        let list = List::new(items)
            .highlight_symbol("> ")
            .direction(ListDirection::BottomToTop);

        StatefulWidget::render(list, splits[1], buf, &mut list_state);
        self.chat.list_state.set(list_state);

        // reaction window
        if let Some(react) = self.chat.react.as_ref() {
            react.widget().render(area, buf)
        }
    }
}

fn make_message_list(
    timeline: &BTreeSet<OrderedEvent>,
    members: &Vec<RoomMember>,
    receipts: &Receipts,
) -> Vec<Message> {
    // TODO: don't split these out
    let mut messages = vec![];

    // split everything into either a starting message, or something that
    // modifies an existing message
    for event in timeline.iter() {
        if let Some(message) = Message::try_from(event, false) {
            messages.push(message);
        } else if Message::apply_timeline_event(&mut messages, event, 0) == MergeResult::Missed {
            // the event needed to be merge, but couldn't for some reason;
            // force it into place, if possible
            if let Some(message) = Message::try_from(event, true) {
                messages.push(message);
            }
        }
    }

    // apply our read receipts
    Message::apply_receipts(&mut messages, &mut receipts.get_all());

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

const REPLY_TEMPLATE: &str = "<!--
  Replying to:

{}
-->";
