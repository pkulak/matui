use crate::app::{App, Popup};
use crate::event::{Event, EventHandler};
use crate::handler::Batch;
use crate::matrix::matrix::Matrix;
use crate::matrix::roomcache::DecoratedRoom;
use crate::settings::{is_muted, max_events, toggle_mute};
use crate::spawn::{get_file_paths, spawn_editor};
use crate::widgets::EventResult::Consumed;
use crate::widgets::command::Command;
use crate::widgets::compose::Compose;
use crate::widgets::message::{LineType, Message, Reaction, ReactionEvent};
use crate::widgets::react::React;
use crate::widgets::react::ReactResult;
use crate::widgets::search::Search;
use crate::widgets::upload::Upload;
use crate::widgets::{EventResult, get_margin};
use crate::{KeyCombo, consumed, limit_list, pretty_list, truncate};
use anyhow::bail;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use log::info;
use matrix_sdk::RoomMemberships;
use matrix_sdk::room::{Room, RoomMember};
use matrix_sdk::ruma::events::receipt::ReceiptEventContent;
use matrix_sdk::ruma::events::room::member::{MembershipChange, RoomMemberEvent};
use matrix_sdk::ruma::events::room::message::MessageType::Text;
use matrix_sdk::ruma::events::room::message::ReplyWithinThread;
use matrix_sdk::ruma::events::room::name::RoomNameEvent;
use matrix_sdk::ruma::events::room::power_levels::UserPowerLevel;
use matrix_sdk::ruma::events::room::topic::RoomTopicEvent;
use matrix_sdk::ruma::events::{AnyStateEvent, AnyTimelineEvent};
use matrix_sdk::ruma::{EventId, MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedUserId, UserId};
use once_cell::sync::OnceCell;
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::ops::{Deref, Range};
use std::sync::Mutex;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
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
    messages: Vec<TimelineItem>,
    window: Range<usize>,
    read_to: Option<OwnedEventId>,
    react: Option<React>,
    typing: Option<String>,
    list_state: Cell<ListState>,
    next_cursor: Option<String>,
    fetching: Cell<bool>,
    height: Cell<usize>,
    line_types: Mutex<Vec<LineType>>,
    bookmark: Cell<Option<Bookmark>>,
    focus: bool,
    search_term: String,
    thread_root: Option<OwnedEventId>,
    delete_combo: KeyCombo,

    members: Vec<RoomMember>,
    pretty_members: OnceCell<String>,
    in_flight: Vec<OwnedUserId>,
}

impl Chat {
    pub fn try_new(matrix: Matrix, room: Room) -> Option<Self> {
        let decorated_room = matrix.wrap_room(&room)?;

        matrix.fetch_messages(room, None, 25);

        Some(Self {
            matrix: matrix.clone(),
            room: decorated_room,
            events: BTreeSet::new(),
            receipts: Receipts::new(matrix.me()),
            messages: vec![],
            window: (0..0),
            read_to: None,
            react: None,
            typing: None,
            list_state: Cell::new(ListState::default()),
            next_cursor: None,
            fetching: Cell::new(true),
            height: Cell::new(20),
            line_types: Mutex::new(vec![]),
            bookmark: Cell::new(Option::None),
            focus: true,
            search_term: "".to_string(),
            thread_root: None,
            delete_combo: KeyCombo::new(vec!['d', 'd']),
            members: vec![],
            pretty_members: OnceCell::new(),
            in_flight: vec![],
        })
    }

    // a new chat viewing a single thread in this room, seeded with
    // everything we've already loaded
    pub fn thread(&self, root: OwnedEventId) -> Self {
        let mut chat = Self {
            matrix: self.matrix.clone(),
            room: self.room.clone(),
            events: self.events.clone(),
            receipts: self.receipts.clone(),
            messages: vec![],
            window: (0..0),
            read_to: None,
            react: None,
            typing: None,
            list_state: Cell::new(ListState::default()),
            next_cursor: self.next_cursor.clone(),
            fetching: Cell::new(false),
            height: Cell::new(20),
            line_types: Mutex::new(vec![]),
            bookmark: Cell::new(Option::None),
            focus: true,
            search_term: "".to_string(),
            thread_root: Some(root),
            delete_combo: KeyCombo::new(vec!['d', 'd']),
            members: self.members.clone(),
            pretty_members: OnceCell::new(),
            in_flight: vec![],
        };

        let mut state = chat.list_state.take();
        state.select(Some(0));
        chat.list_state.set(state);

        chat.set_messages(false);
        chat
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        self.widget().render(area, buf);
    }

