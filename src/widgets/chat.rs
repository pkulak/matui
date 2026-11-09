use crate::app::Popup;
use crate::event::{Event, EventHandler};
use crate::matrix::Matrix;
use crate::matrix::roomcache::DecoratedRoom;
use crate::pictures::{Pictures, picker};
use crate::settings::{is_muted, max_events, toggle_mute};
use crate::spawn::{get_file_paths, spawn_editor};
use crate::widgets::EventResult::Consumed;
use crate::widgets::command::Command;
use crate::widgets::compose::Compose;
use crate::widgets::members::Members;
use crate::widgets::message::{LineType, Message, Reaction, Receipt};
use crate::widgets::react::React;
use crate::widgets::react::ReactResult;
use crate::widgets::search::Search;
use crate::widgets::timeline::Timeline;
use crate::widgets::upload::Upload;
use crate::widgets::{EventResult, get_margin};
use crate::{KeyCombo, consumed, limit_list, pretty_list, truncate};
use anyhow::bail;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use image::DynamicImage;
use log::info;
use matrix_sdk::RoomMemberships;
use matrix_sdk::room::{Room, RoomMember};
use matrix_sdk::ruma::events::StateEventContentChange;
use matrix_sdk::ruma::events::room::MediaSource;
use matrix_sdk::ruma::events::room::message::MessageType::Text;
use matrix_sdk::ruma::events::room::power_levels::UserPowerLevel;
use matrix_sdk::ruma::{
    EventId, MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedMxcUri, OwnedUserId, RoomId, UserId,
};
use matrix_sdk_ui::eyeball_im::Vector;
use matrix_sdk_ui::timeline::{
    AnyOtherStateEventContentChange, EventTimelineItem, MembershipChange, Timeline as SdkTimeline,
    TimelineEventItemId, TimelineItem as SdkItem, TimelineItemContent,
};
use once_cell::sync::OnceCell;
use std::cell::{Cell, RefCell};
use std::collections::{BinaryHeap, HashSet};
use std::ops::Range;
use std::sync::{Arc, Mutex};
use tokio::task::AbortHandle;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders, ListItem, ListState, Paragraph, Widget};

use super::confirm::{Confirm, ConfirmBehavior};

pub struct Chat {
    matrix: Matrix,
    room: DecoratedRoom,
    timeline: Option<Arc<SdkTimeline>>,
    items: Vector<Arc<SdkItem>>,
    messages: Vec<TimelineItem>,
    window: Range<usize>,
    read_to: Option<OwnedEventId>,
    react: Option<React>,
    typing: Option<String>,
    list_state: Cell<ListState>,
    hit_start: bool,
    max_events: usize,
    fetching: Cell<bool>,
    fetch: Cell<Option<AbortHandle>>,
    /// How many events there were when we last paged, until that page's
    /// items arrive, separately from its end.
    paged_from: Cell<Option<usize>>,
    follow: Option<AbortHandle>,
    height: Cell<usize>,
    line_types: Mutex<Vec<LineType>>,
    pictures: RefCell<Pictures>,
    bookmark: Cell<Option<Bookmark>>,
    focus: bool,
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
        let handles = [self.fetch.take(), self.follow.take()];

        for handle in handles.into_iter().flatten() {
            handle.abort();
        }
    }
}

impl Chat {
    pub fn try_new(matrix: Matrix, room: Room, focus: bool) -> Option<Self> {
        let decorated_room = matrix.wrap_room(&room)?;

        let follow = matrix.follow_timeline(room);

        Some(Self {
            matrix: matrix.clone(),
            room: decorated_room,
            timeline: None,
            items: Vector::new(),
            messages: vec![],
            window: (0..0),
            read_to: None,
            react: None,
            typing: None,
            list_state: Cell::new(ListState::default()),
            hit_start: false,
            max_events: max_events(),
            fetching: Cell::new(true),
            fetch: Cell::new(None),
            paged_from: Cell::new(None),
            follow: Some(follow),
            height: Cell::new(20),
            line_types: Mutex::new(vec![]),
            pictures: RefCell::default(),
            bookmark: Cell::new(Option::None),
            focus,
            search_term: "".to_string(),
            thread_root: None,
            delete_combo: KeyCombo::new(vec!['d', 'd']),
            members: vec![],
            pretty_members: OnceCell::new(),
            checked: HashSet::new(),
        })
    }

