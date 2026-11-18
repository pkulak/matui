mod state_events;

pub use state_events::RoomEvents;
use state_events::{PendingEvents, push_group};

use crate::app::Popup;
use crate::event::{Event, EventHandler};
use crate::matrix::Matrix;
use crate::matrix::roomcache::DecoratedRoom;
use crate::pictures::{PictureSource, Pictures, picker};
use crate::selection::{self, Action, BodyRow, Selection, TextMap};
use crate::settings::{is_thread_muted, max_events};
use crate::spawn::{get_file_paths, spawn_editor};
use crate::widgets::EventResult::Consumed;
use crate::widgets::command::Command;
use crate::widgets::compose::Compose;
use crate::widgets::members::Members;
use crate::widgets::message::{LineType, Message, Reaction, Receipt, local_time};
use crate::widgets::react::React;
use crate::widgets::react::ReactResult;
use crate::widgets::search::Search;
use crate::widgets::timeline::Timeline;
use crate::widgets::upload::Upload;
use crate::widgets::{EventResult, get_margin};
use crate::{KeyCombo, consumed, limit_list, pretty_list, truncate};
use anyhow::bail;
use chrono::{Local, NaiveDate};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use image::DynamicImage;
use log::info;
use matrix_sdk::RoomMemberships;
use matrix_sdk::notification_settings::RoomNotificationMode::{MentionsAndKeywordsOnly, Mute};
use matrix_sdk::room::{Room, RoomMember};
use matrix_sdk::ruma::events::room::message::MessageType::Text;
use matrix_sdk::ruma::events::room::power_levels::UserPowerLevel;
use matrix_sdk::ruma::{
    EventId, MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedMxcUri, OwnedUserId, RoomId, UserId,
};
use matrix_sdk_ui::eyeball_im::Vector;
use matrix_sdk_ui::timeline::{
    Timeline as SdkTimeline, TimelineEventItemId, TimelineItem as SdkItem, TimelineItemContent,
    VirtualTimelineItem,
};
use once_cell::sync::OnceCell;
use std::cell::{Cell, RefCell};
use std::collections::{BinaryHeap, HashSet};
use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::AbortHandle;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, ListItem, ListState, Paragraph, Widget};

use super::confirm::{Confirm, ConfirmBehavior};

/// How far paging back through the timeline has got.
enum Paging {
    /// The room's timeline isn't built yet.
    Opening,
    Idle,
    Fetching(AbortHandle),
    AtStart,
    /// The last page failed. Drawing doesn't retry it, or every frame would;
    /// scrolling up does.
    Failed,
}

pub struct Chat {
    matrix: Matrix,
    room: DecoratedRoom,
    timeline: Option<Arc<SdkTimeline>>,
    items: Vector<Arc<SdkItem>>,
    messages: Vec<TimelineItem>,
    window: Range<usize>,
    read_to: Option<OwnedEventId>,
    /// Where the new messages start, as the room's read marker stood until
    /// the chat marked the room read, which moves it to the end, and until
    /// we send a message.
    read_marker: Option<MilliSecondsSinceUnixEpoch>,
    react: Option<React>,
    typing: Option<String>,
    copied: Option<Instant>,
    list_state: Cell<ListState>,
    max_events: usize,
    paging: RefCell<Paging>,
    follow: Option<AbortHandle>,
    line_types: Mutex<Vec<LineType>>,
    /// The message text on screen as of the last draw, and the list's area.
    text_map: RefCell<TextMap<TimelineEventItemId>>,
    selection: Selection<TimelineEventItemId>,
    pictures: RefCell<Pictures>,
    bookmark: Cell<Option<Bookmark>>,
    search_term: String,
    thread_root: Option<OwnedEventId>,
    delete_combo: KeyCombo,

    members: Vec<RoomMember>,
    pretty_members: OnceCell<String>,
    /// Everyone whose member info we've fetched, or are fetching.
    checked: HashSet<OwnedUserId>,
}

// stop following and paging a room nobody is looking at
impl Drop for Chat {
    fn drop(&mut self) {
        if let Paging::Fetching(handle) = self.paging.get_mut() {
            handle.abort();
        }

        if let Some(handle) = &self.follow {
            handle.abort();
        }
    }
}

impl Chat {
    pub fn try_new(matrix: Matrix, room: Room) -> Option<Self> {
        let decorated_room = matrix.wrap_room(&room)?;

        let follow = matrix.follow_timeline(room);

        Some(Self::new(matrix, decorated_room, follow, max_events()))
    }

    fn new(matrix: Matrix, room: DecoratedRoom, follow: AbortHandle, max_events: usize) -> Self {
        Self {
            matrix,
            room,
            timeline: None,
            items: Vector::new(),
            messages: vec![],
            window: (0..0),
            read_to: None,
            read_marker: None,
            react: None,
            typing: None,
            copied: None,
            list_state: Cell::new(ListState::default()),
            max_events,
            paging: RefCell::new(Paging::Opening),
            follow: Some(follow),
            line_types: Mutex::new(vec![]),
            text_map: RefCell::new(TextMap::new(Rect::default())),
            selection: Selection::default(),
            pictures: RefCell::default(),
            bookmark: Cell::new(Option::None),
            search_term: "".to_string(),
            thread_root: None,
            delete_combo: KeyCombo::new(vec!['d', 'd']),
            members: vec![],
            pretty_members: OnceCell::new(),
            checked: HashSet::new(),
        }
    }

    /// A new chat viewing a single thread in this room, on its own timeline.
    pub fn thread(&self, root: OwnedEventId) -> anyhow::Result<Self> {
        let timeline = self.matrix.thread_timeline(&self.room(), root.clone())?;
        let follow = self.matrix.follow(timeline.clone());

        let mut chat = Self::new(
            self.matrix.clone(),
            self.room.clone(),
            follow,
            self.max_events,
        );
        chat.timeline = Some(timeline);
        chat.thread_root = Some(root);
        chat.paging = RefCell::new(Paging::Idle);
        chat.members = self.members.clone();
        chat.checked = self.checked.clone();

        // the root comes once paging reaches the start of the thread
        chat.try_fetch_previous();
        Ok(chat)
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        self.widget().render(area, buf);
    }