    pub fn widget(&self) -> ChatWidget<'_> {
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
        if let KeyCode::Char(c) = input.code
            && input.modifiers.is_empty()
            && self.delete_combo.record(c)
        {
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

        match input.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.previous(1);
                Ok(consumed!())
            }
            KeyCode::Char('d') if input.modifiers.contains(KeyModifiers::CONTROL) => {
                self.previous(self.height.get() / 2);
                Ok(consumed!())
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.next(1);
                self.try_fetch_previous();
                Ok(consumed!())
            }
            KeyCode::Char('u') if input.modifiers.contains(KeyModifiers::CONTROL) => {
                self.next(self.height.get() / 2);
                self.try_fetch_previous();
                Ok(consumed!())
            }
            KeyCode::Char('G') => {
                self.first();
                Ok(consumed!())
            }
            KeyCode::Enter => {
                if !self.search_term.is_empty() {
                    self.search_term = "".to_string();
                    self.set_messages(true);
                } else if let Some(message) = self.selected_reply() {
                    // opening a thread gets its own window; everything else
                    // opens the message itself
                    if self.thread_root.is_none() && !message.thread.is_empty() {
                        let thread = self.thread(message.id.clone());

                        return Ok(Consumed(Box::new(move |app| app.thread = Some(thread))));
                    }

                    message.open(self.matrix.clone())
                }
                Ok(consumed!())
            }
            KeyCode::Esc if self.thread_root.is_some() => {
                Ok(Consumed(Box::new(|app| app.thread = None)))
            }
            KeyCode::Char('s') => {
                if let Some(message) = &self.selected_reply() {
                    message.save(self.matrix.clone())
                }
                Ok(consumed!())
            }
            KeyCode::Char('m') => {
                toggle_mute(self.room().room_id());
                Ok(consumed!())
            }
            KeyCode::Char('c') => {
                let message = match self.selected_reply() {
                    Some(m) => m,
                    None => return Ok(EventResult::Ignored),
                };

                if matches!(message.body, Text(_)) {
                    let result = spawn_editor(
                        handler,
                        None,
                        Some(message.display()),
                        Some(&format!(
                            "<!-- Edit your message above to change it in {}. -->",
                            self.room.name
                        )),
                    );

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
                let room = self.room.clone();
                let matrix = self.matrix.clone();
                let thread = self.thread_target();

                Ok(Consumed(Box::new(|app| {
                    app.set_popup(Popup::Compose(Compose::new(room, matrix, thread)))
                })))
            }
            KeyCode::Char('I') => {
                let result = spawn_editor(
                    handler,
                    Some((&self.matrix, self.room())),
                    None,
                    Some(&format!(
                        "<!-- The message above will be sent to {}. -->",
                        self.room.name
                    )),
                );

                if let Ok(Some(message)) = result
                    && !message.trim().is_empty()
                {
                    match self.thread_target() {
                        Some(target) => self.matrix.send_thread_message(
                            self.room(),
                            message,
                            target,
                            ReplyWithinThread::No,
                        ),
                        None => self.matrix.send_text_message(self.room(), message),
                    }
                }

                Ok(consumed!())
            }
            KeyCode::Char('R') => {
                let message = match self.selected_reply() {
                    Some(m) => m,
                    None => return Ok(consumed!()),
                };

                let wrap_options = textwrap::Options::new(60)
                    .initial_indent("  ")
                    .subsequent_indent("  ");

                let body = textwrap::wrap(message.display(), &wrap_options).join("\n");

                let result = spawn_editor(
                    handler,
                    Some((&self.matrix, self.room())),
                    None,
                    Some(&REPLY_TEMPLATE.replace("{}", &body)),
                );

                if let Ok(input) = result {
                    if let Some(input) = input {
                        if self.thread_root.is_some() {
                            self.matrix.send_thread_message(
                                self.room(),
                                input,
                                message.id.clone(),
                                ReplyWithinThread::Yes,
                            );
                        } else {
                            self.matrix
                                .send_reply(self.room(), input, message.id.clone());
                        }
                        Ok(consumed!())
                    } else {
                        bail!("Ignoring blank message.")
                    }
                } else {
                    bail!("Couldn't read from editor.")
                }
            }
            KeyCode::Char('T') => {
                // threads don't nest
                if self.thread_root.is_some() {
                    return Ok(EventResult::Ignored);
                }

                let message = match self.selected_reply() {
                    Some(m) => m,
                    None => return Ok(EventResult::Ignored),
                };

                // open the thread view, empty or not; sending the first
                // message is what actually creates the thread
                let thread = self.thread(message.id.clone());

                Ok(Consumed(Box::new(move |app| app.thread = Some(thread))))
            }
            KeyCode::Char('v') => {
                let message = match self.selected_reply() {
                    Some(m) => m,
                    None => return Ok(EventResult::Ignored),
                };

                spawn_editor(handler, None, Some(&message.display_full()), None)?;

                Ok(consumed!())
            }
            KeyCode::Char('V') => {
                spawn_editor(handler, None, Some(&self.display_full()?), None)?;

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

                let matrix = self.matrix.clone();
                let room = self.room();

                Ok(Consumed(Box::new(move |app| {
                    app.set_popup(Popup::Upload(Upload::new(matrix, room, paths)))
                })))
            }
            KeyCode::Char('/') => Ok(Consumed(Box::new(|app| {
                app.set_popup(Popup::Search(Search::default()))
            }))),
            KeyCode::Char(':') => Ok(Consumed(Box::new(|app| {
                app.set_popup(Popup::Command(Command::default()))
            }))),
            KeyCode::Char('U') => {
                let message = match self.selected_reply() {
                    Some(m) => m,
                    None => return Ok(EventResult::Ignored),
                };

                let sender = message.sender.id.clone();

                Ok(Consumed(Box::new(move |app| {
                    app.set_popup(Popup::Command(Command::with_user(&sender)))
                })))
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
        self.set_messages(false);
        self.pretty_members = OnceCell::new();
        self.set_fully_read();
    }

    pub fn typing_event(&mut self, room: Room, ids: Vec<OwnedUserId>) {
        if room.room_id() != self.room.room_id() {
            return;
        }

        let me = self.matrix.me();

        let typing: Vec<&RoomMember> = self
            .members
            .iter()
            .filter(|m| m.user_id() != me && ids.iter().any(|id| m.user_id() == id))
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
            self.set_messages(false);
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
        let batch_size = batch.events.len();

        for event in batch.events {
            self.check_event_sender(&event);
            self.events.insert(OrderedEvent::new(event));
        }

        let reset = self.messages.is_empty();

        self.set_messages(false);
        self.pretty_members = OnceCell::new();
        self.fetching.set(false);
        self.set_fully_read();

        if reset {
            let mut state = self.list_state.take();
            state.select(Some(0));
            self.list_state.set(state);
        }

        // searches and threads only surface a sliver of each batch, so any
        // events at all count as progress there
        let filtering = !self.search_term.is_empty() || self.thread_root.is_some();

        if self.messages.len() > previous_count || (filtering && batch_size > 0) {
            self.try_fetch_previous();
        } else {
            info!("refusing to fetch more messages without making progress");
        }
    }

    pub fn search_event(&mut self, search_term: &str) {
        self.search_term = search_term.to_string();
        self.set_messages(false);
        self.try_fetch_previous();

        if search_term.is_empty() {
            self.first();
        }
    }

    fn check_event_sender(&mut self, event: &AnyTimelineEvent) {
        self.check_sender(&event.sender().to_owned());

        // member events also have a target
        if let AnyTimelineEvent::State(AnyStateEvent::RoomMember(member)) = event {
            self.check_sender(member.state_key());
        }
    }

    fn set_messages(&mut self, force_bookmark: bool) {
        self.messages = make_message_list(
            &self.events,
            &self.members,
            &self.receipts,
            &self.search_term,
            self.thread_root.as_deref(),
        );

        self.adjust_range(force_bookmark);
    }

    fn adjust_range(&mut self, force_bookmark: bool) {
        if self.messages.is_empty() {
            self.window = 0..0;
            return;
        }

        let first_id = self.iter_messages().next().map(|m| m.id.clone());

        let mut bookmark = match self.get_bookmark() {
            Some(b) => b,
            None => match first_id {
                Some(message_id) => Bookmark {
                    message_id,
                    offset: 1,
                    window_offset: 0,
                },
                None => {
                    // nothing selectable at all; show everything
                    self.window = 0..self.messages.len();
                    return;
                }
            },
        };

        let new_selected = self
            .messages
            .iter()
            .position(|i| {
                i.message()
                    .is_some_and(|m| m.contains_id(&bookmark.message_id))
            })
            .unwrap_or_default();

        // only keep a 100-long window around our index
        let start = new_selected.saturating_sub(100);
        let end = (new_selected + 101).min(self.messages.len());

        self.window = start..end;

        let list_state = self.list_state.take();
        let selected = list_state.selected().unwrap_or_default();
        let offset = list_state.offset();
        self.list_state.set(list_state);

        if offset == 0 && !force_bookmark {
            // if we're on the bottom, stay there
            if selected == 0 {
                return;
            }

            // if it's just the offset on the bottom, keep it
            if offset == 0 {
                bookmark.window_offset = usize::MAX;
            }
        }

        self.bookmark.set(Some(bookmark));
    }

    fn iter_messages(&self) -> impl Iterator<Item = &Message> {
        self.messages.iter().filter_map(TimelineItem::message)
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
        // the thread view leaves the room's read marker alone; the room
        // chat underneath keeps it current
        if !self.focus || self.thread_root.is_some() {
            return;
        }

        let read_to = self.iter_messages().next().map(|m| m.id.clone());

        if read_to == self.read_to {
            return;
        }

        if let Some(id) = read_to.clone() {
            self.matrix.read_to(self.room(), id);
            self.read_to = read_to;
        }
    }

    fn display_full(&self) -> anyhow::Result<String> {
        // normally a local store read; the app syncs the full member
        // list the first time a room's senders are resolved
        let mut members = App::get_handle()
            .block_on(async { self.room().members(RoomMemberships::JOIN).await })?;

        members.sort_by_key(|m| {
            m.display_name()
                .unwrap_or(m.user_id().as_str())
                .to_lowercase()
        });

        let mut ret = format!("{} ({})\n\n", self.room.name, self.room.room_id());

        ret.push_str(&format!("# Members ({})\n\n", members.len()));

        for m in &members {
            let power = match m.power_level() {
                UserPowerLevel::Infinite => " — creator".to_string(),
                UserPowerLevel::Int(n) if i64::from(n) != 0 => format!(" — {}", n),
                _ => String::new(),
            };

            ret.push_str(&format!(
                "* {} ({}){}\n",
                m.display_name().unwrap_or(m.user_id().as_str()),
                m.user_id(),
                power
            ));
        }

        Ok(ret)
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
        self.set_messages(false);
    }

    fn try_fetch_previous(&self) {
        if self.next_cursor.is_none() || self.fetching.get() {
            return;
        }

        let state = self.list_state.take();
        let buffer = self.total_list_items() - state.selected().unwrap_or_default();
        self.list_state.set(state);

        if buffer < 100 && self.events.len() < max_events() {
            let limit = if self.search_term.is_empty() { 32 } else { 256 };

            self.matrix
                .fetch_messages(self.room(), self.next_cursor.clone(), limit);

            self.fetching.set(true);
            info!("fetching more events... {}", self.events.len())
        }
    }

    fn next(&mut self, step: usize) {
        let total = self.total_list_items();

        if total == 0 {
            return;
        }

        let mut state = self.list_state.take();

        let mut i = match state.selected() {
            Some(i) => (i + step).min(total - 1),
            None => 0,
        };

        while i < total && self.invalid_selection(i) {
            i += 1;
        }

        // nothing selectable above us; walk back down
        if i >= total {
            i = total - 1;

            while i > 0 && self.invalid_selection(i) {
                i -= 1;
            }
        }

        state.select(Some(i));
        self.list_state.set(state);
        self.adjust_range(false);
    }

    fn previous(&mut self, step: usize) {
        let total = self.total_list_items();

        if total == 0 {
            return;
        }

        let mut state = self.list_state.take();

        let mut i = state
            .selected()
            .map(|i| i.saturating_sub(step))
            .unwrap_or_default();

        while i > 0 && self.invalid_selection(i) {
            i -= 1;
        }

        // nothing selectable below us; walk back up
        while i < total - 1 && self.invalid_selection(i) {
            i += 1;
        }

        state.select(Some(i));
        self.list_state.set(state);
        self.adjust_range(false);
    }

    fn first(&mut self) {
        let mut state = self.list_state.take();
        *state.selected_mut() = Some(0);
        *state.offset_mut() = 0;
        self.list_state.set(state);

        self.window = 0..(self.messages.len().min(200));
        self.bookmark.set(None);
    }

    fn get_bookmark(&self) -> Option<Bookmark> {
        let list_state = self.list_state.take();
        let ls_selected = list_state.selected().unwrap_or_default();
        let ls_offset = list_state.offset();
        self.list_state.set(list_state);

        let mut selected = ls_selected;
        let mut offset = 0;
        let lines = self.line_types.lock().unwrap();

        loop {
            let line = lines.get(selected)?;

            if let LineType::MessageStart(message_id) = line {
                return Some(Bookmark {
                    message_id: message_id.clone(),
                    offset,
                    window_offset: ls_selected.saturating_sub(ls_offset),
                });
            }

            selected += 1;
            offset += 1;
        }
    }

    fn total_list_items(&self) -> usize {
        self.line_types.lock().unwrap().len()
    }

    // when viewing a thread, new messages attach to the newest one
    fn thread_target(&self) -> Option<OwnedEventId> {
        self.thread_root.as_ref()?;
        self.iter_messages().next().map(|m| m.id.clone())
    }

    // the message (or reply) currently selected by the UI
    fn selected_reply(&self) -> Option<&Message> {
        let bookmark = self.get_bookmark()?;
        self.iter_messages()
            .find_map(|m| m.find_by_id(&bookmark.message_id))
    }

    // is the given selection in the middle of two messages?
    fn invalid_selection(&self, selected: usize) -> bool {
        matches!(
            self.line_types.lock().unwrap().get(selected),
            Some(LineType::DeadSpace | LineType::RoomEvent)
        )
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
                continue;
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

pub struct Bookmark {
    // which message are we pointing at?
    message_id: OwnedEventId,

    // how many lines down from the top?
    offset: usize,

    // and how offset is the bottom of the window?
    window_offset: usize,
}

// a good PR would be to add Ord to AnyTimelineEvent
#[derive(Clone)]
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

        buf.set_style(area, Style::default().bg(Color::Reset));

        let area = Layout::default()
            .direction(Direction::Horizontal)
            .horizontal_margin(get_margin(area.width, 80))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .vertical_margin(1)
            .constraints([Constraint::Length(3), Constraint::Percentage(100)].as_ref())
            .split(area);

        let mut header_text = match &self.chat.thread_root {
            Some(_) => format!(
                "Thread from: {} (esc to go back)",
                truncate(self.chat.room.name.to_string(), 24)
            ),
            None => self.chat.room.name.to_string(),
        };

        if self.chat.muted() {
            header_text.push_str(" (muted)")
        }

        // render the header
        let header = Block::default()
            .title(truncate(header_text, (splits[0].width - 8).into()))
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(Color::Reset))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        header.render(splits[0], buf);

        let p_area = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(2)
            .vertical_margin(1)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(splits[0])[0];

        let (p_content, p_color) = if let Some(typing) = &self.chat.typing {
            (typing.to_string(), Color::Yellow)
        } else if self.chat.fetching.get() {
            let term = if self.chat.search_term.is_empty() {
                "Loading"
            } else {
                "Searching"
            };

            (
                format!("{}... ({})", term, self.chat.events.len()),
                Color::Yellow,
            )
        } else {
            (self.chat.pretty_members().to_string(), Color::Magenta)
        };

        Paragraph::new(p_content)
            .style(Style::default().fg(p_color))
            .render(p_area, buf);

        let mut line_types = self.chat.line_types.lock().unwrap();
        line_types.clear();

        let mut items: Vec<ListItem> = vec![];
        let mut buffer: Vec<LineType> = vec![];
        let window = &self.chat.messages[self.chat.window.clone()];

        for m in window.iter() {
            items.append(&mut m.to_list_items((area.width - 2) as usize, &mut buffer));
            line_types.append(&mut buffer);
        }

        // make sure we save our last render dimensions
        self.chat.height.set((splits[1].height).into());

        let mut list_state = self.chat.list_state.take();

        // if there's a bookmark, use it to set our selection
        if let Some(bookmark) = self.chat.bookmark.take() {
            let selected = find_bookmark(&bookmark, &line_types).unwrap_or_default();
            let offset = selected.saturating_sub(bookmark.window_offset);

            *list_state.selected_mut() = Some(selected);
            *list_state.offset_mut() = offset;
        }

        // never leave the cursor resting on a spacer or room-event line
        if let Some(mut i) = list_state.selected() {
            while matches!(
                line_types.get(i),
                Some(LineType::DeadSpace | LineType::RoomEvent)
            ) {
                i += 1;
            }

            *list_state.selected_mut() = Some(i.min(line_types.len().saturating_sub(1)));
        }

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

fn find_bookmark(bookmark: &Bookmark, lines: &[LineType]) -> Option<usize> {
    for (i, line) in lines.iter().enumerate() {
        if let LineType::MessageStart(message_id) = line
            && *message_id == bookmark.message_id
        {
            return Some(i.saturating_sub(bookmark.offset));
        }
    }

    None
}

#[allow(clippy::large_enum_variant)]
pub enum TimelineItem {
    Message(Message),
    Events(RoomEvents),
}

impl TimelineItem {
    fn message(&self) -> Option<&Message> {
        match self {
            TimelineItem::Message(m) => Some(m),
            TimelineItem::Events(_) => None,
        }
    }

    pub fn to_list_items(&self, width: usize, sidecar: &mut Vec<LineType>) -> Vec<ListItem<'_>> {
        match self {
            TimelineItem::Message(m) => m.to_list_items(width, sidecar),
            TimelineItem::Events(e) => e.to_list_items(sidecar),
        }
    }
}

// a group of consecutive room state events, rendered as single gray lines
pub struct RoomEvents {
    ts: MilliSecondsSinceUnixEpoch,
    lines: Vec<String>,
}

impl RoomEvents {
    fn to_list_items(&self, sidecar: &mut Vec<LineType>) -> Vec<ListItem<'_>> {
        // mirror Message::to_list_items: build top-down, then reverse
        let mut items = vec![ListItem::new(" ")];
        sidecar.push(LineType::DeadSpace);