    /// A new chat viewing a single thread in this room, on its own timeline.
    pub fn thread(&self, root: OwnedEventId) -> anyhow::Result<Self> {
        let timeline = self.matrix.thread_timeline(&self.room(), root.clone())?;
        let follow = self.matrix.follow(timeline.clone());

        let chat = Self {
            matrix: self.matrix.clone(),
            room: self.room.clone(),
            timeline: Some(timeline),
            items: Vector::new(),
            messages: vec![],
            window: (0..0),
            read_to: None,
            react: None,
            typing: None,
            list_state: Cell::new(ListState::default()),
            hit_start: false,
            max_events: self.max_events,
            fetching: Cell::new(false),
            fetch: Cell::new(None),
            paged_from: Cell::new(None),
            follow: Some(follow),
            height: Cell::new(20),
            line_types: Mutex::new(vec![]),
            pictures: RefCell::default(),
            bookmark: Cell::new(Option::None),
            focus: true,
            search_term: "".to_string(),
            thread_root: Some(root),
            delete_combo: KeyCombo::new(vec!['d', 'd']),
            members: self.members.clone(),
            pretty_members: OnceCell::new(),
            checked: self.checked.clone(),
        };

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
                    if self.thread_root.is_none()
                        && message.thread_replies > 0
                        && let Some(id) = message.event_id()
                    {
                        let thread = self.thread(id.clone())?;

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
                    app.set_popup(Popup::Compose(Compose::new(room, timeline, matrix, "")))
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
                    app.set_popup(Popup::Members(members))
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

                Ok(Consumed(Box::new(move |app| app.thread = Some(thread))))
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
                    app.set_popup(Popup::Upload(Upload::new(matrix, timeline, paths)))
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

        let first = self.timeline.is_none();
        let reset = self.messages.is_empty();

        self.timeline = Some(timeline.clone());
        self.items = items.clone();
        self.check_item_senders();
        self.set_messages(false);
        self.pretty_members = OnceCell::new();
        self.set_fully_read();

        // the SDK can drop earlier events while a page is on its way
        if let Some(from) = self.paged_from.get() {
            self.paged_from.set(Some(from.min(self.event_count())));
        }

        if reset {
            let mut state = self.list_state.take();
            state.select(Some(0));
            self.list_state.set(state);
        }

        // done loading; now page back until there's enough to show
        if first {
            self.fetching.set(false);
            self.try_fetch_previous();
        } else {
            self.keep_paging();
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

    /// A timeline paged back.
    pub fn paginated_event(&mut self, timeline: &Arc<SdkTimeline>, hit_start: bool) {
        if !self
            .timeline
            .as_ref()
            .is_some_and(|t| Arc::ptr_eq(t, timeline))
        {
            return;
        }

        self.hit_start = hit_start;

        if self.fetch.take().is_some() {
            self.fetching.set(false);
            self.keep_paging();
        }
    }

    /// A timeline lost its earlier events, so page back to them again.
    pub fn not_at_start_event(&mut self, timeline: &Arc<SdkTimeline>) {
        if !self.hit_start
            || !self
                .timeline
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, timeline))
        {
            return;
        }

        self.hit_start = false;
        self.try_fetch_previous();
    }

    // page again once the last page is done and its items are here
    fn keep_paging(&self) {
        if !self.fetching.get()
            && self
                .paged_from
                .get()
                .is_some_and(|from| self.event_count() > from)
        {
            self.try_fetch_previous();
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

    fn fetch_pictures(&self, sources: Vec<MediaSource>) {
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
        self.messages = make_message_list(
            &self.items,
            &self.members,
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

    fn check_sender(&mut self, user_id: &UserId) {
        if !self.checked.contains(user_id) {
            self.checked.insert(user_id.to_owned());
            self.matrix
                .fetch_room_member(self.room(), user_id.to_owned());
            info!("fetching {}", user_id);
        }
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

        if self.hit_start || self.fetching.get() {
            return;
        }

        let count = self.event_count();
        self.paged_from.set(None);

        let state = self.list_state.take();
        let buffer = self.total_list_items() - state.selected().unwrap_or_default();
        self.list_state.set(state);

        if buffer < 100 && count < self.max_events {
            let limit = if self.search_term.is_empty() { 32 } else { 256 };

            self.fetch
                .set(Some(self.matrix.paginate(timeline.clone(), limit)));

            self.fetching.set(true);
            self.paged_from.set(Some(count));
            info!("fetching more events... {}", count)
        }
    }

    fn event_count(&self) -> usize {
        self.items.iter().filter(|i| i.as_event().is_some()).count()
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

        let p_area = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(2)
            .vertical_margin(1)
            .constraints([Constraint::Percentage(100)])
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
                format!("{}... ({})", term, self.chat.event_count()),
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

        let window = &self.chat.messages[self.chat.window.clone()];
        let mut pictures = self.chat.pictures.borrow_mut();

        let timeline = Timeline::new(
            window,
            (area.width - 2) as usize,
            &mut line_types,
            &mut pictures,
            picker(),
        );

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

        let fetches = timeline.render(splits[1], buf, &mut list_state);
        self.chat.list_state.set(list_state);
        self.chat.fetch_pictures(fetches);

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
    pub(crate) fn message(&self) -> Option<&Message> {
        match self {
            TimelineItem::Message(m) => Some(m),
            TimelineItem::Events(_) => None,
        }
    }

    pub fn to_list_items(
        &self,
        width: usize,
        picture_rows: Option<u16>,
        sidecar: &mut Vec<LineType>,
    ) -> Vec<ListItem<'_>> {
        match self {
            TimelineItem::Message(m) => m.to_list_items(width, picture_rows, sidecar),
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

fn state_entry(event: &EventTimelineItem, members: &[RoomMember]) -> Option<StateEntry> {
    let line = match event.content() {
        TimelineItemContent::MembershipChange(change) => {
            let kind = match change.change()? {
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

            let name = display_name(change.user_id(), change.display_name().as_deref(), members);
            return Some(StateEntry::Member(kind, name));
        }
        TimelineItemContent::OtherState(state) => match state.content() {
            AnyOtherStateEventContentChange::RoomName(StateEventContentChange::Original {
                content,
                ..
            }) => format!(
                "{} changed the room name to {}.",
                display_name(event.sender(), None, members),
                truncate(content.name.clone(), 48),
            ),
            AnyOtherStateEventContentChange::RoomTopic(StateEventContentChange::Original {
                content,
                ..
            }) => format!(
                "{} changed the topic to {}.",
                display_name(event.sender(), None, members),
                truncate(content.topic.clone(), 48),
            ),
            _ => return None,
        },
        _ => return None,
    };

    Some(StateEntry::Line(line))
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
            if thread_root.is_none()
                && search_term.is_empty()
                && let Some(entry) = state_entry(event, members)
            {
                pending.push(entry, event.timestamp());
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

const REPLY_TEMPLATE: &str = "<!--
  Replying to:

{}
-->";

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::handler::MatuiEvent;
    use crate::widgets::message::Sending;
    use crate::widgets::message::tests::{reaction_event, reply_event, text_event, thread_event};
    use matrix_sdk::Client;
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
    async fn joined() -> (MatrixMockServer, Client, Room) {
        let server = MatrixMockServer::new().await;
        server.mock_room_state_encryption().plain().mount().await;

        let client = server.client_builder().build().await;
        client.event_cache().subscribe().unwrap();

        let room = server.sync_joined_room(&client, room_id()).await;
        (server, client, room)
    }

    /// Sync these events into the room, oldest first.
    async fn sync(server: &MatrixMockServer, client: &Client, events: Vec<Value>) {
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

        (Chat::try_new(matrix, room, focus).unwrap(), events)
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
                        Event::Matui(MatuiEvent::Paginated(timeline, hit_start)) => {
                            chat.paginated_event(&timeline, hit_start)
                        }
                        Event::Matui(MatuiEvent::NotAtStart(timeline)) => {
                            chat.not_at_start_event(&timeline)
                        }
                        _ => {}
                    }
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

    /// Open the room with one message, and return whether it's marked read.
    async fn marks_read(focus: bool) -> bool {
        let (server, client, room) = joined().await;
        server.mock_send_read_markers().ok().mount().await;

        let (mut chat, events) = focused_chat(&client, room, focus).await;

        sync(
            &server,
            &client,
            vec![text_event("$1:example.org", 1000, "one")],
        )
        .await;

        until(&mut chat, &events, |c| !ids(c).is_empty()).await;

        // give it a chance to send
        tokio::time::sleep(Duration::from_millis(200)).await;

        let requests = server.server().received_requests().await.unwrap();
        let marked: Vec<Value> = requests
            .iter()
            .filter(|r| r.url.path().ends_with("/read_markers"))
            .map(|r| r.body_json().unwrap())
            .collect();

        match marked.as_slice() {
            [] => false,
            [body] => {
                assert_eq!(body["m.fully_read"], "$1:example.org");
                assert_eq!(body["m.read"], "$1:example.org");
                true
            }
            _ => panic!("marked read more than once: {:?}", marked),
        }
    }

    #[tokio::test]
    async fn opening_a_room_marks_it_read() {
        assert!(marks_read(true).await);
    }

    #[tokio::test]
    async fn opening_a_room_out_of_focus_leaves_it_unread() {
        assert!(!marks_read(false).await);
    }

    /// The thread view of this root, once it's paged back to the root.
    async fn thread_view(chat: &Chat, events: &Receiver<Event>, root: &str) -> Chat {
        let root = OwnedEventId::try_from(root).unwrap();
        let mut thread = tokio::task::block_in_place(|| chat.thread(root.clone())).unwrap();

        until(&mut thread, events, |c| {
            c.hit_start && message(c, root.as_str()).is_some()
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

        until(&mut chat, &events, |c| {
            !c.fetching.get() && c.event_count() >= 5
        })
        .await;

        // give it a chance to page too far
        tokio::time::sleep(Duration::from_millis(100)).await;
        until(&mut chat, &events, |c| !c.fetching.get()).await;

        assert_eq!(chat.event_count(), 5);
        assert_eq!(ids(&chat), ["$2:example.org"]);
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
            c.hit_start && ids(c) == ["$3:example.org", "$2:example.org", "$1:example.org"]
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