    pub fn widget(&self) -> ChatWidget<'_> {
        ChatWidget { chat: self }
    }

    pub fn reacting(&self) -> bool {
        self.react.is_some()
    }

    pub fn paste_event(&mut self, value: &str) -> EventResult {
        let Some(react) = &mut self.react else {
            return EventResult::Ignored;
        };

        react.paste_event(value);
        consumed!()
    }

    pub fn key_event(
        &mut self,
        input: &KeyEvent,
        handler: &EventHandler,
    ) -> anyhow::Result<EventResult> {
        self.selection.clear();

        // give our reaction window first dibs
        if let Some(react) = &mut self.react {
            match react.key_event(input) {
                ReactResult::Exit => {
                    self.react = None;
                    return Ok(consumed!());
                }
                ReactResult::SelectReaction(reaction) | ReactResult::RemoveReaction(reaction) => {
                    self.react = None;

                    if let Some(message) = self.selected_reply()
                        && let Some(timeline) = &self.timeline
                    {
                        self.matrix
                            .toggle_reaction(timeline.clone(), message.id.clone(), reaction)
                    }

                    return Ok(consumed!());
                }
                ReactResult::Consumed => return Ok(consumed!()),
            }
        }

        // then look for key combos
        if let KeyCode::Char(c) = input.code
            && input.modifiers.is_empty()
            && self.delete_combo.record(c)
        {
            let (Some(message), Some(timeline)) = (self.selected_reply(), &self.timeline) else {
                return Ok(EventResult::Ignored);
            };

            let preview = truncate(message.display().to_string(), 16);
            let warning = format!("Are you sure you want to delete \"{}\"", preview);

            let confirm = Confirm::new(
                "Delete Message".to_string(),
                warning,
                "Yes".to_string(),
                "No".to_string(),
                ConfirmBehavior::DeleteMessage(timeline.clone(), message.id.clone()),
            );

            return Ok(Consumed(Box::new(|app| {
                app.open_popup(Popup::Confirm(confirm))
            })));
        }

        match input.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.previous(1);
                Ok(consumed!())
            }
            KeyCode::Char('d') if input.modifiers.contains(KeyModifiers::CONTROL) => {
                self.previous(self.half_page());
                Ok(consumed!())
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.next(1);
                self.try_fetch_previous();
                Ok(consumed!())
            }
            KeyCode::Char('u') if input.modifiers.contains(KeyModifiers::CONTROL) => {
                self.next(self.half_page());
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
                    if self.thread_root.is_none()
                        && message.thread_replies > 0
                        && let Some(id) = message.event_id()
                    {
                        let thread = self.thread(id.clone())?;

                        return Ok(Consumed(Box::new(move |app| app.open_thread(thread))));
                    }

                    message.open(self.matrix.clone())
                }
                Ok(consumed!())
            }
            KeyCode::Esc if self.thread_root.is_some() => {
                Ok(Consumed(Box::new(|app| app.close_thread())))
            }
            KeyCode::Char('s') => {
                if let Some(message) = &self.selected_reply() {
                    message.save(self.matrix.clone())
                }
                Ok(consumed!())
            }
            KeyCode::Char('y') => {
                if let Some(text) = self.copy_text() {
                    selection::copy(text)?;
                    self.copied = Some(Instant::now());
                }
                Ok(consumed!())
            }
            KeyCode::Char('c') => {
                let (Some(message), Some(timeline)) = (self.selected_reply(), &self.timeline)
                else {
                    return Ok(EventResult::Ignored);
                };

                // only a sent message can be edited
                let Some(id) = message.event_id() else {
                    return Ok(EventResult::Ignored);
                };

                if matches!(message.body, Text(_)) {
                    let original = message.editable();

                    let result = spawn_editor(
                        handler,
                        None,
                        Some(&original),
                        Some(&format!(
                            "<!-- Edit your message above to change it in {}. -->",
                            self.room.name
                        )),
                    );

                    if let Ok(edit) = result {
                        if let Some(edit) = edit {
                            // Unchanged text means the editor was closed without saving.
                            if edit == original {
                                return Ok(consumed!());
                            }

                            self.matrix
                                .replace_event(timeline.clone(), id.clone(), edit);

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
                let Some(timeline) = self.timeline.clone() else {
                    return Ok(EventResult::Ignored);
                };

                let room = self.room.clone();
                let matrix = self.matrix.clone();

                Ok(Consumed(Box::new(|app| {
                    app.open_popup(Popup::Compose(Compose::new(room, timeline, matrix, "")))
                })))
            }
            KeyCode::Char('@') => {
                let Some(timeline) = self.timeline.clone() else {
                    return Ok(EventResult::Ignored);
                };

                // normally a local store read, like V
                let members = self
                    .matrix
                    .handle()
                    .block_on(async { self.room().members(RoomMemberships::JOIN).await })?;

                let members = Members::new(
                    self.room.clone(),
                    timeline,
                    self.matrix.clone(),
                    members,
                    &self.matrix.me(),
                );

                Ok(Consumed(Box::new(|app| {
                    app.open_popup(Popup::Members(members))
                })))
            }
            KeyCode::Char('I') => {
                let Some(timeline) = self.timeline.clone() else {
                    return Ok(EventResult::Ignored);
                };

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
                    self.matrix.send_text_message(timeline, message);
                }

                Ok(consumed!())
            }
            KeyCode::Char('R') => {
                let (Some(message), Some(timeline)) = (self.selected_reply(), &self.timeline)
                else {
                    return Ok(consumed!());
                };

                // only a sent message can be replied to
                let Some(id) = message.event_id() else {
                    return Ok(consumed!());
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
                        self.matrix.send_reply(timeline.clone(), input, id.clone());
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

                let Some(id) = self.selected_reply().and_then(Message::event_id) else {
                    return Ok(EventResult::Ignored);
                };

                // open the thread view, empty or not; sending the first
                // message is what actually creates the thread
                let thread = self.thread(id.clone())?;

                Ok(Consumed(Box::new(move |app| app.open_thread(thread))))
            }
            KeyCode::Char('v') => {
                let message = match self.selected_reply() {
                    Some(m) => m,
                    None => return Ok(EventResult::Ignored),
                };

                let history = match (&self.timeline, message.event_id()) {
                    (Some(timeline), Some(id)) if message.edited => {
                        self.matrix.edit_history(timeline, id)
                    }
                    _ => vec![],
                };

                spawn_editor(handler, None, Some(&message.display_full(&history)), None)?;

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
                let Some(timeline) = self.timeline.clone() else {
                    return Ok(EventResult::Ignored);
                };

                let paths = get_file_paths(self.matrix.handle())?;

                handler.sender().send(Event::Redraw)?;

                if paths.is_empty() {
                    return Ok(EventResult::Ignored);
                }

                let matrix = self.matrix.clone();

                Ok(Consumed(Box::new(move |app| {
                    app.open_popup(Popup::Upload(Upload::new(matrix, timeline, paths)))
                })))
            }
            KeyCode::Char('/') => Ok(Consumed(Box::new(|app| {
                app.open_popup(Popup::Search(Search::default()))
            }))),
            KeyCode::Char(':') => Ok(Consumed(Box::new(|app| {
                app.open_popup(Popup::Command(Command::default()))
            }))),
            KeyCode::Char('U') => {
                let message = match self.selected_reply() {
                    Some(m) => m,
                    None => return Ok(EventResult::Ignored),
                };

                let sender = message.sender.id.clone();

                Ok(Consumed(Box::new(move |app| {
                    app.open_popup(Popup::Command(Command::with_user(&sender)))
                })))
            }
            _ => Ok(EventResult::Ignored),
        }
    }

    /// A press, drag or release of the left button. A press moves the cursor
    /// to it, and the selection does the rest.
    pub fn mouse_event(&mut self, event: &MouseEvent) -> Option<Action<TimelineEventItemId>> {
        if event.kind == MouseEventKind::Down(MouseButton::Left) {
            self.move_to(event.column, event.row);
        }

        let action = self
            .selection
            .mouse_event(&self.text_map.borrow(), event, Instant::now());

        // the caller copies it, and the header says so, as for y
        if let Some(Action::Copy(_)) = action {
            self.copied = Some(Instant::now());
        }

        action
    }

    /// Put the cursor on the list row at the pointer, or on the first row
    /// above it that it can rest on. Above the oldest row, that's the oldest
    /// row.
    fn move_to(&mut self, x: u16, y: u16) {
        let area = self.text_map.borrow().area;

        if !area.contains(Position::new(x, y)) {
            return;
        }

        let mut state = self.list_state.take();
        state.select(Some(state.offset() + usize::from(area.bottom() - 1 - y)));
        self.list_state.set(state);
        self.next(0);
    }

    /// Open a message, as Enter does, without going into its thread.
    pub fn open_message(&self, id: &TimelineEventItemId) {
        if let Some(message) = self.iter_messages().find_map(|m| m.find_by_id(id)) {
            message.open(self.matrix.clone());
        }
    }

    fn half_page(&self) -> usize {
        (self.text_map.borrow().area.height / 2).into()
    }

    /// Whether the chat needs drawing again, because the copy notice has
    /// had its time.
    pub fn tick_event(&mut self) -> bool {
        if self.copied.is_some_and(|at| at.elapsed() >= COPIED_FOR) {
            self.copied = None;
            return true;
        }

        false
    }

    pub fn focus_event(&mut self) {
        self.mark_read();
    }

    /// A timeline changed, to these items. The room chat takes the room's
    /// first timeline that isn't a thread's; a thread chat has its own.
    pub fn items_event(&mut self, timeline: &Arc<SdkTimeline>, items: &Vector<Arc<SdkItem>>) {
        let ours = match &self.timeline {
            Some(own) => Arc::ptr_eq(own, timeline),
            None => timeline.room().room_id() == self.room.room_id() && !timeline.is_threaded(),
        };

        if !ours {
            return;
        }

        let reset = self.messages.is_empty();

        self.timeline = Some(timeline.clone());
        self.items = items.clone();

        if self.read_to.is_none() {
            self.read_marker = read_marker(items);
        }

        // sending a message after it is done with what's new
        let me = self.matrix.me();

        if let Some(read) = self.read_marker
            && items
                .iter()
                .filter_map(|i| i.as_event())
                .any(|e| e.sender() == me && e.timestamp() > read)
        {
            self.read_marker = None;
        }

        self.check_item_senders();
        self.set_messages(false);
        self.pretty_members = OnceCell::new();
        self.mark_read();

        if reset {
            let mut state = self.list_state.take();
            state.select(Some(0));
            self.list_state.set(state);
        }

        // done loading; drawing it pages back until there's enough to show
        let paging = self.paging.get_mut();

        if matches!(paging, Paging::Opening) {
            *paging = Paging::Idle;
        }
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

    /// A timeline paged back, reaching its start or not, or failed to.
    pub fn paginated_event(&mut self, timeline: &Arc<SdkTimeline>, result: &Result<bool, String>) {
        if !self
            .timeline
            .as_ref()
            .is_some_and(|t| Arc::ptr_eq(t, timeline))
        {
            return;
        }

        *self.paging.get_mut() = match result {
            Ok(true) => Paging::AtStart,
            Ok(false) => Paging::Idle,
            Err(_) => Paging::Failed,
        };
    }

    /// A timeline may have lost its earlier events, so page back to them
    /// again.
    pub fn not_at_start_event(&mut self, timeline: &Arc<SdkTimeline>) {
        if self
            .timeline
            .as_ref()
            .is_some_and(|t| Arc::ptr_eq(t, timeline))
        {
            let paging = self.paging.get_mut();

            if matches!(paging, Paging::AtStart) {
                *paging = Paging::Idle;
            }
        }
    }

    pub fn picture_event(
        &mut self,
        room_id: &RoomId,
        uri: OwnedMxcUri,
        image: Option<DynamicImage>,
    ) {
        if room_id != self.room.room_id() {
            return;
        }

        self.pictures.get_mut().insert(uri, image);

        // the picture's rows may push the selection onto another message,
        // so keep it where it was
        self.adjust_range(false);
    }

    fn fetch_pictures(&self, sources: Vec<PictureSource>) {
        for source in sources {
            self.matrix
                .fetch_picture(self.room.room_id().to_owned(), source);
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

    fn check_item_senders(&mut self) {
        let items = self.items.clone();

        for event in items.iter().filter_map(|i| i.as_event()) {
            self.check_sender(event.sender());

            for id in event.read_receipts().keys() {
                self.check_sender(id);
            }

            // member events also have a target
            if let TimelineItemContent::MembershipChange(change) = event.content() {
                self.check_sender(change.user_id());
            }
        }
    }

    fn set_messages(&mut self, force_bookmark: bool) {
        self.messages = mark_days(make_message_list(
            &self.items,
            &self.members,
            &self.search_term,
            self.thread_root.as_deref(),
        ));

        // the read marker goes above everything newer than it, newest first,
        // and under their day's marker
        if let Some(read) = self.read_marker.filter(|_| self.search_term.is_empty())
            && let Some(mut i) = self.messages.iter().position(|item| match item {
                TimelineItem::Message(m) => *m.sort_order() <= read,
                TimelineItem::Events(e) => e.ts <= read,
                TimelineItem::ReadMarker | TimelineItem::DayMarker(_) => false,
            })
            && i > 0
        {
            if matches!(self.messages[i - 1], TimelineItem::DayMarker(_)) {
                i -= 1;
            }

            self.messages.insert(i, TimelineItem::ReadMarker);
        }

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

    fn check_sender(&mut self, user_id: &UserId) {
        if !self.checked.contains(user_id) {
            self.checked.insert(user_id.to_owned());
            self.matrix
                .fetch_room_member(self.room(), user_id.to_owned());
            info!("fetching {}", user_id);
        }
    }

    fn muted(&self) -> bool {
        match &self.thread_root {
            Some(root) => is_thread_muted(root),
            // the room's own setting, which sync keeps up to date
            None => matches!(
                self.room.inner.cached_user_defined_notification_mode(),
                Some(Mute | MentionsAndKeywordsOnly)
            ),
        }
    }

    /// Mark the room's main timeline read, or the thread's.
    fn mark_read(&mut self) {
        if !self
            .matrix
            .sees(self.room.room_id(), self.thread_root.as_deref())
        {
            return;
        }

        let read_to = self.iter_messages().find_map(|m| m.event_id().cloned());

        if read_to == self.read_to {
            return;
        }

        if let (Some(id), Some(timeline)) = (read_to.clone(), &self.timeline) {
            self.matrix.read_to(timeline.clone(), id);
            self.read_to = read_to;
        }
    }

    fn display_full(&self) -> anyhow::Result<String> {
        // normally a local store read; the app syncs the full member
        // list the first time a room's senders are resolved
        let mut members = self
            .matrix
            .handle()
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

    /// The root of the thread this chat views, if it views one.
    pub fn thread_root(&self) -> Option<&EventId> {
        self.thread_root.as_deref()
    }

    /// The room's timeline, once it's loaded.
    pub fn timeline(&self) -> Option<Arc<SdkTimeline>> {
        self.timeline.clone()
    }

    fn pretty_members(&self) -> &str {
        self.pretty_members.get_or_init(|| {
            let mut members: Vec<&RoomMember> = vec![];

            // first grab folks who have sent read receipts
            let mut receipts = receipts(&self.items);

            while let Some(receipt) = receipts.pop() {
                if let Some(member) = self.members.iter().find(|m| m.user_id() == receipt.user_id) {
                    members.push(member);
                }
            }

            // then walk all the events backwards, until we have a decent number
            for event in self.items.iter().rev().filter_map(|i| i.as_event()) {
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

        self.members.push(member);
        self.pretty_members = OnceCell::new();
        self.set_messages(false);
    }

    fn try_fetch_previous(&self) {
        let Some(timeline) = &self.timeline else {
            return;
        };

        if !matches!(*self.paging.borrow(), Paging::Idle) {
            return;
        }

        let count = self.event_count();

        let state = self.list_state.take();
        let buffer = self.total_list_items() - state.selected().unwrap_or_default();
        self.list_state.set(state);

        if buffer < 100 && count < self.max_events {
            let limit = if self.search_term.is_empty() { 32 } else { 256 };

            let fetch = self.matrix.paginate(timeline.clone(), limit);
            *self.paging.borrow_mut() = Paging::Fetching(fetch);
            info!("fetching more events... {}", count)
        }
    }

    fn event_count(&self) -> usize {
        self.items.iter().filter(|i| i.as_event().is_some()).count()
    }

    fn next(&mut self, step: usize) {
        // scrolling up retries a page that failed
        let paging = self.paging.get_mut();

        if matches!(paging, Paging::Failed) {
            *paging = Paging::Idle;
        }

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

    // the message (or reply) currently selected by the UI
    fn selected_reply(&self) -> Option<&Message> {
        let bookmark = self.get_bookmark()?;
        self.iter_messages()
            .find_map(|m| m.find_by_id(&bookmark.message_id))
    }

    /// What `y` copies: the whole text of the message under the cursor,
    /// including lines past `message_line_limit`.
    fn copy_text(&self) -> Option<&str> {
        self.selected_reply().map(Message::display)
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

        ret.retain(|r| r.senders.iter().any(|s| s.id == me));
        ret
    }
}

pub struct Bookmark {
    // which message are we pointing at?
    message_id: TimelineEventItemId,

    // how many lines down from the top?
    offset: usize,

    // and how offset is the bottom of the window?
    window_offset: usize,
}

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
            .constraints([Constraint::Percentage(100)])
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .vertical_margin(1)
            .constraints([Constraint::Length(3), Constraint::Percentage(100)])
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

        let mut line_types = self.chat.line_types.lock().unwrap();
        line_types.clear();

        let window = &self.chat.messages[self.chat.window.clone()];
        let mut pictures = self.chat.pictures.borrow_mut();

        // the cursor's two columns on the left, and two to match on the
        // right, so the chat sits centred under the header
        let timeline = Timeline::new(
            window,
            (area.width - 4) as usize,
            &mut line_types,
            &mut pictures,
            picker(),
        );

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

        let (fetches, map) = timeline.render(splits[1], buf, &mut list_state);
        selection::paint(buf, &map, &self.chat.selection);
        self.chat.text_map.replace(map);
        self.chat.list_state.set(list_state);
        self.chat.fetch_pictures(fetches);

        // page back here, where it's known how much is above the selection
        drop(line_types);
        self.chat.try_fetch_previous();

        // after paging, so a search shows as searching between its pages
        let p_area = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(2)
            .vertical_margin(1)
            .constraints([Constraint::Percentage(100)])
            .split(splits[0])[0];

        let (p_content, p_color) = if self.chat.copied.is_some() {
            ("Copied!".to_string(), Color::Green)
        } else if let Some(typing) = &self.chat.typing {
            (typing.to_string(), Color::Yellow)
        } else if matches!(
            *self.chat.paging.borrow(),
            Paging::Opening | Paging::Fetching(_)
        ) {
            let term = if self.chat.search_term.is_empty() {
                "Loading"
            } else {
                "Searching"
            };

            (
                format!("{}... ({})", term, self.chat.event_count()),
                Color::Yellow,
            )
        } else if matches!(*self.chat.paging.borrow(), Paging::Failed) {
            (
                "Couldn't load earlier messages. Scroll up to retry.".to_string(),
                Color::Red,
            )
        } else {
            (self.chat.pretty_members().to_string(), Color::Magenta)
        };

        Paragraph::new(p_content)
            .style(Style::default().fg(p_color))
            .render(p_area, buf);

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
    /// The line above the new messages.
    ReadMarker,
    /// The line above a day's first item.
    DayMarker(NaiveDate),
}

impl TimelineItem {
    pub(crate) fn message(&self) -> Option<&Message> {
        match self {
            TimelineItem::Message(m) => Some(m),
            TimelineItem::Events(_) | TimelineItem::ReadMarker | TimelineItem::DayMarker(_) => None,
        }
    }

    // a reply chain's is the day of its latest reply
    fn day(&self) -> Option<NaiveDate> {
        let ts = match self {
            TimelineItem::Message(m) => *m.sort_order(),
            TimelineItem::Events(e) => e.ts,
            TimelineItem::ReadMarker | TimelineItem::DayMarker(_) => return None,
        };

        Some(local_time(ts).date_naive())
    }

    pub fn to_list_items<'a>(
        &'a self,
        width: usize,
        picture_rows: Option<u16>,
        sidecar: &mut Vec<LineType>,
        bodies: &mut Vec<Option<BodyRow<'a, TimelineEventItemId>>>,
    ) -> Vec<ListItem<'a>> {
        match self {
            TimelineItem::Message(m) => m.to_list_items(width, picture_rows, sidecar, bodies),
            TimelineItem::Events(e) => {
                let items = e.to_list_items(sidecar);
                bodies.extend(items.iter().map(|_| None));
                items
            }
            TimelineItem::ReadMarker => {
                let line = divider_line("new messages", width, Color::DarkGray);
                divider(line, sidecar, bodies)
            }
            TimelineItem::DayMarker(day) => divider(day_line(*day, width), sidecar, bodies),
        }
    }
}

/// A divider's rows: its line, with a blank row above.
fn divider<'a>(
    line: Line<'a>,
    sidecar: &mut Vec<LineType>,
    bodies: &mut Vec<Option<BodyRow<'a, TimelineEventItemId>>>,
) -> Vec<ListItem<'a>> {
    // bottom up, like the others
    sidecar.extend([LineType::RoomEvent, LineType::DeadSpace]);
    bodies.extend([None, None]);

    vec![ListItem::new(line), ListItem::new(" ")]
}

/// A gray line across the chat, with a label in the middle.
fn divider_line(label: &str, width: usize, label_color: Color) -> Line<'static> {
    let label = format!(" {label} ");
    let dashes = width.saturating_sub(label.chars().count());
    let gray = Style::default().fg(Color::DarkGray);

    Line::from(vec![
        Span::styled("─".repeat(dashes / 2), gray),
        Span::styled(label, Style::default().fg(label_color)),
        Span::styled("─".repeat(dashes - dashes / 2), gray),
    ])
}

/// A day marker's line, with the day lighter than the line.
pub(crate) fn day_line(day: NaiveDate, width: usize) -> Line<'static> {
    divider_line(
        &label_day(day, Local::now().date_naive()),
        width,
        Color::Gray,
    )
}

/// What a day's marker says, as in Element: Today, Yesterday, the weekday
/// for the rest of the week, then the full date.
fn label_day(day: NaiveDate, today: NaiveDate) -> String {
    match (today - day).num_days() {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        2..=6 => day.format("%A").to_string(),
        _ => day.format("%a, %b %-d, %Y").to_string(),
    }
}

/// Puts a marker above each day's first item. The items are newest first.
fn mark_days(items: Vec<TimelineItem>) -> Vec<TimelineItem> {
    let mut marked = Vec::with_capacity(items.len());
    let mut newer: Option<NaiveDate> = None;

    for item in items {
        if let Some(day) = item.day() {
            if let Some(newer) = newer.filter(|&newer| newer != day) {
                marked.push(TimelineItem::DayMarker(newer));
            }

            newer = Some(day);
        }

        marked.push(item);
    }

    marked.extend(newer.map(TimelineItem::DayMarker));
    marked
}

/// When the last event before the timeline's read marker was sent.
fn read_marker(items: &Vector<Arc<SdkItem>>) -> Option<MilliSecondsSinceUnixEpoch> {
    let marker = items
        .iter()
        .position(|i| matches!(i.as_virtual(), Some(VirtualTimelineItem::ReadMarker)))?;

    items
        .iter()
        .take(marker)
        .rev()
        .find_map(|i| i.as_event())
        .map(|e| e.timestamp())
}

/// How far each user has read, from the receipts on the timeline's items.
fn receipts(items: &Vector<Arc<SdkItem>>) -> BinaryHeap<Receipt> {
    items
        .iter()
        .filter_map(|i| i.as_event())
        .flat_map(|event| {
            event.read_receipts().keys().map(|user_id| Receipt {
                read_to: event.timestamp(),
                user_id: user_id.clone(),
            })
        })
        .collect()
}

pub(crate) fn make_message_list(
    items: &Vector<Arc<SdkItem>>,
    members: &Vec<RoomMember>,
    search_term: &str,
    thread_root: Option<&EventId>,
) -> Vec<TimelineItem> {
    let mut messages = vec![];
    let mut groups: Vec<RoomEvents> = vec![];
    let mut pending = PendingEvents::default();

    for event in items.iter().filter_map(|i| i.as_event()) {
        let Some(message) = Message::from_item(event) else {
            if thread_root.is_none() && search_term.is_empty() {
                pending.push_event(event, members);
            }

            continue;
        };

        // the thread view shows the root, then its messages, like a room,
        // with the root's reply count left to the room
        if thread_root.is_some() {
            let mut message = message;
            message.thread_replies = 0;
            messages.push(message);
            continue;
        }

        // the room's timeline leaves out its threads' messages, except
        // those it can't decrypt yet
        if message.thread_root.is_some() {
            continue;
        }

        // replies go under the message they're for, if it's here
        let message = match message.in_reply_to.clone() {
            Some(target) => Message::nest(&mut messages, message, &target, 0),
            None => Some(message),
        };

        if let Some(message) = message {
            groups.extend(pending.flush());
            messages.push(message);
        }
    }

    groups.extend(pending.flush());

    if !search_term.is_empty() {
        messages.retain(|m| m.contains_search_term(search_term));
        messages.iter().for_each(|m| m.set_search_term(search_term));
    }

    // receipts mark room positions, not thread ones
    if thread_root.is_none() {
        Message::apply_receipts(&mut messages, &mut receipts(items));
    }

    // update senders to friendly names
    messages.iter_mut().for_each(|m| m.update_senders(members));

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

/// How long the header says a message was copied.
const COPIED_FOR: Duration = Duration::from_secs(3);

const REPLY_TEMPLATE: &str = "<!--
  Replying to:

{}
-->";

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::handler::MatuiEvent;
    use crate::selection::tests::{at, drag_to, end, highlighted, press, release};
    use crate::widgets::message::Sending;
    use crate::widgets::message::tests::{reaction_event, reply_event, text_event, thread_event};
    use matrix_sdk::Client;
    use matrix_sdk::ruma::api::client::receipt::create_receipt::v3::ReceiptType as SendReceiptType;
    use matrix_sdk::ruma::events::receipt::{ReceiptThread, ReceiptType};
    use matrix_sdk::ruma::events::room::member::RoomMemberEvent;
    use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
    use matrix_sdk::ruma::exports::serde_json::{Value, json};
    use matrix_sdk::ruma::serde::Raw;
    use matrix_sdk::ruma::{RoomVersionId, event_id, owned_event_id, room_id, user_id};
    use matrix_sdk::send_queue::RoomSendQueueUpdate;
    use matrix_sdk::test_utils::mocks::{
        MatrixMockServer, RoomMessagesResponseTemplate, RoomRelationsResponseTemplate,
    };
    use matrix_sdk_test::JoinedRoomBuilder;
    use matrix_sdk_test::event_factory::EventFactory;
    use matrix_sdk_ui::timeline::RoomExt;
    use matrix_sdk_ui::timeline::{TimelineFocus, TimelineReadReceiptTracking};
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Duration;
    use tokio::runtime::Handle;
    use wiremock::ResponseTemplate;

    fn room_id() -> &'static RoomId {
        room_id!("!room:example.org")
    }

    fn raw<T>(events: Vec<Value>) -> Vec<Raw<T>> {
        events
            .into_iter()
            .map(|e| Raw::new(&e).unwrap().cast_unchecked())
            .collect()
    }

    /// A client on a mock server, in an empty room.
    pub(crate) async fn joined() -> (MatrixMockServer, Client, Room) {
        let server = MatrixMockServer::new().await;
        server.mock_room_state_encryption().plain().mount().await;

        let client = server.client_builder().build().await;
        client.event_cache().subscribe().unwrap();

        let room = server.sync_joined_room(&client, room_id()).await;
        (server, client, room)
    }

    /// Sync these events into the room, oldest first.
    pub(crate) async fn sync(server: &MatrixMockServer, client: &Client, events: Vec<Value>) {
        let room = JoinedRoomBuilder::new(room_id()).add_timeline_bulk(raw(events));
        server.sync_room(client, room).await;
    }

    /// The items of a room's timeline, once it has these events.
    pub(crate) async fn timeline_items(events: Vec<Value>) -> Vector<Arc<SdkItem>> {
        let (server, client, room) = joined().await;
        let focus = TimelineFocus::Live {
            hide_threaded_events: true,
        };
        // without receipts, which would add a line under each sender's
        // newest message
        let timeline = room
            .timeline_builder()
            .with_focus(focus)
            .track_read_marker_and_receipts(TimelineReadReceiptTracking::Disabled)
            .build()
            .await
            .unwrap();

        sync(&server, &client, events.clone()).await;

        tokio::time::timeout(Duration::from_secs(5), async {
            while !shows(&timeline, &events).await {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the timeline never caught up");

        timeline.items().await
    }

    /// The root of the thread the event is in, from its relation.
    fn in_thread(event: &Value) -> Option<&str> {
        let relation = &event["content"]["m.relates_to"];
        (relation["rel_type"] == "m.thread").then(|| relation["event_id"].as_str().unwrap())
    }

    /// Whether the room's timeline has caught up with these events: its
    /// newest is the newest outside a thread, and each thread root counts
    /// its replies.
    async fn shows(timeline: &SdkTimeline, events: &[Value]) -> bool {
        let last = events
            .iter()
            .rev()
            .find(|e| in_thread(e).is_none())
            .map(|e| OwnedEventId::try_from(e["event_id"].as_str().unwrap()).unwrap());

        if timeline.latest_event_id().await != last {
            return false;
        }

        let items = timeline.items().await;

        events.iter().filter_map(in_thread).all(|root| {
            let replies = events.iter().filter(|e| in_thread(e) == Some(root)).count();

            items
                .iter()
                .filter_map(|i| i.as_event())
                .find(|e| e.event_id().is_some_and(|id| id == root))
                .and_then(|e| e.content().thread_summary())
                .is_some_and(|s| s.num_replies as usize == replies)
        })
    }

    /// A chat on this client's room, and the events it sends the app.
    async fn chat(client: &Client, room: Room) -> (Chat, Receiver<Event>) {
        focused_chat(client, room, true).await
    }

    /// A chat opened while the terminal has focus, or doesn't.
    async fn focused_chat(client: &Client, room: Room, focus: bool) -> (Chat, Receiver<Event>) {
        let (sender, events) = channel();
        let matrix = Matrix::new(sender, Handle::current());
        matrix.set_client(client.clone());
        matrix.add_room(room.clone()).await;
        matrix.room_opened_event(&room);

        if !focus {
            matrix.blur_event();
        }

        (Chat::try_new(matrix, room).unwrap(), events)
    }

    /// Hand the chat its timeline's events until it matches.
    async fn until(chat: &mut Chat, events: &Receiver<Event>, matches: impl Fn(&Chat) -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                while let Ok(event) = events.try_recv() {
                    match event {
                        Event::Matui(MatuiEvent::TimelineItems(timeline, items)) => {
                            chat.items_event(&timeline, &items)
                        }
                        Event::Matui(MatuiEvent::Paginated(timeline, result)) => {
                            chat.paginated_event(&timeline, &result)
                        }
                        Event::Matui(MatuiEvent::NotAtStart(timeline)) => {
                            chat.not_at_start_event(&timeline)
                        }
                        _ => {}
                    }

                    // draw it after every event, as the app does
                    let area = Rect::new(0, 0, 80, 24);
                    chat.render(area, &mut Buffer::empty(area));
                }

                if matches(chat) {
                    return;
                }

                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the chat never matched");
    }

    fn idle(chat: &Chat) -> bool {
        matches!(*chat.paging.borrow(), Paging::Idle)
    }

    fn at_start(chat: &Chat) -> bool {
        matches!(*chat.paging.borrow(), Paging::AtStart)
    }

    fn message<'a>(chat: &'a Chat, id: &str) -> Option<&'a Message> {
        chat.iter_messages().find(|m| event_id(m) == id)
    }

    /// The chat's messages, newest first.
    fn ids(chat: &Chat) -> Vec<&str> {
        chat.iter_messages().map(event_id).collect()
    }

    /// The message's event ID, or nothing until it's sent.
    fn event_id(message: &Message) -> &str {
        message.event_id().map_or("", |id| id.as_str())
    }

    fn edit_event(id: &str, ts: u64, target: &str, body: &str) -> Value {
        json!({
            "type": "m.room.message",
            "event_id": id,
            "sender": "@alice:example.org",
            "origin_server_ts": ts,
            "content": {
                "msgtype": "m.text",
                "body": format!("* {body}"),
                "m.new_content": { "msgtype": "m.text", "body": body },
                "m.relates_to": { "rel_type": "m.replace", "event_id": target }
            }
        })
    }

    #[tokio::test]
    async fn shows_edits_reactions_and_replies() {
        let (server, client, room) = joined().await;
        let (mut chat, events) = chat(&client, room).await;

        sync(
            &server,
            &client,
            vec![
                text_event("$text:example.org", 1000, "hello"),
                edit_event(
                    "$edit:example.org",
                    2000,
                    "$text:example.org",
                    "hello there",
                ),
                reaction_event("$reaction:example.org", 3000, "$text:example.org", "👍"),
                reply_event("$reply:example.org", 4000, "$text:example.org", "hi back"),
            ],
        )
        .await;

        until(&mut chat, &events, |c| {
            message(c, "$text:example.org").is_some_and(|m| m.edited && !m.replies.is_empty())
        })
        .await;

        assert_eq!(ids(&chat), ["$text:example.org"]);

        let text = message(&chat, "$text:example.org").unwrap();
        assert_eq!(text.display(), "hello there");
        assert_eq!(text.reactions.len(), 1);
        assert_eq!(text.reactions[0].body, "👍");
        assert_eq!(event_id(&text.replies[0]), "$reply:example.org");
        assert_eq!(text.replies[0].display(), "hi back");
    }

    #[tokio::test]
    async fn y_copies_the_whole_message_under_the_cursor() {
        let (server, client, room) = joined().await;
        let (mut chat, events) = chat(&client, room).await;

        // more paragraphs than message_line_limit shows
        let long = (1..=30)
            .map(|i| format!("paragraph {i}"))
            .collect::<Vec<_>>()
            .join("\n\n");

        sync(
            &server,
            &client,
            vec![
                text_event("$text:example.org", 1000, &long),
                reply_event("$reply:example.org", 2000, "$text:example.org", "hi back"),
            ],
        )
        .await;

        until(&mut chat, &events, |c| {
            message(c, "$text:example.org").is_some_and(|m| !m.replies.is_empty())
        })
        .await;

        // the cursor starts on the newest line, the reply's
        assert_eq!(chat.copy_text(), Some("hi back"));

        // up to the original message's own lines
        for _ in 0..chat.total_list_items() {
            if chat.selected_reply().map(event_id) == Some("$text:example.org") {
                break;
            }

            chat.next(1);
        }

        assert_eq!(chat.copy_text(), Some(long.as_str()));
    }

    fn draw(chat: &Chat) -> Buffer {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        chat.render(area, &mut buf);
        buf
    }

    #[tokio::test]
    async fn the_chat_is_centred_under_its_header() {
        let (server, client, room) = joined().await;
        let (mut chat, events) = chat(&client, room).await;

        sync(
            &server,
            &client,
            vec![text_event("$1:example.org", 1000, "one")],
        )
        .await;
        until(&mut chat, &events, |c| {
            message(c, "$1:example.org").is_some()
        })
        .await;

        let buf = draw(&chat);
        let row = |y: u16| -> Vec<&str> { (0..80).map(|x| buf[(x, y)].symbol()).collect() };

        // the header's borders, and the day's line, as far in from each side
        let header = row(1);
        let header = header.iter().position(|c| *c != " ").unwrap()
            ..header.iter().rposition(|c| *c != " ").unwrap();

        // below the header's three rows
        let day = (4..24).map(row).find(|r| r.contains(&"─")).unwrap();
        let line = day.iter().position(|c| *c == "─").unwrap()
            ..day.iter().rposition(|c| *c == "─").unwrap();

        assert_eq!(line.start - header.start, header.end - line.end);
    }

    #[tokio::test]
    async fn a_drag_copies_and_highlights_until_a_key() {
        let (server, client, room) = joined().await;
        let (mut chat, events) = chat(&client, room).await;

        sync(
            &server,
            &client,
            vec![text_event("$text:example.org", 1000, "hello wide world")],
        )
        .await;

        until(&mut chat, &events, |c| {
            message(c, "$text:example.org").is_some()
        })
        .await;

        let buf = draw(&chat);
        assert_eq!(chat.mouse_event(&press(at(&buf, "wide"))), None);
        assert_eq!(chat.mouse_event(&drag_to(end(&buf, "world"))), None);

        assert_eq!(
            chat.mouse_event(&release(end(&buf, "world"))),
            Some(Action::Copy("wide world".to_string()))
        );

        let buf = draw(&chat);
        assert!(highlighted(&buf, "wide world"));
        assert!(!highlighted(&buf, "hello"));
        assert!(header(&chat).contains("Copied!"));

        let down = KeyEvent::from(KeyCode::Down);
        chat.key_event(&down, &EventHandler::without_terminal())
            .unwrap();

        assert!(!highlighted(&draw(&chat), "wide"));
    }

    #[tokio::test]
    async fn a_press_moves_the_cursor_to_the_nearest_row_it_can_rest_on() {
        let (server, client, room) = joined().await;
        let (mut chat, events) = chat(&client, room).await;

        sync(
            &server,
            &client,
            vec![
                text_event("$older:example.org", 1000, "older"),
                text_event("$newer:example.org", 2000, "newer"),
            ],
        )
        .await;

        until(&mut chat, &events, |c| {
            message(c, "$newer:example.org").is_some()
        })
        .await;

        fn selected(chat: &Chat) -> Option<&str> {
            chat.selected_reply().map(event_id)
        }

        let buf = draw(&chat);
        let area = chat.text_map.borrow().area;
        assert_eq!(selected(&chat), Some("$newer:example.org"));

        // on a message's text, or anywhere along its row
        chat.mouse_event(&press(at(&buf, "older")));
        assert_eq!(selected(&chat), Some("$older:example.org"));

        chat.mouse_event(&press((area.right() - 1, at(&buf, "newer").1)));
        assert_eq!(selected(&chat), Some("$newer:example.org"));

        // above the oldest message, the oldest message
        chat.mouse_event(&press((area.x, area.y)));
        assert_eq!(selected(&chat), Some("$older:example.org"));

        // on the spacer between them, the message above it, wherever the
        // cursor was
        let spacer = (0..chat.total_list_items())
            .find(|&i| chat.invalid_selection(i))
            .unwrap();
        let spacer = (area.x, area.bottom() - 1 - spacer as u16);

        for from in ["older", "newer"] {
            chat.mouse_event(&press(at(&buf, from)));
            chat.mouse_event(&press(spacer));
            assert_eq!(selected(&chat), Some("$older:example.org"), "from {from}");
        }

        // but not off the list
        chat.mouse_event(&press(at(&buf, "older")));
        chat.mouse_event(&press((0, 0)));
        assert_eq!(selected(&chat), Some("$older:example.org"));
    }

    #[tokio::test]
    async fn links_are_underlined() {
        let (server, client, room) = joined().await;
        let (mut chat, events) = chat(&client, room).await;

        sync(
            &server,
            &client,
            vec![text_event(
                "$text:example.org",
                1000,
                "see https://example.org now",
            )],
        )
        .await;

        until(&mut chat, &events, |c| {
            message(c, "$text:example.org").is_some()
        })
        .await;

        let buf = draw(&chat);
        let underlined = |text: &str| {
            let (x, y) = at(&buf, text);
            (x..x + text.len() as u16).all(|x| {
                buf[(x, y)]
                    .modifier
                    .contains(ratatui::style::Modifier::UNDERLINED)
            })
        };

        assert!(underlined("https://example.org"));
        assert!(!underlined("see"));
        assert!(!underlined("now"));
    }

    #[tokio::test]
    async fn a_message_arriving_mid_drag_leaves_the_selection_on_its_text() {
        let (server, client, room) = joined().await;
        let (mut chat, events) = chat(&client, room).await;

        sync(
            &server,
            &client,
            vec![text_event("$text:example.org", 1000, "hello wide world")],
        )
        .await;

        until(&mut chat, &events, |c| {
            message(c, "$text:example.org").is_some()
        })
        .await;
        let before = at(&draw(&chat), "wide");
        chat.mouse_event(&press(before));

        // the new message pushes the old one up
        sync(
            &server,
            &client,
            vec![text_event("$newer:example.org", 2000, "newer")],
        )
        .await;

        until(&mut chat, &events, |c| {
            message(c, "$newer:example.org").is_some()
        })
        .await;
        let buf = draw(&chat);
        assert!(at(&buf, "wide").1 < before.1);

        chat.mouse_event(&drag_to(end(&buf, "world")));

        assert_eq!(
            chat.mouse_event(&release(end(&buf, "world"))),
            Some(Action::Copy("wide world".to_string()))
        );
        assert!(highlighted(&draw(&chat), "wide world"));
    }

    /// The chat's header, as drawn.
    fn header(chat: &Chat) -> String {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        chat.render(area, &mut buf);

        (0..3)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol())
            .collect()
    }

    #[tokio::test]
    async fn the_header_says_a_copy_happened_for_a_while() {
        let (_server, client, room) = joined().await;
        let (mut chat, _events) = chat(&client, room).await;

        chat.copied = Some(Instant::now());
        assert!(!chat.tick_event());
        assert!(header(&chat).contains("Copied!"));

        chat.copied = Some(Instant::now() - COPIED_FOR);
        assert!(chat.tick_event(), "the chat needs drawing without it");
        assert!(!header(&chat).contains("Copied"));
    }

    #[tokio::test]
    async fn a_thread_shows_as_a_reply_count_on_its_root() {
        let (server, client, room) = joined().await;
        let (mut chat, events) = chat(&client, room).await;

        sync(
            &server,
            &client,
            vec![
                text_event("$root:example.org", 1000, "root"),
                text_event("$other:example.org", 1500, "other"),
                thread_event("$t1:example.org", 2000, "$root:example.org", None),
            ],
        )
        .await;

        until(&mut chat, &events, |c| {
            message(c, "$root:example.org").is_some_and(|m| m.thread_replies == 1)
        })
        .await;

        // newest first, with the root where it was and its reply left out
        assert_eq!(ids(&chat), ["$other:example.org", "$root:example.org"]);
    }

    /// Bob's read receipt on this event.
    fn bob_read_to(id: &str) -> JoinedRoomBuilder {
        let receipt = EventFactory::new()
            .read_receipts()
            .add(
                &OwnedEventId::try_from(id).unwrap(),
                user_id!("@bob:example.org"),
                ReceiptType::Read,
                ReceiptThread::Unthreaded,
            )
            .into_event();

        JoinedRoomBuilder::new(room_id()).add_receipt(receipt)
    }

    fn seen_by_bob(chat: &Chat, id: &str) -> bool {
        chat.iter_messages()
            .find_map(|m| m.find_by_id(&TimelineEventItemId::EventId(id.try_into().unwrap())))
            .is_some_and(|m| m.receipts.iter().any(|u| u.id == "@bob:example.org"))
    }

    #[tokio::test]
    async fn a_receipt_shows_on_the_message_read_to() {
        let (server, client, room) = joined().await;
        let (mut chat, events) = chat(&client, room).await;

        sync(
            &server,
            &client,
            vec![
                text_event("$1:example.org", 1000, "one"),
                text_event("$2:example.org", 2000, "two"),
                text_event("$3:example.org", 3000, "three"),
            ],
        )
        .await;
        server
            .sync_room(&client, bob_read_to("$2:example.org"))
            .await;

        until(&mut chat, &events, |c| seen_by_bob(c, "$2:example.org")).await;

        assert!(!seen_by_bob(&chat, "$1:example.org"));
        assert!(!seen_by_bob(&chat, "$3:example.org"));
    }

    #[tokio::test]
    async fn a_receipt_shows_in_each_reply_chain_read_past() {
        let (server, client, room) = joined().await;
        let (mut chat, events) = chat(&client, room).await;

        sync(
            &server,
            &client,
            vec![
                text_event("$1:example.org", 1000, "one"),
                text_event("$2:example.org", 2000, "two"),
                reply_event("$3:example.org", 3000, "$1:example.org", "re: one"),
                text_event("$4:example.org", 4000, "four"),
            ],
        )
        .await;
        server
            .sync_room(&client, bob_read_to("$4:example.org"))
            .await;

        until(&mut chat, &events, |c| seen_by_bob(c, "$4:example.org")).await;

        // the reply moved up under the first message, behind the second
        assert!(seen_by_bob(&chat, "$3:example.org"));
        assert!(!seen_by_bob(&chat, "$1:example.org"));
        assert!(!seen_by_bob(&chat, "$2:example.org"));
    }

    /// Answer the receipts the client sends.
    async fn mock_receipts(server: &MatrixMockServer) {
        for receipt_type in [SendReceiptType::Read, SendReceiptType::FullyRead] {
            server.mock_send_receipt(receipt_type).ok().mount().await;
        }
    }

    /// The receipts sent so far, each as its type, event and thread, once
    /// they've had a chance to send.
    async fn receipts_sent(server: &MatrixMockServer) -> Vec<String> {
        tokio::time::sleep(Duration::from_millis(200)).await;

        let requests = server.server().received_requests().await.unwrap();

        requests
            .iter()
            .filter_map(|r| {
                let mut segments = r.url.path().rsplit('/');
                let event = segments.next()?;
                let receipt_type = segments.next()?;
                (segments.next()? == "receipt").then(|| {
                    let body: Value = r.body_json().unwrap();
                    let thread = body["thread_id"].as_str().unwrap_or("unthreaded");
                    format!("{receipt_type} {event} {thread}")
                })
            })
            .collect()
    }

    /// Open the room with one message, and return the receipts it sends.
    async fn marks_read(focus: bool) -> Vec<String> {
        let (server, client, room) = joined().await;
        mock_receipts(&server).await;

        let (mut chat, events) = focused_chat(&client, room, focus).await;

        sync(
            &server,
            &client,
            vec![text_event("$1:example.org", 1000, "one")],
        )
        .await;

        until(&mut chat, &events, |c| !ids(c).is_empty()).await;

        receipts_sent(&server).await
    }

    #[tokio::test]
    async fn opening_a_room_marks_its_main_timeline_read() {
        let mut sent = marks_read(true).await;
        sent.sort();

        assert_eq!(
            sent,
            [
                "m.fully_read $1:example.org unthreaded",
                "m.read $1:example.org main"
            ]
        );
    }

    #[tokio::test]
    async fn opening_a_room_out_of_focus_leaves_it_unread() {
        assert!(marks_read(false).await.is_empty());
    }

    /// Sync these events into the room, with its read marker on this one.
    async fn sync_read_to(
        server: &MatrixMockServer,
        client: &Client,
        events: Vec<Value>,
        id: &str,
    ) {
        let read_to = EventFactory::new().fully_read(&OwnedEventId::try_from(id).unwrap());

        let room = JoinedRoomBuilder::new(room_id())
            .add_timeline_bulk(raw(events))
            .add_account_data(read_to);

        server.sync_room(client, room).await;
    }

    /// The chat's items, newest first: messages by ID, and the markers.
    fn names(items: &[TimelineItem]) -> Vec<&str> {
        items
            .iter()
            .map(|item| match item {
                TimelineItem::Message(m) => event_id(m),
                TimelineItem::Events(_) => "events",
                TimelineItem::ReadMarker => "new",
                TimelineItem::DayMarker(_) => "day",
            })
            .collect()
    }

    // 2001-09-09 01:46:40 UTC, far from midnight in most time zones
    const SOME_DAY: u64 = 1_000_000_000_000;
    const DAY: u64 = 24 * 60 * 60 * 1000;

    #[test]
    fn day_labels_count_back_a_week() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let label = |days| label_day(today - chrono::Days::new(days), today);

        assert_eq!(label(0), "Today");
        assert_eq!(label(1), "Yesterday");
        assert_eq!(label(2), "Friday");
        assert_eq!(label(6), "Monday");
        assert_eq!(label(7), "Sun, Nov 8, 2026");
        assert_eq!(label(400), "Sat, Oct 11, 2025");
    }

    #[tokio::test]
    async fn days_are_marked_above_their_first_item() {
        let items = timeline_items(vec![
            text_event("$old:example.org", SOME_DAY, "old"),
            text_event("$same-day:example.org", SOME_DAY + 1000, "same day"),
            text_event("$later:example.org", SOME_DAY + 2 * DAY, "later"),
        ])
        .await;

        let marked = mark_days(make_message_list(&items, &vec![], "", None));

        assert_eq!(
            names(&marked),
            [
                "$later:example.org",
                "day",
                "$same-day:example.org",
                "$old:example.org",
                "day"
            ]
        );

        // a reply takes its message to the reply's day, leaving no marker
        // over a day with nothing left in it
        let items = timeline_items(vec![
            text_event("$old:example.org", SOME_DAY, "old"),
            reply_event(
                "$reply:example.org",
                SOME_DAY + 2 * DAY,
                "$old:example.org",
                "hi",
            ),
        ])
        .await;

        let marked = mark_days(make_message_list(&items, &vec![], "", None));

        assert_eq!(names(&marked), ["$old:example.org", "day"]);
    }

    #[tokio::test]
    async fn the_read_marker_goes_under_its_days_marker() {
        let (server, client, room) = joined().await;
        mock_receipts(&server).await;

        sync_read_to(
            &server,
            &client,
            vec![
                text_event("$1:example.org", SOME_DAY, "one"),
                text_event("$2:example.org", SOME_DAY + 2 * DAY, "two"),
            ],
            "$1:example.org",
        )
        .await;

        let (mut chat, events) = chat(&client, room).await;
        let opened = ["$2:example.org", "new", "day", "$1:example.org", "day"];

        until(&mut chat, &events, |c| names(&c.messages) == opened).await;
    }

    #[tokio::test]
    async fn the_read_marker_stays_where_the_room_opened_with_it() {
        let (server, client, room) = joined().await;
        mock_receipts(&server).await;

        sync_read_to(
            &server,
            &client,
            vec![
                text_event("$1:example.org", 1000, "one"),
                text_event("$2:example.org", 2000, "two"),
                text_event("$3:example.org", 3000, "three"),
            ],
            "$1:example.org",
        )
        .await;

        let (mut chat, events) = chat(&client, room).await;
        let opened = [
            "$3:example.org",
            "$2:example.org",
            "new",
            "$1:example.org",
            "day",
        ];

        until(&mut chat, &events, |c| names(&c.messages) == opened).await;

        // reading the room moves the marker past what it shows, but the line
        // stays put while the room's open
        sync_read_to(
            &server,
            &client,
            vec![text_event("$4:example.org", 4000, "four")],
            "$3:example.org",
        )
        .await;

        until(&mut chat, &events, |c| {
            read_marker(&c.items) == Some(MilliSecondsSinceUnixEpoch(3000u32.into()))
        })
        .await;

        assert_eq!(
            names(&chat.messages),
            [
                "$4:example.org",
                "$3:example.org",
                "$2:example.org",
                "new",
                "$1:example.org",
                "day"
            ]
        );
    }

    #[tokio::test]
    async fn sending_a_message_takes_the_read_marker_away() {
        let (server, client, room) = joined().await;
        mock_receipts(&server).await;

        sync_read_to(
            &server,
            &client,
            vec![
                text_event("$1:example.org", 1000, "one"),
                text_event("$2:example.org", 2000, "two"),
            ],
            "$1:example.org",
        )
        .await;

        let (mut chat, events) = chat(&client, room).await;
        until(&mut chat, &events, |c| names(&c.messages).contains(&"new")).await;

        let mut mine = text_event("$3:example.org", 3000, "three");
        mine["sender"] = json!(client.user_id().unwrap());
        sync(&server, &client, vec![mine]).await;

        until(&mut chat, &events, |c| {
            message(c, "$3:example.org").is_some()
        })
        .await;

        assert_eq!(
            names(&chat.messages),
            ["$3:example.org", "$2:example.org", "$1:example.org", "day"]
        );
    }

    /// The thread view of this root, once it's paged back to the root.
    async fn thread_view(chat: &Chat, events: &Receiver<Event>, root: &str) -> Chat {
        let root = OwnedEventId::try_from(root).unwrap();
        let mut thread = tokio::task::block_in_place(|| chat.thread(root.clone())).unwrap();

        until(&mut thread, events, |c| {
            at_start(c) && message(c, root.as_str()).is_some()
        })
        .await;

        thread
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_thread_view_shows_only_the_thread() {
        let (server, client, room) = joined().await;
        let (chat, events) = chat(&client, room).await;

        let t1 = thread_event("$t1:example.org", 2000, "$root:example.org", None);
        let t2 = thread_event(
            "$t2:example.org",
            3000,
            "$root:example.org",
            Some("$t1:example.org"),
        );
        let r1 = reaction_event("$r1:example.org", 4000, "$t1:example.org", "👍");

        sync(
            &server,
            &client,
            vec![
                text_event("$root:example.org", 1000, "root"),
                text_event("$other:example.org", 1500, "other"),
                t1.clone(),
                t2.clone(),
                r1.clone(),
            ],
        )
        .await;

        server
            .mock_room_relations()
            .match_target_event(owned_event_id!("$root:example.org"))
            .ok(RoomRelationsResponseTemplate::default().events(raw(vec![r1, t2, t1])))
            .mount()
            .await;

        let mut thread = thread_view(&chat, &events, "$root:example.org").await;

        until(&mut thread, &events, |c| {
            message(c, "$t1:example.org").is_some_and(|m| m.reactions.len() == 1)
        })
        .await;

        // newest first, flat, without the unrelated message
        assert_eq!(
            ids(&thread),
            ["$t2:example.org", "$t1:example.org", "$root:example.org"]
        );

        // the root's reply count belongs to the room
        assert_eq!(
            message(&thread, "$root:example.org")
                .unwrap()
                .thread_replies,
            0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn opening_a_thread_marks_it_read() {
        let (server, client, room) = joined().await;
        let (chat, events) = chat(&client, room).await;

        let t1 = thread_event("$t1:example.org", 2000, "$root:example.org", None);

        sync(
            &server,
            &client,
            vec![text_event("$root:example.org", 1000, "root"), t1.clone()],
        )
        .await;

        server
            .mock_room_relations()
            .match_target_event(owned_event_id!("$root:example.org"))
            .ok(RoomRelationsResponseTemplate::default().events(raw(vec![t1])))
            .mount()
            .await;

        mock_receipts(&server).await;
        chat.matrix
            .thread_opened_event(event_id!("$root:example.org"));

        let thread = thread_view(&chat, &events, "$root:example.org").await;
        assert_eq!(ids(&thread), ["$t1:example.org", "$root:example.org"]);

        assert_eq!(
            receipts_sent(&server).await,
            ["m.read $t1:example.org $root:example.org"]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_first_message_in_an_empty_thread_view_starts_the_thread() {
        let (server, client, room) = joined().await;
        let (chat, events) = chat(&client, room).await;

        sync(
            &server,
            &client,
            vec![text_event("$root:example.org", 1000, "root")],
        )
        .await;

        server
            .mock_room_relations()
            .match_target_event(owned_event_id!("$root:example.org"))
            .ok(RoomRelationsResponseTemplate::default())
            .mount()
            .await;

        let mut thread = thread_view(&chat, &events, "$root:example.org").await;
        assert_eq!(ids(&thread), ["$root:example.org"]);

        let (sent, mock) = server
            .mock_room_send()
            .ok_with_capture(event_id!("$first:example.org"), client.user_id().unwrap());
        mock.mock_once().mount().await;

        thread
            .matrix
            .send_text_message(thread.timeline().unwrap(), "first".to_string());

        let sent = sent.await.unwrap().deserialize_as::<Value>().unwrap();
        let relation = &sent["content"]["m.relates_to"];
        assert_eq!(relation["rel_type"], "m.thread");
        assert_eq!(relation["event_id"], "$root:example.org");

        until(&mut thread, &events, |c| {
            message(c, "$first:example.org").is_some_and(|m| m.sending.is_none())
        })
        .await;
        assert_eq!(ids(&thread), ["$first:example.org", "$root:example.org"]);
    }

    #[tokio::test]
    async fn search_pages_back_until_max_events() {
        let (server, client, room) = joined().await;

        let page = |events: Vec<Value>, end: &str| {
            RoomMessagesResponseTemplate::default()
                .events(raw(events))
                .end_token(end)
        };

        let pages = [
            ("p1", "p2", [("$4", 4000, "four"), ("$3", 3000, "three")]),
            ("p2", "p3", [("$2", 2000, "needle"), ("$1", 1000, "one")]),
            ("p3", "p4", [("$0", 500, "zero"), ("$00", 400, "needle")]),
        ];

        for (from, end, events) in pages {
            let events = events
                .iter()
                .map(|(id, ts, body)| text_event(&format!("{id}:example.org"), *ts, body))
                .collect();

            server
                .mock_room_messages()
                .match_from(from)
                .ok(page(events, end))
                .mount()
                .await;
        }

        let newest = JoinedRoomBuilder::new(room_id())
            .set_timeline_limited()
            .set_timeline_prev_batch("p1")
            .add_timeline_bulk(raw(vec![text_event("$5:example.org", 5000, "five")]));
        server.sync_room(&client, newest).await;

        let (mut chat, events) = chat(&client, room).await;
        chat.max_events = 5;
        chat.search_event("needle");

        until(&mut chat, &events, |c| idle(c) && c.event_count() >= 5).await;

        // give it a chance to page too far
        tokio::time::sleep(Duration::from_millis(100)).await;
        until(&mut chat, &events, idle).await;

        assert_eq!(chat.event_count(), 5);
        assert_eq!(ids(&chat), ["$2:example.org"]);
    }

    #[tokio::test]
    async fn search_says_searching_between_pages() {
        let (server, client, room) = joined().await;

        server
            .mock_room_messages()
            .match_from("p1")
            .ok(RoomMessagesResponseTemplate::default()
                .events(raw(vec![text_event("$1:example.org", 1000, "one")]))
                .end_token("p2"))
            .mount()
            .await;

        server
            .mock_room_messages()
            .match_from("p2")
            .ok(
                RoomMessagesResponseTemplate::default().events(raw(vec![text_event(
                    "$0:example.org",
                    500,
                    "zero",
                )])),
            )
            .mount()
            .await;

        let newest = JoinedRoomBuilder::new(room_id())
            .set_timeline_limited()
            .set_timeline_prev_batch("p1")
            .add_timeline_bulk(raw(vec![text_event("$2:example.org", 2000, "two")]));
        server.sync_room(&client, newest).await;

        let (mut chat, events) = chat(&client, room).await;
        chat.search_event("needle");

        let mut pages = 0;

        tokio::time::timeout(Duration::from_secs(5), async {
            while !at_start(&chat) {
                while let Ok(event) = events.try_recv() {
                    let more = match event {
                        Event::Matui(MatuiEvent::TimelineItems(timeline, items)) => {
                            chat.items_event(&timeline, &items);
                            false
                        }
                        Event::Matui(MatuiEvent::Paginated(timeline, result)) => {
                            chat.paginated_event(&timeline, &result);
                            result == Ok(false)
                        }
                        _ => false,
                    };

                    let area = Rect::new(0, 0, 80, 24);
                    let mut buf = Buffer::empty(area);
                    chat.render(area, &mut buf);

                    // the header's second line, drawn as the next page starts
                    if more {
                        let line: String = (0..area.width).map(|x| buf[(x, 2)].symbol()).collect();
                        assert!(line.contains("Searching..."), "header was {line:?}");
                        pages += 1;
                    }
                }

                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the search never reached the start");

        assert!(pages > 0);
    }

    #[tokio::test]
    async fn pages_back_again_after_a_limited_sync_resets_the_timeline() {
        let (server, client, room) = joined().await;

        let texts = |events: &[(&str, u64)]| -> Vec<Value> {
            events
                .iter()
                .map(|(id, ts)| text_event(&format!("{id}:example.org"), *ts, id))
                .collect()
        };

        // pages without an end token reach the start of the room
        for (from, events) in [
            ("p1", texts(&[("$2", 2000), ("$1", 1000)])),
            ("p2", texts(&[("$3", 3000), ("$2", 2000), ("$1", 1000)])),
        ] {
            server
                .mock_room_messages()
                .match_from(from)
                .ok(RoomMessagesResponseTemplate::default().events(raw(events)))
                .mount()
                .await;
        }

        let limited = |prev_batch: &str, events: Vec<Value>| {
            JoinedRoomBuilder::new(room_id())
                .set_timeline_limited()
                .set_timeline_prev_batch(prev_batch)
                .add_timeline_bulk(raw(events))
        };

        server
            .sync_room(&client, limited("p1", texts(&[("$3", 3000)])))
            .await;

        let (mut chat, events) = chat(&client, room).await;
        until(&mut chat, &events, |c| {
            at_start(c) && ids(c) == ["$3:example.org", "$2:example.org", "$1:example.org"]
        })
        .await;

        // a gap since the last sync makes the SDK drop all but the newest
        server
            .sync_room(&client, limited("p2", texts(&[("$4", 4000)])))
            .await;

        until(&mut chat, &events, |c| {
            ids(c)
                == [
                    "$4:example.org",
                    "$3:example.org",
                    "$2:example.org",
                    "$1:example.org",
                ]
        })
        .await;
    }

    #[tokio::test]
    async fn pages_back_again_after_a_reset_short_of_the_start() {
        let (server, client, room) = joined().await;

        let texts = |events: &[(&str, u64)]| -> Vec<Value> {
            events
                .iter()
                .map(|(id, ts)| text_event(&format!("{id}:example.org"), *ts, id))
                .collect()
        };

        for (from, events) in [
            ("p1", texts(&[("$2", 2000), ("$1", 1000)])),
            ("p3", texts(&[("$3", 3000), ("$2", 2000)])),
        ] {
            server
                .mock_room_messages()
                .match_from(from)
                .ok(RoomMessagesResponseTemplate::default()
                    .events(raw(events))
                    .end_token("more"))
                .mount()
                .await;
        }

        let limited = |prev_batch: &str, events: Vec<Value>| {
            JoinedRoomBuilder::new(room_id())
                .set_timeline_limited()
                .set_timeline_prev_batch(prev_batch)
                .add_timeline_bulk(raw(events))
        };

        server
            .sync_room(&client, limited("p1", texts(&[("$3", 3000)])))
            .await;

        // the room stops paging before its start
        let (mut chat, events) = chat(&client, room).await;
        chat.max_events = 3;
        until(&mut chat, &events, |c| {
            idle(c) && ids(c) == ["$3:example.org", "$2:example.org", "$1:example.org"]
        })
        .await;

        server
            .sync_room(&client, limited("p3", texts(&[("$4", 4000)])))
            .await;

        // the SDK drops all but the newest, then pages the rest back in from
        // its store
        until(&mut chat, &events, |c| {
            ids(c).len() >= 3 && ids(c)[0] == "$4:example.org"
        })
        .await;
    }

    #[tokio::test]
    async fn pages_on_past_a_page_with_nothing_to_show() {
        let (server, client, room) = joined().await;

        // reactions only add to a message that's already showing
        let pages = [
            (
                "p1",
                Some("p2"),
                vec![
                    reaction_event("$r2:example.org", 4000, "$5:example.org", "👍"),
                    reaction_event("$r1:example.org", 3000, "$5:example.org", "🎉"),
                ],
            ),
            ("p2", None, vec![text_event("$1:example.org", 1000, "one")]),
        ];

        for (from, end, events) in pages {
            let mut page = RoomMessagesResponseTemplate::default().events(raw(events));

            if let Some(end) = end {
                page = page.end_token(end);
            }

            server
                .mock_room_messages()
                .match_from(from)
                .ok(page)
                .mount()
                .await;
        }

        let newest = JoinedRoomBuilder::new(room_id())
            .set_timeline_limited()
            .set_timeline_prev_batch("p1")
            .add_timeline_bulk(raw(vec![text_event("$5:example.org", 5000, "five")]));
        server.sync_room(&client, newest).await;

        let (mut chat, events) = chat(&client, room).await;

        until(&mut chat, &events, |c| {
            at_start(c) && ids(c) == ["$5:example.org", "$1:example.org"]
        })
        .await;
    }

    #[tokio::test]
    async fn a_failed_page_says_so_until_scrolling_up_retries_it() {
        let (server, client, room) = joined().await;

        server
            .mock_room_messages()
            .match_from("p1")
            .error500()
            .up_to_n_times(1)
            .mount()
            .await;

        server
            .mock_room_messages()
            .match_from("p1")
            .ok(
                RoomMessagesResponseTemplate::default().events(raw(vec![text_event(
                    "$1:example.org",
                    1000,
                    "one",
                )])),
            )
            .mount()
            .await;

        let newest = JoinedRoomBuilder::new(room_id())
            .set_timeline_limited()
            .set_timeline_prev_batch("p1")
            .add_timeline_bulk(raw(vec![text_event("$2:example.org", 2000, "two")]));
        server.sync_room(&client, newest).await;

        let (mut chat, events) = chat(&client, room).await;
        until(&mut chat, &events, |c| {
            matches!(*c.paging.borrow(), Paging::Failed)
        })
        .await;

        // drawing again doesn't retry
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        chat.render(area, &mut buf);
        assert!(matches!(*chat.paging.borrow(), Paging::Failed));

        let line: String = (0..area.width).map(|x| buf[(x, 2)].symbol()).collect();
        assert!(
            line.contains("Couldn't load earlier messages."),
            "header was {line:?}"
        );

        // drawing after scrolling pages back, as `k` does
        chat.next(1);
        chat.render(area, &mut buf);
        until(&mut chat, &events, |c| {
            at_start(c) && ids(c) == ["$2:example.org", "$1:example.org"]
        })
        .await;
    }

    #[tokio::test]
    async fn undecryptable_messages_fill_in_when_their_key_arrives() {
        let server = MatrixMockServer::new().await;
        server.mock_crypto_endpoints_preset().await;

        let (alice, bob) = server.set_up_alice_and_bob_for_encryption().await;
        let alice_id = alice.user_id().unwrap();
        let bob_id = bob.user_id().unwrap();

        let factory = EventFactory::new().room(room_id()).sender(alice_id);
        let members: Vec<Raw<RoomMemberEvent>> = vec![
            factory.member(alice_id).into_raw(),
            factory.member(bob_id).sender(bob_id).into_raw(),
        ];

        for client in [&alice, &bob] {
            let mut room = JoinedRoomBuilder::new(room_id())
                .add_state_event(factory.create(alice_id, RoomVersionId::V1))
                .add_state_event(factory.room_encryption());

            for member in &members {
                room = room.add_state_event(member.clone().cast_unchecked());
            }

            server.sync_room(client, room).await;
        }

        server.mock_get_members().ok(members.clone()).mount().await;

        let bob_room = bob.get_room(room_id()).unwrap();
        let (mut chat, events) = chat(&bob, bob_room).await;

        let (sent, mock) = server
            .mock_room_send()
            .ok_with_capture(event_id!("$secret"), alice_id);
        mock.mock_once().mount().await;

        // hold back the room key until Bob has the event
        let key_sent = server
            .mock_capture_put_to_device_then_sync_back(alice_id, &bob)
            .await;

        alice
            .get_room(room_id())
            .unwrap()
            .send(RoomMessageEventContent::text_plain("secret"))
            .await
            .unwrap();

        let encrypted = JoinedRoomBuilder::new(room_id()).add_timeline_event(sent.await.unwrap());
        server.sync_room(&bob, encrypted).await;

        until(&mut chat, &events, |c| message(c, "$secret").is_some()).await;
        assert_eq!(
            message(&chat, "$secret").unwrap().display(),
            "Unable to decrypt this message."
        );

        key_sent.await;

        until(&mut chat, &events, |c| {
            message(c, "$secret").is_some_and(|m| m.display() == "secret")
        })
        .await;
    }

    /// A chat that sends, once its timeline has loaded, with these events.
    async fn sending_chat(
        events: Vec<Value>,
    ) -> (MatrixMockServer, Chat, Receiver<Event>, Arc<SdkTimeline>) {
        let loaded = loaded_chat(events).await;
        loaded.1.matrix.watch_send_queue();
        loaded
    }

    /// A chat once its timeline has loaded, with these events, without
    /// matui watching the send queue.
    async fn loaded_chat(
        events: Vec<Value>,
    ) -> (MatrixMockServer, Chat, Receiver<Event>, Arc<SdkTimeline>) {
        let (server, client, room) = joined().await;
        let (mut chat, receiver) = chat(&client, room).await;

        let count = events.len();
        sync(&server, &client, events).await;
        until(&mut chat, &receiver, |c| c.iter_messages().count() == count).await;

        let timeline = chat.timeline().unwrap();
        (server, chat, receiver, timeline)
    }

    /// A message of ours, from the server.
    fn own_text_event(id: &str, body: &str) -> Value {
        let mut event = text_event(id, 1000, body);
        event["sender"] = json!("@example:localhost");
        event
    }

    /// A failure retrying can't fix, late enough to see what came before it.
    fn too_large_later() -> ResponseTemplate {
        ResponseTemplate::new(413)
            .set_body_json(json!({ "errcode": "M_TOO_LARGE", "error": "Too large" }))
            .set_delay(Duration::from_millis(300))
    }

    fn by_body<'a>(chat: &'a Chat, body: &str) -> Option<&'a Message> {
        chat.iter_messages().find(|m| m.display() == body)
    }

    /// Hand the chat its timeline's events until the app gets an error,
    /// and return the error.
    async fn until_error(chat: &mut Chat, events: &Receiver<Event>) -> String {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                while let Ok(event) = events.try_recv() {
                    match event {
                        Event::Matui(MatuiEvent::TimelineItems(timeline, items)) => {
                            chat.items_event(&timeline, &items)
                        }
                        Event::Matui(MatuiEvent::Error(error)) => return error,
                        _ => {}
                    }
                }

                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the app never got an error")
    }

    #[tokio::test]
    async fn a_sent_message_shows_as_sending_then_sent() {
        let (server, mut chat, events, timeline) = sending_chat(vec![]).await;

        server
            .mock_room_send()
            .ok_with_delay(event_id!("$sent:example.org"), Duration::from_millis(500))
            .mock_once()
            .mount()
            .await;

        chat.matrix.send_text_message(timeline, "hello".to_string());

        until(&mut chat, &events, |c| by_body(c, "hello").is_some()).await;
        let pending = by_body(&chat, "hello").unwrap();
        assert_eq!(pending.sending, Some(Sending::Pending));
        assert_eq!(pending.event_id(), None);

        until(&mut chat, &events, |c| {
            message(c, "$sent:example.org").is_some_and(|m| m.sending.is_none())
        })
        .await;
        assert_eq!(ids(&chat), ["$sent:example.org"]);
    }

    #[tokio::test]
    async fn a_stuck_message_shows_its_error_until_deleted() {
        let (server, mut chat, events, timeline) = sending_chat(vec![]).await;

        server
            .mock_room_send()
            .error_too_large()
            .mock_once()
            .mount()
            .await;

        chat.matrix
            .send_text_message(timeline.clone(), "too big".to_string());

        until(&mut chat, &events, |c| {
            by_body(c, "too big").is_some_and(|m| matches!(m.sending, Some(Sending::Stuck(_))))
        })
        .await;

        let Some(Sending::Stuck(error)) = &by_body(&chat, "too big").unwrap().sending else {
            unreachable!()
        };
        assert!(error.contains("M_TOO_LARGE"), "{}", error);

        // deleting it lets the next message through
        let id = by_body(&chat, "too big").unwrap().id.clone();
        chat.matrix.redact_event(timeline.clone(), id);
        until(&mut chat, &events, |c| by_body(c, "too big").is_none()).await;

        server
            .mock_room_send()
            .ok(event_id!("$next:example.org"))
            .mock_once()
            .mount()
            .await;

        chat.matrix.send_text_message(timeline, "next".to_string());

        until(&mut chat, &events, |c| {
            message(c, "$next:example.org").is_some_and(|m| m.sending.is_none())
        })
        .await;
    }

    #[tokio::test]
    async fn a_stuck_edit_is_cancelled_with_its_error() {
        let (server, mut chat, events, timeline) =
            sending_chat(vec![own_text_event("$text:example.org", "hello")]).await;

        server
            .mock_room_send()
            .respond_with(too_large_later())
            .mount()
            .await;

        chat.matrix.replace_event(
            timeline,
            owned_event_id!("$text:example.org"),
            "goodbye".to_string(),
        );

        until(&mut chat, &events, |c| by_body(c, "goodbye").is_some()).await;

        let error = until_error(&mut chat, &events).await;
        assert!(
            error.starts_with("Could not edit the message:"),
            "{}",
            error
        );

        // nothing is pending anymore, though the SDK's open timeline keeps
        // the new text until the room is opened again
        until(&mut chat, &events, |c| {
            message(c, "$text:example.org").is_some_and(|m| m.sending.is_none())
        })
        .await;
    }

    #[tokio::test]
    async fn a_stuck_delete_is_cancelled_with_its_error() {
        let (server, mut chat, events, timeline) =
            sending_chat(vec![text_event("$text:example.org", 1000, "hello")]).await;

        server
            .mock_room_redact()
            .respond_with(too_large_later())
            .mount()
            .await;

        let id = TimelineEventItemId::EventId(owned_event_id!("$text:example.org"));
        chat.matrix.redact_event(timeline, id);

        until(&mut chat, &events, |c| ids(c).is_empty()).await;

        let error = until_error(&mut chat, &events).await;
        assert!(
            error.starts_with("Could not delete the message:"),
            "{}",
            error
        );

        until(&mut chat, &events, |c| ids(c) == ["$text:example.org"]).await;
    }

    /// React to the message, which the server refuses for good.
    async fn react_and_get_stuck(server: &MatrixMockServer, chat: &Chat, id: &str) {
        server
            .mock_room_send()
            .error_too_large()
            .mock_once()
            .mount()
            .await;

        let id = TimelineEventItemId::EventId(id.try_into().unwrap());
        chat.matrix
            .toggle_reaction(chat.timeline().unwrap(), id, "👍".to_string());
    }

    /// Send a message, and wait until the server has it.
    async fn sends(server: &MatrixMockServer, chat: &mut Chat, events: &Receiver<Event>) {
        server
            .mock_room_send()
            .ok(event_id!("$next:example.org"))
            .mock_once()
            .mount()
            .await;

        chat.matrix
            .send_text_message(chat.timeline().unwrap(), "next".to_string());

        until(chat, events, |c| {
            message(c, "$next:example.org").is_some_and(|m| m.sending.is_none())
        })
        .await;
    }

    #[tokio::test]
    async fn a_stuck_reaction_is_cancelled_with_its_error() {
        let (server, mut chat, events, _) =
            sending_chat(vec![text_event("$text:example.org", 1000, "hello")]).await;

        react_and_get_stuck(&server, &chat, "$text:example.org").await;

        let error = until_error(&mut chat, &events).await;
        assert!(
            error.starts_with("Could not react to the message:"),
            "{}",
            error
        );

        sends(&server, &mut chat, &events).await;
    }

    #[tokio::test]
    async fn a_request_stuck_before_matui_watched_is_cancelled() {
        let (server, mut chat, events, timeline) =
            loaded_chat(vec![text_event("$text:example.org", 1000, "hello")]).await;

        let client = timeline.room().client();
        let mut updates = client.send_queue().subscribe();

        react_and_get_stuck(&server, &chat, "$text:example.org").await;

        tokio::time::timeout(Duration::from_secs(5), async {
            while !matches!(
                updates.recv().await.unwrap().update,
                RoomSendQueueUpdate::SendError { .. }
            ) {}
        })
        .await
        .expect("the reaction never got stuck");

        // as when matui starts
        chat.matrix.watch_send_queue();

        let error = until_error(&mut chat, &events).await;
        assert!(
            error.starts_with("Could not react to the message:"),
            "{}",
            error
        );

        sends(&server, &mut chat, &events).await;
    }

    // an event nobody has the key for, with its relation in the clear
    fn encrypted_event(id: &str, ts: u64, relates_to: Option<Value>) -> Value {
        let mut content = json!({
            "algorithm": "m.megolm.v1.aes-sha2",
            "ciphertext": "AwgAEnAC",
            "sender_key": "key",
            "device_id": "DEVICE",
            "session_id": "session"
        });

        if let Some(relates_to) = relates_to {
            content["m.relates_to"] = relates_to;
        }

        json!({
            "type": "m.room.encrypted",
            "event_id": id,
            "sender": "@alice:example.org",
            "origin_server_ts": ts,
            "content": content
        })
    }

    #[tokio::test]
    async fn only_undecryptable_messages_get_a_placeholder() {
        let root = "$root:example.org";

        let items = timeline_items(vec![
            text_event(root, 1000, "root"),
            encrypted_event(
                "$edit:example.org",
                2000,
                Some(json!({ "rel_type": "m.replace", "event_id": root })),
            ),
            encrypted_event(
                "$reaction:example.org",
                3000,
                Some(json!({ "rel_type": "m.annotation", "event_id": root, "key": "👍" })),
            ),
            encrypted_event(
                "$thread:example.org",
                4000,
                Some(json!({
                    "rel_type": "m.thread",
                    "event_id": root,
                    "is_falling_back": true,
                    "m.in_reply_to": { "event_id": root }
                })),
            ),
            encrypted_event("$plain:example.org", 5000, None),
        ])
        .await;

        let list = make_message_list(&items, &vec![], "", None);
        let messages: Vec<&Message> = list.iter().filter_map(TimelineItem::message).collect();

        // newest first: the plain one stands alone, and the thread reply,
        // edit and reaction don't show
        let ids: Vec<&str> = messages.iter().map(|m| event_id(m)).collect();
        assert_eq!(ids, ["$plain:example.org", root]);
        assert_eq!(messages[0].display(), "Unable to decrypt this message.");
    }
}