        for line in &self.lines {
            items.push(ListItem::new(Span::styled(
                line.as_str(),
                Style::default().fg(Color::DarkGray),
            )));

            sidecar.push(LineType::RoomEvent);
        }

        sidecar.reverse();
        items.reverse();
        items
    }
}

#[derive(Clone, Copy, PartialEq)]
enum MemberKind {
    Joined,
    Left,
    Invited,
    Kicked,
    Banned,
    Unbanned,
}

enum StateEntry {
    Member(MemberKind, String),
    Line(String),
}

#[derive(Default)]
struct PendingEvents {
    entries: Vec<StateEntry>,
    ts: Option<MilliSecondsSinceUnixEpoch>,
}

impl PendingEvents {
    fn push(&mut self, entry: StateEntry, ts: MilliSecondsSinceUnixEpoch) {
        self.entries.push(entry);
        self.ts = Some(ts);
    }

    // combine runs of same-kind member entries into single lines
    fn flush(&mut self) -> Option<RoomEvents> {
        let ts = self.ts.take()?;
        let mut lines = vec![];
        let mut run: Option<(MemberKind, Vec<String>)> = None;

        for entry in self.entries.drain(..) {
            match entry {
                StateEntry::Member(kind, name) => match &mut run {
                    Some((k, names)) if *k == kind => {
                        if !names.contains(&name) {
                            names.push(name);
                        }
                    }
                    _ => {
                        if let Some((k, names)) = run.take() {
                            lines.push(member_line(k, names));
                        }

                        run = Some((kind, vec![name]));
                    }
                },
                StateEntry::Line(line) => {
                    if let Some((k, names)) = run.take() {
                        lines.push(member_line(k, names));
                    }

                    lines.push(line);
                }
            }
        }

        if let Some((k, names)) = run.take() {
            lines.push(member_line(k, names));
        }

        Some(RoomEvents { ts, lines })
    }
}

fn state_entry(event: &AnyTimelineEvent, members: &[RoomMember]) -> Option<StateEntry> {
    let AnyTimelineEvent::State(state) = event else {
        return None;
    };

    match state {
        AnyStateEvent::RoomMember(RoomMemberEvent::Original(ev)) => {
            let kind = match ev.membership_change() {
                MembershipChange::Joined | MembershipChange::InvitationAccepted => {
                    MemberKind::Joined
                }
                MembershipChange::Left => MemberKind::Left,
                MembershipChange::Invited => MemberKind::Invited,
                MembershipChange::Kicked => MemberKind::Kicked,
                MembershipChange::Banned | MembershipChange::KickedAndBanned => MemberKind::Banned,
                MembershipChange::Unbanned => MemberKind::Unbanned,
                // profile changes, knocks, etc are just noise
                _ => return None,
            };

            let name = display_name(&ev.state_key, ev.content.displayname.as_deref(), members);
            Some(StateEntry::Member(kind, name))
        }
        AnyStateEvent::RoomName(RoomNameEvent::Original(ev)) => Some(StateEntry::Line(format!(
            "{} changed the room name to {}.",
            display_name(&ev.sender, None, members),
            truncate(ev.content.name.clone(), 48),
        ))),
        AnyStateEvent::RoomTopic(RoomTopicEvent::Original(ev)) => Some(StateEntry::Line(format!(
            "{} changed the topic to {}.",
            display_name(&ev.sender, None, members),
            truncate(ev.content.topic.clone(), 48),
        ))),
        _ => None,
    }
}

fn display_name(id: &UserId, fallback: Option<&str>, members: &[RoomMember]) -> String {
    members
        .iter()
        .find(|m| m.user_id() == id)
        .and_then(|m| m.display_name())
        .or(fallback)
        .unwrap_or_else(|| id.localpart())
        .to_string()
}

fn member_line(kind: MemberKind, names: Vec<String>) -> String {
    let total = names.len();
    let subject = pretty_list(limit_list(names.into_iter(), 3, total, None));

    let verb = match (kind, total > 1) {
        (MemberKind::Joined, _) => "joined the room",
        (MemberKind::Left, _) => "left the room",
        (MemberKind::Invited, false) => "was invited to the room",
        (MemberKind::Invited, true) => "were invited to the room",
        (MemberKind::Kicked, false) => "was kicked from the room",
        (MemberKind::Kicked, true) => "were kicked from the room",
        (MemberKind::Banned, false) => "was banned from the room",
        (MemberKind::Banned, true) => "were banned from the room",
        (MemberKind::Unbanned, false) => "was unbanned",
        (MemberKind::Unbanned, true) => "were unbanned",
    };

    format!("{} {}.", subject, verb)
}

// merge adjacent groups; only possible when the message that separated
// them was redacted
fn push_group(items: &mut Vec<TimelineItem>, group: RoomEvents) {
    if let Some(TimelineItem::Events(prev)) = items.last_mut() {
        // the incoming group is older, so its lines go on top
        let mut lines = group.lines;
        lines.append(&mut prev.lines);
        prev.lines = lines;
    } else {
        items.push(TimelineItem::Events(group));
    }
}

fn make_message_list(
    timeline: &BTreeSet<OrderedEvent>,
    members: &Vec<RoomMember>,
    receipts: &Receipts,
    search_term: &str,
    thread_root: Option<&EventId>,
) -> Vec<TimelineItem> {
    // TODO: don't split these out
    let mut messages = vec![];
    let mut groups: Vec<RoomEvents> = vec![];
    let mut pending = PendingEvents::default();

    if let Some(root) = thread_root {
        for event in timeline.iter() {
            if event.event_id() == root {
                if let Some(message) = Message::try_from(event, true) {
                    messages.push(message);
                }
            } else {
                // thread messages attach to the root; edits, reactions and
                // redactions merge; everything else misses and drops
                Message::apply_timeline_event(&mut messages, event, 0);
            }
        }

        // show the thread like a room: root first, then its messages
        if let Some(root_message) = messages.first_mut() {
            let thread = std::mem::take(&mut root_message.thread);
            messages.extend(thread);
        }
    } else {
        // split everything into either a starting message, a room state
        // event, or something that modifies an existing message
        for event in timeline.iter() {
            if let Some(message) = Message::try_from(event, false) {
                groups.extend(pending.flush());
                messages.push(message);
            } else if search_term.is_empty()
                && let Some(entry) = state_entry(event, members)
            {
                pending.push(entry, event.origin_server_ts());
            } else if Message::apply_timeline_event(&mut messages, event, 0) == MergeResult::Missed
            {
                // the event needed to be merge, but couldn't for some reason;
                // force it into place, if possible
                if let Some(message) = Message::try_from(event, true) {
                    groups.extend(pending.flush());
                    messages.push(message);
                }
            }
        }

        groups.extend(pending.flush());
    }

    if !search_term.is_empty() {
        messages.retain(|m| m.contains_search_term(search_term));
        messages.iter().for_each(|m| m.set_search_term(search_term));
    }

    // apply our read receipts (they mark room positions, not thread ones)
    if thread_root.is_none() {
        Message::apply_receipts(&mut messages, &mut receipts.get_all());
    }

    // update senders to friendly names
    messages.iter_mut().for_each(|m| m.update_senders(members));

    // merge all the reactions
    for m in messages.iter_mut() {
        m.merge_reactions();
    }

    // our message list is reversed because we start at the bottom of the
    // window and move up, like any good chat
    messages.reverse();

    // interleave the state event groups, newest first
    let mut items = Vec::with_capacity(messages.len() + groups.len());
    let mut groups = groups.into_iter().rev().peekable();

    for message in messages {
        while groups.peek().is_some_and(|g| g.ts > *message.sort_order()) {
            push_group(&mut items, groups.next().unwrap());
        }

        items.push(TimelineItem::Message(message));
    }

    for group in groups {
        push_group(&mut items, group);
    }

    items
}

const REPLY_TEMPLATE: &str = "<!--
  Replying to:

{}
-->";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::message::tests::{reaction_event, text_event, thread_event};
    use matrix_sdk::ruma::{owned_event_id, owned_user_id};

    fn items(events: Vec<AnyTimelineEvent>, thread_root: Option<&EventId>) -> Vec<TimelineItem> {
        let timeline: BTreeSet<OrderedEvent> = events.into_iter().map(OrderedEvent::new).collect();
        let receipts = Receipts::new(owned_user_id!("@me:example.org"));

        make_message_list(&timeline, &vec![], &receipts, "", thread_root)
    }

    #[test]
    fn room_mode_groups_threads_under_the_root() {
        let items = items(
            vec![
                text_event("$root:example.org", 1000, "root"),
                text_event("$other:example.org", 1500, "other"),
                thread_event("$t1:example.org", 2000, "$root:example.org", None),
            ],
            None,
        );

        assert_eq!(items.len(), 2);

        // the root bounced to the bottom (newest first)
        let root = items[0].message().unwrap();
        assert_eq!(root.id.as_str(), "$root:example.org");
        assert_eq!(root.thread.len(), 1);
    }

    #[test]
    fn thread_mode_shows_only_the_thread() {
        let root_id = owned_event_id!("$root:example.org");

        let items = items(
            vec![
                text_event("$root:example.org", 1000, "root"),
                text_event("$other:example.org", 1500, "other"),
                thread_event("$t1:example.org", 2000, "$root:example.org", None),
                thread_event(
                    "$t2:example.org",
                    3000,
                    "$root:example.org",
                    Some("$t1:example.org"),
                ),
                reaction_event("$r1:example.org", 4000, "$t1:example.org", "👍"),
            ],
            Some(&root_id),
        );

        // newest first: t1 (with t2 nested and a reaction), then the root;
        // the unrelated message is gone
        assert_eq!(items.len(), 2);

        let newest = items[0].message().unwrap();
        assert_eq!(newest.id.as_str(), "$t1:example.org");
        assert_eq!(newest.replies.len(), 1);
        assert_eq!(newest.replies[0].id.as_str(), "$t2:example.org");
        assert_eq!(newest.reactions.len(), 1);

        assert_eq!(items[1].message().unwrap().id.as_str(), "$root:example.org");
    }

    fn member(kind: MemberKind, name: &str) -> StateEntry {
        StateEntry::Member(kind, name.to_string())
    }

    fn flush(entries: Vec<StateEntry>) -> Vec<String> {
        let mut pending = PendingEvents::default();

        for entry in entries {
            pending.push(entry, MilliSecondsSinceUnixEpoch::now());
        }

        pending.flush().unwrap().lines
    }

    #[test]
    fn combines_runs_of_the_same_kind() {
        let lines = flush(vec![
            member(MemberKind::Joined, "Bob"),
            member(MemberKind::Joined, "Alice"),
            member(MemberKind::Left, "Jeff"),
        ]);

        assert_eq!(
            lines,
            vec!["Bob and Alice joined the room.", "Jeff left the room."]
        );
    }

    #[test]
    fn dedups_names_and_counts_overflow() {
        let lines = flush(vec![
            member(MemberKind::Left, "Bob"),
            member(MemberKind::Left, "Bob"),
            member(MemberKind::Left, "Alice"),
            member(MemberKind::Left, "Jeff"),
            member(MemberKind::Left, "Carol"),
            member(MemberKind::Left, "Dave"),
        ]);

        assert_eq!(lines, vec!["Bob, Alice, Jeff and 2 others left the room."]);
    }

    #[test]
    fn standalone_lines_break_runs() {
        let lines = flush(vec![
            member(MemberKind::Joined, "Bob"),
            StateEntry::Line("Alice changed the topic to Fun.".to_string()),
            member(MemberKind::Joined, "Jeff"),
        ]);

        assert_eq!(
            lines,
            vec![
                "Bob joined the room.",
                "Alice changed the topic to Fun.",
                "Jeff joined the room.",
            ]
        );
    }

    #[test]
    fn singular_verbs_agree() {
        assert_eq!(
            member_line(MemberKind::Kicked, vec!["Bob".to_string()]),
            "Bob was kicked from the room."
        );
    }
}
