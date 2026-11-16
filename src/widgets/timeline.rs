use crate::pictures::{PictureSource, Pictures, picture_source, source_uri};
use crate::selection::{BodyRow, TextMap};
use crate::widgets::chat::{TimelineItem, day_line};
use crate::widgets::message::LineType;
use chrono::NaiveDate;
use matrix_sdk_ui::timeline::TimelineEventItemId;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::{Clear, List, ListDirection, ListItem, ListState, StatefulWidget, Widget};
use ratatui_image::picker::Picker;
use ratatui_image::sliced::{SignedPosition, SlicedImage};
use std::ops::Range;

const HIGHLIGHT_SYMBOL: &str = "> ";

// the chat's message list, drawn bottom to top, with any pictures drawn
// over the blank rows left for them
pub struct Timeline<'a> {
    items: Vec<ListItem<'a>>,
    pictures: &'a mut Pictures,
    picker: Option<&'a Picker>,
    placements: Vec<Placement>,
    // the message text on each list row
    bodies: Vec<Option<BodyRow<'a, TimelineEventItemId>>>,
    // the list row of each day marker's line, bottom up
    days: Vec<(usize, NaiveDate)>,
    width: usize,
}

// where a message with a picture source landed in the list
struct Placement {
    source: PictureSource,
    message: TimelineEventItemId,
    // the message's list rows
    rows: Range<usize>,
    // the list row of the picture's top, and its size, once that's known
    picture: Option<(usize, Size)>,
}

impl<'a> Timeline<'a> {
    // lays out the visible messages, recording what each list row is in
    // line_types, so the caller can place the selection before drawing
    pub fn new(
        messages: &'a [TimelineItem],
        width: usize,
        line_types: &mut Vec<LineType>,
        pictures: &'a mut Pictures,
        picker: Option<&'a Picker>,
    ) -> Self {
        let mut items = vec![];
        let mut placements = vec![];
        let mut bodies = vec![];
        let mut days = vec![];

        // each item reverses its own rows, so collect them one at a time
        let mut buffer = vec![];
        let mut body_buffer = vec![];

        for m in messages {
            // the message, and where its picture comes from
            let source = picker
                .and(m.message())
                .and_then(|m| Some((&m.id, picture_source(&m.body)?)));

            let size = picker
                .zip(source.as_ref())
                .and_then(|(picker, (_, source))| pictures.size(source, picker, width as u16));

            items.append(&mut m.to_list_items(
                width,
                size.map(|s| s.height),
                &mut buffer,
                &mut body_buffer,
            ));

            let start = line_types.len();
            line_types.append(&mut buffer);
            bodies.append(&mut body_buffer);

            if let TimelineItem::DayMarker(day) = m {
                days.push((start, *day));
            }

            if let Some((message, source)) = source {
                // the picture sits right under the author line, which is
                // the message's topmost start
                let picture = size.and_then(|size| {
                    let author = line_types[start..]
                        .iter()
                        .rposition(|l| matches!(l, LineType::MessageStart(_)))?;

                    Some((start + author - 1, size))
                });

                placements.push(Placement {
                    source,
                    message: message.clone(),
                    rows: start..line_types.len(),
                    picture,
                });
            }
        }

        Self {
            items,
            pictures,
            picker,
            placements,
            bodies,
            days,
            width,
        }
    }

    // the nearest day marker at or above a row
    fn marker_over(&self, row: usize) -> Option<&(usize, NaiveDate)> {
        self.days.iter().find(|(marker, _)| *marker >= row)
    }

    // the day to pin over a list this high, if any. The pin takes the top
    // row, so it's for the row under that, once that's a message whose
    // day's marker is off screen.
    fn pinned_day(&self, offset: usize, height: u16) -> Option<NaiveDate> {
        let top = offset + usize::from(height) - 1;
        let under = top.checked_sub(1)?;

        let (marker, day) = self.marker_over(under)?;
        let on_blank = self.days.iter().any(|(marker, _)| marker + 1 == under);

        (*marker > top && !on_blank).then_some(*day)
    }

    // draws the list and its ready pictures, returning the picture sources
    // that were drawn for the first time and need fetching, and where the
    // message text landed
    pub fn render(
        mut self,
        area: Rect,
        buf: &mut Buffer,
        state: &mut ListState,
    ) -> (Vec<PictureSource>, TextMap<TimelineEventItemId>) {
        let list = List::new(std::mem::take(&mut self.items))
            .highlight_symbol(HIGHLIGHT_SYMBOL)
            .direction(ListDirection::BottomToTop);

        StatefulWidget::render(&list, area, buf, state);

        // the list only makes room for the marker when something's selected
        let x = if state.selected().is_some() {
            HIGHLIGHT_SYMBOL.len() as u16
        } else {
            0
        };

        // once the top day's marker scrolls off, its day stays pinned above
        // a list one row shorter, so it can't hide the cursor
        let mut area = area;

        if area.height > 1
            && let Some(day) = self.pinned_day(state.offset(), area.height)
        {
            let pin = Rect::new(area.x, area.y, area.width, 1);
            area = Rect::new(area.x, area.y + 1, area.width, area.height - 1);

            Clear.render(pin.union(area), buf);
            StatefulWidget::render(&list, area, buf, state);

            // the day of the list's top row, as the list may have scrolled
            // to keep the cursor showing
            let top = state.offset() + usize::from(area.height) - 1;
            let day = self.marker_over(top).map_or(day, |(_, day)| *day);

            let line = Rect::new(pin.x + x, pin.y, self.width as u16, 1);
            day_line(day, self.width).render(line.intersection(pin), buf);
        }

        // every row is one line high, so list rows map straight to screen
        // rows, counting up from the bottom
        let offset = state.offset();
        let visible = offset..offset + area.height as usize;

        let mut map = TextMap::new(area);

        for row in visible.clone() {
            if let Some(Some(body)) = self.bodies.get(row) {
                let y = area.bottom() - 1 - (row - offset) as u16;
                map.add_row(area.x + x, y, body);
            }
        }

        // the rows of messages partly scrolled off draw text too
        for (row, body) in self.bodies.iter().enumerate() {
            if let Some(body) = body
                && !visible.contains(&row)
            {
                map.add_drawn(body);
            }
        }

        let Some(picker) = self.picker else {
            return (vec![], map);
        };

        let mut fetches = vec![];

        for placement in self.placements {
            if placement.rows.end <= visible.start || placement.rows.start >= visible.end {
                continue;
            }

            let uri = source_uri(&placement.source.media);

            if self.pictures.request(uri) {
                fetches.push(placement.source.clone());
            }

            let Some((top, size)) = placement.picture else {
                continue;
            };

            let Some(protocol) = self.pictures.protocol(uri, picker, size) else {
                continue;
            };

            // may be above the area, if the top has scrolled off
            let y = area.height as i64 - 1 - (top as i64 - offset as i64);
            let position = SignedPosition::from((x as i16, y as i16));

            SlicedImage::new(protocol, position).render(area, buf);

            // the part on screen, for clicking
            let clamp = |row: i64| row.clamp(area.top().into(), area.bottom().into()) as u16;
            let top = clamp(i64::from(area.y) + y);
            let bottom = clamp(i64::from(area.y) + y + i64::from(size.height));
            let cells = Rect::new(area.x + x, top, size.width, bottom - top).intersection(area);

            map.add_picture(cells, placement.message);
        }

        (fetches, map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::{self, tests::copied};
    use crate::widgets::chat::make_message_list;
    use crate::widgets::chat::tests::timeline_items;
    use crate::widgets::message::tests::{
        image_event, join_event, reaction_event, reply_event, text_event, thread_event,
    };
    use chrono::NaiveDate;
    use image::DynamicImage;
    use matrix_sdk::ruma::exports::serde_json::{Value, json};
    use matrix_sdk::ruma::{OwnedMxcUri, owned_mxc_uri};
    use matrix_sdk_ui::timeline::TimelineEventItemId;
    use ratatui::style::Color;
    use ratatui_image::FontSize;
    use ratatui_image::picker::ProtocolType;

    const WIDTH: u16 = 40;
    const HEIGHT: u16 = 10;

    // what Kitty draws a picture with
    const PLACEHOLDER: char = '\u{10EEEE}';

    struct Drawn {
        // each screen row as text, top to bottom, with picture cells (Kitty's
        // placeholders, or halfblocks' coloured cells) as #
        rows: Vec<String>,
        line_types: Vec<LineType>,
        fetches: Vec<OwnedMxcUri>,
        buf: Buffer,
        map: TextMap<TimelineEventItemId>,
    }

    // draws the timeline for some events with the given row selected
    async fn draw_with(
        events: Vec<Value>,
        selected: usize,
        height: u16,
        pictures: &mut Pictures,
        picker: Option<&Picker>,
    ) -> Drawn {
        let items = timeline_items(events).await;
        let messages = make_message_list(&items, &vec![], "", None);

        draw_messages(&messages, selected, height, pictures, picker)
    }

    fn draw_messages(
        messages: &[TimelineItem],
        selected: usize,
        height: u16,
        pictures: &mut Pictures,
        picker: Option<&Picker>,
    ) -> Drawn {
        let area = Rect::new(0, 0, WIDTH, height);
        let mut buf = Buffer::empty(area);
        let mut line_types = vec![];
        let mut state = ListState::default().with_selected(Some(selected));

        let (fetches, map) = Timeline::new(
            messages,
            (WIDTH - 2).into(),
            &mut line_types,
            pictures,
            picker,
        )
        .render(area, &mut buf, &mut state);

        let rows = (0..height)
            .map(|y| {
                let row: String = (0..WIDTH)
                    .map(|x| match &buf[(x, y)] {
                        c if c.symbol().contains(PLACEHOLDER) => "#",
                        c if matches!(c.bg, Color::Rgb(..)) => "#",
                        c => c.symbol(),
                    })
                    .collect();

                without_time(row.trim_end())
            })
            .collect();

        Drawn {
            rows,
            line_types,
            fetches: fetches
                .iter()
                .map(|s| source_uri(&s.media))
                .cloned()
                .collect(),
            buf,
            map,
        }
    }

    async fn draw(events: Vec<Value>, selected: usize) -> (Vec<String>, Vec<LineType>) {
        let drawn = draw_with(events, selected, HEIGHT, &mut Pictures::default(), None).await;
        (drawn.rows, drawn.line_types)
    }

    #[allow(deprecated)]
    fn kitty() -> Picker {
        let mut picker = Picker::from_fontsize(FontSize::new(10, 20));
        picker.set_protocol_type(ProtocolType::Kitty);
        picker
    }

    // a small image, which shows its original
    fn dog(caption: Option<&str>) -> Value {
        match caption {
            Some(caption) => image_event(caption, Some("dog.jpg"), Some(1000), None, None),
            None => image_event("dog.jpg", None, Some(1000), None, None),
        }
    }

    fn dog_uri() -> OwnedMxcUri {
        owned_mxc_uri!("mxc://example.org/image")
    }

    // the dog's picture, ready at the given pixel size
    fn ready(width: u32, height: u32) -> Pictures {
        let mut pictures = Pictures::default();
        pictures.insert(dog_uri(), Some(DynamicImage::new_rgb8(width, height)));
        pictures
    }

    fn picture_rows(rows: &[String]) -> Vec<usize> {
        (0..rows.len()).filter(|&i| rows[i].contains('#')).collect()
    }

    // swap an author line's "4:00 PM", which depends on the time zone, for
    // a stable placeholder
    fn without_time(row: &str) -> String {
        if !row.ends_with(" AM") && !row.ends_with(" PM") {
            return row.to_string();
        }

        let author = row.rsplitn(3, ' ').last().unwrap();
        format!("{} <time>", author)
    }

    fn bottom(rows: &[String], n: usize) -> &[String] {
        &rows[rows.len() - n..]
    }

    #[tokio::test]
    async fn text_message_lands_on_the_bottom_rows() {
        let events = vec![text_event("$text:example.org", 1000, "hello")];

        // the newest row sits at the bottom, with the author above it
        let (rows, line_types) = draw(events.clone(), 0).await;

        assert_eq!(
            bottom(&rows, 3),
            ["", "  @alice:example.org <time>", "> hello"]
        );

        assert!(matches!(line_types[0], LineType::MessageContent));
        assert!(matches!(line_types[1], LineType::MessageStart(_)));
        assert!(matches!(line_types[2], LineType::DeadSpace));

        // moving the selection up one row lands on the author line
        let (rows, _) = draw(events, 1).await;

        assert_eq!(bottom(&rows, 2), ["> @alice:example.org <time>", "  hello"]);
    }

    #[tokio::test]
    async fn newer_messages_sit_below_older_ones() {
        let (rows, _) = draw(
            vec![
                text_event("$older:example.org", 1000, "older"),
                text_event("$newer:example.org", 2000, "newer"),
            ],
            0,
        )
        .await;

        assert_eq!(
            bottom(&rows, 6),
            [
                "",
                "  @alice:example.org <time>",
                "  older",
                "",
                "  @alice:example.org <time>",
                "> newer",
            ]
        );
    }

    #[tokio::test]
    async fn long_text_wraps_to_the_width() {
        let events = vec![text_event("$text:example.org", 1000, &"word ".repeat(20))];
        let (rows, line_types) = draw(events.clone(), 0).await;

        assert_eq!(
            bottom(&rows, 4),
            [
                "  @alice:example.org <time>",
                "  word word word word word word word",
                "  word word word word word word word",
                "> word word word word word word",
            ]
        );

        // one line type per row: three body rows, the author, the spacer
        assert_eq!(line_types.len(), 5);

        // moving the selection up steps through the body a row at a time,
        // then onto the author line
        for selected in 1..=3 {
            let (rows, _) = draw(events.clone(), selected).await;
            let marker = rows.iter().position(|row| row.starts_with("> "));

            assert_eq!(marker, Some(rows.len() - 1 - selected));
        }
    }

    #[tokio::test]
    async fn reactions_show_under_the_body() {
        let (rows, _) = draw(
            vec![
                text_event("$text:example.org", 1000, "hello"),
                reaction_event("$reaction:example.org", 2000, "$text:example.org", "👍"),
            ],
            0,
        )
        .await;

        assert_eq!(
            bottom(&rows, 3),
            [
                "  @alice:example.org <time>",
                "  hello",
                "> 👍  @alice:example.org",
            ]
        );
    }

    #[tokio::test]
    async fn replies_are_indented_with_a_gutter() {
        let (rows, line_types) = draw(
            vec![
                text_event("$text:example.org", 1000, "hello"),
                reply_event(
                    "$reply:example.org",
                    2000,
                    "$text:example.org",
                    "> <@alice:example.org> hello\n\nhi back",
                ),
            ],
            0,
        )
        .await;

        // the quoted header is dropped, and the reply's spacer opens the gutter
        assert_eq!(
            bottom(&rows, 5),
            [
                "  @alice:example.org <time>",
                "  hello",
                "  ╷",
                "  │ @bob:example.org <time>",
                "> │ hi back",
            ]
        );

        // the reply's author line starts a message of its own, so it can be
        // selected and acted on separately
        assert!(
            matches!(&line_types[1], LineType::MessageStart(TimelineEventItemId::EventId(id)) if id == "$reply:example.org")
        );
    }

    #[tokio::test]
    async fn threads_show_a_reply_count() {
        let (rows, _) = draw(
            vec![
                text_event("$root:example.org", 1000, "root"),
                thread_event("$thread:example.org", 2000, "$root:example.org", None),
            ],
            0,
        )
        .await;

        assert_eq!(
            bottom(&rows, 3),
            [
                "  @alice:example.org <time>",
                "  root",
                "> 1 reply (enter to view)",
            ]
        );
    }

    #[tokio::test]
    async fn room_events_sit_between_messages() {
        let (rows, line_types) = draw(
            vec![
                text_event("$older:example.org", 1000, "older"),
                join_event("$join:example.org", 1500, "Bob"),
                text_event("$newer:example.org", 2000, "newer"),
            ],
            0,
        )
        .await;

        assert_eq!(
            bottom(&rows, 7),
            [
                "  @alice:example.org <time>",
                "  older",
                "",
                "  Bob joined the room.",
                "",
                "  @alice:example.org <time>",
                "> newer",
            ]
        );

        assert!(matches!(line_types[3], LineType::RoomEvent));
        assert!(matches!(line_types[4], LineType::DeadSpace));
    }

    #[tokio::test]
    async fn emotes_show_their_action() {
        let (rows, _) = draw(
            vec![json!({
                "type": "m.room.message",
                "event_id": "$emote:example.org",
                "sender": "@alice:example.org",
                "origin_server_ts": 1000,
                "content": { "msgtype": "m.emote", "body": "waves" }
            })],
            0,
        )
        .await;

        assert_eq!(bottom(&rows, 2), ["  @alice:example.org <time>", "> waves"]);
    }

    fn poll(kind: &str) -> Value {
        let answer = |id: &str, text: &str| json!({ "id": id, "org.matrix.msc1767.text": text });

        json!({
            "type": "org.matrix.msc3381.poll.start",
            "event_id": "$poll:example.org",
            "sender": "@alice:example.org",
            "origin_server_ts": 1000,
            "content": {
                "org.matrix.msc3381.poll.start": {
                    "kind": kind,
                    "max_selections": 1,
                    "question": { "org.matrix.msc1767.text": "Where should we eat?" },
                    "answers": [answer("pizza", "Pizza"), answer("tacos", "Tacos")]
                },
                "org.matrix.msc1767.text": "Where should we eat?"
            }
        })
    }

    fn vote(id: &str, sender: &str, answer: &str) -> Value {
        json!({
            "type": "org.matrix.msc3381.poll.response",
            "event_id": id,
            "sender": sender,
            "origin_server_ts": 2000,
            "content": {
                "org.matrix.msc3381.poll.response": { "answers": [answer] },
                "m.relates_to": { "rel_type": "m.reference", "event_id": "$poll:example.org" }
            }
        })
    }

    fn poll_end() -> Value {
        json!({
            "type": "org.matrix.msc3381.poll.end",
            "event_id": "$end:example.org",
            "sender": "@alice:example.org",
            "origin_server_ts": 3000,
            "content": {
                "org.matrix.msc3381.poll.end": {},
                "org.matrix.msc1767.text": "Ended poll",
                "m.relates_to": { "rel_type": "m.reference", "event_id": "$poll:example.org" }
            }
        })
    }

    #[tokio::test]
    async fn polls_show_their_votes() {
        let votes = || {
            vec![
                vote("$a:example.org", "@alice:example.org", "pizza"),
                vote("$b:example.org", "@bob:example.org", "pizza"),
                vote("$c:example.org", "@carol:example.org", "tacos"),
            ]
        };

        let mut events = vec![poll("org.matrix.msc3381.poll.disclosed")];
        events.extend(votes());
        let (rows, _) = draw(events, 0).await;

        assert_eq!(
            bottom(&rows, 4),
            [
                "  📊  Where should we eat?",
                "  Pizza · 2",
                "  Tacos · 1",
                "> (open in another client to vote)"
            ]
        );

        // undisclosed polls hide the votes until they end
        let mut events = vec![poll("org.matrix.msc3381.poll.undisclosed")];
        events.extend(votes());
        let (rows, _) = draw(events.clone(), 0).await;

        assert_eq!(bottom(&rows, 3)[..2], ["  Pizza", "  Tacos"]);

        events.push(poll_end());
        let (rows, _) = draw(events, 0).await;

        assert_eq!(bottom(&rows, 3), ["  Pizza · 2", "  Tacos · 1", "> Ended"]);
    }

    #[tokio::test]
    async fn the_read_marker_draws_a_line_above_the_new_messages() {
        let items = timeline_items(vec![
            text_event("$older:example.org", 1000, "older"),
            text_event("$newer:example.org", 2000, "newer"),
        ])
        .await;

        let mut messages = make_message_list(&items, &vec![], "", None);
        messages.insert(1, TimelineItem::ReadMarker);

        let drawn = draw_messages(&messages, 0, HEIGHT, &mut Pictures::default(), None);

        assert_eq!(
            bottom(&drawn.rows, 6),
            [
                "  older",
                "",
                "  ──────────── new messages ────────────",
                "",
                "  @alice:example.org <time>",
                "> newer",
            ]
        );

        assert!(matches!(drawn.line_types[3], LineType::RoomEvent));
        assert!(matches!(drawn.line_types[4], LineType::DeadSpace));
    }

    // five messages from one day, under its marker, newest first
    async fn busy_day() -> Vec<TimelineItem> {
        let events = (1..=5)
            .map(|i| {
                text_event(
                    &format!("${i}:example.org"),
                    i * 1000,
                    &format!("message {i}"),
                )
            })
            .collect();

        let mut messages = make_message_list(&timeline_items(events).await, &vec![], "", None);
        messages.push(TimelineItem::DayMarker(some_day()));
        messages
    }

    fn some_day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2001, 9, 8).unwrap()
    }

    fn pinned() -> String {
        format!("  {:─^38}", " Sat, Sep 8, 2001 ")
    }

    #[tokio::test]
    async fn a_message_replied_to_on_a_later_day_shows_its_date() {
        // 2001-09-09, and two days later
        let rows = draw(
            vec![
                text_event("$old:example.org", 1_000_000_000_000, "old"),
                reply_event(
                    "$reply:example.org",
                    1_000_172_800_000,
                    "$old:example.org",
                    "hi",
                ),
            ],
            0,
        )
        .await
        .0;

        // the time's cut off at this width
        let dated: Vec<_> = rows.iter().filter(|r| r.contains(", 2001, ")).collect();

        assert_eq!(dated.len(), 1);
        assert!(dated[0].starts_with("  @alice:example.org Sep "));
        assert!(rows.iter().any(|r| r.ends_with("@bob:example.org <time>")));
    }

    #[tokio::test]
    async fn the_day_stays_pinned_once_its_marker_scrolls_off() {
        let messages = busy_day().await;

        // fifteen rows of messages, then the marker, in ten
        let drawn = draw_messages(&messages, 0, HEIGHT, &mut Pictures::default(), None);

        assert_eq!(drawn.rows[0], pinned());
        assert_eq!(drawn.rows[1], "");
        assert_eq!(drawn.rows[3], "  message 3");
        assert_eq!(drawn.rows[9], "> message 5");

        // the day is lighter than its line
        assert_eq!(drawn.buf[(2, 0)].fg, Color::DarkGray);
        assert_eq!(drawn.buf[(13, 0)].fg, Color::Gray);

        // with the marker showing, nothing's pinned
        let drawn = draw_messages(&messages, 0, 17, &mut Pictures::default(), None);

        assert_eq!(drawn.rows[0], "");
        assert_eq!(drawn.rows[1], pinned());
        assert_eq!(drawn.rows[3], "  @alice:example.org <time>");
    }

    #[tokio::test]
    async fn the_pinned_day_never_hides_the_cursor() {
        let messages = busy_day().await;

        // message 2 is on the tenth row up, which the pin would cover
        let drawn = draw_messages(&messages, 9, HEIGHT, &mut Pictures::default(), None);

        assert_eq!(drawn.rows[0], pinned());
        assert_eq!(drawn.rows[1], "> message 2");
    }

    #[tokio::test]
    async fn the_next_days_marker_replaces_the_pin() {
        let items = timeline_items(vec![
            text_event("$1:example.org", 1000, "one"),
            text_event("$2:example.org", 2000, "two"),
        ])
        .await;

        let mut messages = make_message_list(&items, &vec![], "", None);
        messages.insert(1, TimelineItem::DayMarker(some_day().succ_opt().unwrap()));
        messages.push(TimelineItem::DayMarker(some_day()));

        // the newer marker on the top row
        let drawn = draw_messages(&messages, 0, 4, &mut Pictures::default(), None);

        assert!(drawn.rows[0].contains(" Sun, Sep 9, 2001 "));
        assert!(!drawn.rows.iter().any(|row| row.contains("Sep 8")));

        // nothing's pinned until there's a row of the older day to go under
        // it: not over the marker's blank row, nor that and one more
        for height in [5, 6] {
            let drawn = draw_messages(&messages, 0, height, &mut Pictures::default(), None);

            assert!(!drawn.rows.iter().any(|row| row.contains("Sep 8")));
        }

        let drawn = draw_messages(&messages, 0, 7, &mut Pictures::default(), None);

        assert_eq!(drawn.rows[0], pinned());
        assert_eq!(drawn.rows[1], "  one");
        assert!(drawn.rows[3].contains(" Sun, Sep 9, 2001 "));
    }

    #[tokio::test]
    async fn selecting_an_older_row_scrolls_it_into_view() {
        let events: Vec<_> = (1..=5)
            .map(|i| {
                text_event(
                    &format!("${i}:example.org"),
                    i * 1000,
                    &format!("message {i}"),
                )
            })
            .collect();

        // five messages need fifteen rows; the oldest author line is row 13
        let (rows, line_types) = draw(events, 13).await;

        assert_eq!(line_types.len(), 15);
        assert!(
            matches!(&line_types[13], LineType::MessageStart(TimelineEventItemId::EventId(id)) if id == "$1:example.org")
        );

        assert_eq!(rows[0], "> @alice:example.org <time>");
        assert_eq!(rows[1], "  message 1");
        assert!(!rows.contains(&"  message 5".to_string()));
    }

    #[tokio::test]
    async fn ready_pictures_replace_the_title_line() {
        // a wide picture fills the width beside the marker column
        let drawn = draw_with(
            vec![dog(Some("Look at this dog"))],
            0,
            20,
            &mut ready(400, 50),
            Some(&kitty()),
        )
        .await;

        let picture = format!("  {}", "#".repeat(38));

        assert_eq!(
            bottom(&drawn.rows, 5),
            [
                "  @alice:example.org <time>".to_string(),
                picture.clone(),
                picture.clone(),
                picture,
                "> Look at this dog".to_string(),
            ]
        );

        assert!(!drawn.rows.iter().any(|row| row.contains("dog.jpg")));
        assert!(drawn.fetches.is_empty());
    }

    #[tokio::test]
    async fn tall_pictures_stop_at_ten_rows() {
        let drawn = draw_with(vec![dog(None)], 0, 20, &mut ready(400, 300), Some(&kitty())).await;
        let rows = picture_rows(&drawn.rows);

        // keeping their shape, so narrower than the width
        assert_eq!(rows, (10..20).collect::<Vec<_>>());

        for i in rows {
            assert_eq!(drawn.rows[i].matches('#').count(), 27);
        }

        assert_eq!(drawn.rows[9], "  @alice:example.org <time>");
    }

    #[tokio::test]
    async fn halfblock_pictures_take_the_same_cells() {
        let picker = Picker::halfblocks();
        let drawn = draw_with(vec![dog(None)], 0, 20, &mut ready(400, 300), Some(&picker)).await;
        let rows = picture_rows(&drawn.rows);

        assert_eq!(rows, (10..20).collect::<Vec<_>>());

        for i in rows {
            assert_eq!(drawn.rows[i].matches('#').count(), 27);
        }
    }

    #[tokio::test]
    async fn pictures_that_are_not_ready_show_the_title_line() {
        let mut loading = Pictures::default();
        loading.request(&dog_uri());

        let mut failed = Pictures::default();
        failed.insert(dog_uri(), None);

        for mut pictures in [loading, failed] {
            let drawn = draw_with(vec![dog(None)], 0, 10, &mut pictures, Some(&kitty())).await;

            assert_eq!(
                bottom(&drawn.rows, 2),
                ["  @alice:example.org <time>", "> Image: dog.jpg (1000 B)"]
            );

            assert!(picture_rows(&drawn.rows).is_empty());
            assert!(drawn.fetches.is_empty());
        }
    }

    #[tokio::test]
    async fn pictures_with_dimensions_hold_their_rows_while_loading() {
        // the event's 400×100 fits in five rows
        let event = image_event("dog.jpg", None, Some(1000), None, Some((400, 100)));
        let mut pictures = Pictures::default();
        let picker = kitty();

        // the first draw asks for the picture and already leaves its rows,
        // without the title line
        let loading = draw_with(vec![event.clone()], 0, 20, &mut pictures, Some(&picker)).await;

        assert_eq!(loading.fetches, [dog_uri()]);
        assert_eq!(loading.rows[14], "  @alice:example.org <time>");
        assert!(picture_rows(&loading.rows).is_empty());
        assert!(!loading.rows.iter().any(|row| row.contains("dog.jpg")));

        // the decoded picture is taller than the event said, which alone
        // would take six rows, but it keeps the five
        pictures.insert(dog_uri(), Some(DynamicImage::new_rgb8(400, 120)));
        let ready = draw_with(vec![event], 0, 20, &mut pictures, Some(&picker)).await;

        assert_eq!(ready.rows[14], "  @alice:example.org <time>");
        assert_eq!(picture_rows(&ready.rows), (15..20).collect::<Vec<_>>());
        assert_eq!(ready.line_types.len(), loading.line_types.len());
    }

    #[tokio::test]
    async fn failed_pictures_with_dimensions_show_the_title_line() {
        let event = image_event("dog.jpg", None, Some(1000), None, Some((400, 100)));
        let mut pictures = Pictures::default();
        pictures.insert(dog_uri(), None);

        let drawn = draw_with(vec![event], 0, 10, &mut pictures, Some(&kitty())).await;

        assert_eq!(
            bottom(&drawn.rows, 2),
            ["  @alice:example.org <time>", "> Image: dog.jpg (1000 B)"]
        );
    }

    #[tokio::test]
    async fn stickers_show_as_pictures() {
        let sticker = json!({
            "type": "m.sticker",
            "event_id": "$sticker:example.org",
            "sender": "@alice:example.org",
            "origin_server_ts": 1000,
            "content": {
                "body": "Thumbs up",
                "url": "mxc://example.org/image",
                "info": { "size": 1000, "w": 400, "h": 100 }
            }
        });

        let drawn = draw_with(
            vec![sticker.clone()],
            0,
            10,
            &mut Pictures::default(),
            Some(&kitty()),
        )
        .await;

        assert_eq!(drawn.fetches, [dog_uri()]);

        let titled = draw_with(vec![sticker], 0, 10, &mut Pictures::default(), None).await;

        assert_eq!(
            bottom(&titled.rows, 2),
            ["  @alice:example.org <time>", "> Image: Thumbs up (1000 B)"]
        );
    }

    #[tokio::test]
    async fn without_a_picker_pictures_stay_titles() {
        let drawn = draw_with(vec![dog(None)], 0, 10, &mut ready(400, 50), None).await;

        assert_eq!(
            bottom(&drawn.rows, 2),
            ["  @alice:example.org <time>", "> Image: dog.jpg (1000 B)"]
        );

        assert!(drawn.fetches.is_empty());
    }

    #[tokio::test]
    async fn pictures_scrolled_partly_off_show_their_visible_rows() {
        let events = vec![dog(None), text_event("$text:example.org", 2000, "hello")];
        let mut pictures = ready(400, 300);
        let picker = kitty();

        // the newer message pushes the picture's top off the screen
        let drawn = draw_with(events.clone(), 0, 8, &mut pictures, Some(&picker)).await;

        assert_eq!(picture_rows(&drawn.rows), [0, 1, 2, 3, 4]);
        assert_eq!(
            bottom(&drawn.rows, 2),
            ["  @alice:example.org <time>", "> hello"]
        );

        // selecting the picture's author scrolls its bottom off instead
        let drawn = draw_with(events, 13, 8, &mut pictures, Some(&picker)).await;

        assert_eq!(drawn.rows[0], "> @alice:example.org <time>");
        assert_eq!(picture_rows(&drawn.rows), [1, 2, 3, 4, 5, 6, 7]);
    }

    #[tokio::test]
    async fn the_cells_a_picture_shows_in_open_its_message() {
        let events = vec![dog(None), text_event("$text:example.org", 2000, "hello")];
        let mut pictures = ready(400, 300);
        let picker = kitty();
        let dog_id = TimelineEventItemId::EventId("$image:example.org".try_into().unwrap());

        // with its top scrolled off, then its bottom
        for selected in [0, 13] {
            let drawn = draw_with(events.clone(), selected, 8, &mut pictures, Some(&picker)).await;

            for (y, row) in drawn.rows.iter().enumerate() {
                for x in 0..WIDTH {
                    let shown = row.chars().nth(x.into()) == Some('#');
                    let picture = drawn.map.picture(x, y as u16);

                    assert_eq!(picture, shown.then(|| dog_id.clone()), "at ({x}, {y})");
                }
            }
        }
    }

    #[tokio::test]
    async fn the_cursor_steps_through_picture_rows() {
        let mut pictures = ready(400, 300);
        let picker = kitty();

        for selected in 0..10 {
            let drawn =
                draw_with(vec![dog(None)], selected, 20, &mut pictures, Some(&picker)).await;
            let marker = drawn.rows.iter().position(|row| row.starts_with("> "));

            assert_eq!(marker, Some(19 - selected));
            assert!(matches!(
                drawn.line_types[selected],
                LineType::MessageContent
            ));
        }
    }

    #[tokio::test]
    async fn drawing_an_unfetched_picture_asks_for_it_once() {
        let mut pictures = Pictures::default();
        let picker = kitty();

        // it shows the title line while it loads
        let drawn = draw_with(vec![dog(None)], 0, 10, &mut pictures, Some(&picker)).await;

        assert_eq!(drawn.fetches, [dog_uri()]);
        assert_eq!(bottom(&drawn.rows, 1), ["> Image: dog.jpg (1000 B)"]);

        let drawn = draw_with(vec![dog(None)], 0, 10, &mut pictures, Some(&picker)).await;
        assert!(drawn.fetches.is_empty());
    }

    #[tokio::test]
    async fn pictures_off_screen_are_not_fetched() {
        let events = vec![dog(None), text_event("$text:example.org", 2000, "hello")];
        let drawn = draw_with(events, 0, 3, &mut Pictures::default(), Some(&kitty())).await;

        assert!(drawn.fetches.is_empty());
    }

    fn at(drawn: &Drawn, text: &str) -> (u16, u16) {
        selection::tests::at(&drawn.buf, text)
    }

    fn end(drawn: &Drawn, text: &str) -> (u16, u16) {
        selection::tests::end(&drawn.buf, text)
    }

    // what a drag from the start of one text to the end of another copies
    fn drag(drawn: &Drawn, from: &str, to: &str) -> String {
        copied(&drawn.map, at(drawn, from), end(drawn, to)).unwrap()
    }

    async fn draw_text(body: &str) -> Drawn {
        let events = vec![text_event("$text:example.org", 1000, body)];
        draw_with(events, 0, 20, &mut Pictures::default(), None).await
    }

    fn hits(drawn: &Drawn, text: &str) -> bool {
        let (x, y) = at(drawn, text);
        drawn.map.hit(x, y).is_some()
    }

    #[tokio::test]
    async fn a_wrapped_paragraph_copies_without_the_wrap() {
        let drawn =
            draw_text("alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima")
                .await;

        assert_ne!(at(&drawn, "echo").1, at(&drawn, "india").1);
        assert_eq!(
            drag(&drawn, "echo", "india"),
            "echo foxtrot golf hotel india"
        );
    }

    #[tokio::test]
    async fn a_selection_across_paragraphs_keeps_the_blank_line() {
        let drawn = draw_text("first paragraph\n\nsecond paragraph").await;

        assert_eq!(drag(&drawn, "paragraph", "second"), "paragraph\n\nsecond");
    }

    #[tokio::test]
    async fn lists_copy_their_source_without_their_bullets() {
        let drawn = draw_text("- one\n- two\n- three").await;

        assert_eq!(drag(&drawn, "one", "three"), "- one\n- two\n- three");
        assert!(!hits(&drawn, "•"));
    }

    #[tokio::test]
    async fn quotes_copy_their_source_without_their_marks() {
        let drawn = draw_text("> quoted\n> lines").await;

        assert_eq!(drag(&drawn, "quoted", "lines"), "> quoted\n> lines");
        assert!(!hits(&drawn, "▐"));
    }

    #[tokio::test]
    async fn inline_code_copies_its_source() {
        let drawn = draw_text("run `ls -l` now").await;

        assert_eq!(drag(&drawn, "run", "now"), "run `ls -l` now");
        assert_eq!(drag(&drawn, "ls", "-l"), "`ls -l`");
    }

    #[tokio::test]
    async fn code_blocks_copy_their_source() {
        let drawn = draw_text("```\nlet x = 1;\nlet y = 2;\n```").await;

        assert_eq!(drag(&drawn, "x = 1", "2;"), "x = 1;\nlet y = 2;");
    }

    #[tokio::test]
    async fn tables_copy_their_source_without_their_borders() {
        let drawn = draw_text("| apple | berry |\n|---|---|\n| cherry | damson |").await;

        assert_eq!(
            drag(&drawn, "apple", "damson"),
            "| apple | berry |\n|---|---|\n| cherry | damson |"
        );
        assert!(!hits(&drawn, "│"));
    }

    // a message that came out wrong before copies took hidden markdown
    const FORECAST: &str = "Good morning, everyone—happy Saturday! ☕️\n\n\
        It’s **51.6°F** now, with a high of **56°F**; expect cloudy skies and \
        light rain, with rain most likely around 9:45–10:30am and up to a 95% \
        chance today.\n\n\
        * At 7am Chase has PAC.\n\
        * ☀️ At 7:08am the sun rises.\n\
        * At 8:15am Gwen has Irvington Doubles.";

    #[tokio::test]
    async fn a_copy_takes_the_hidden_markdown_around_it() {
        let drawn = draw_with(
            vec![text_event("$text:example.org", 1000, FORECAST)],
            0,
            30,
            &mut Pictures::default(),
            None,
        )
        .await;

        // a list item's bullet, whether or not the drag leaves the item
        assert_eq!(drag(&drawn, "At 7am", "PAC."), "* At 7am Chase has PAC.");
        assert_eq!(
            drag(&drawn, "At 7am", "rises."),
            "* At 7am Chase has PAC.\n* ☀️ At 7:08am the sun rises."
        );

        // both ends of bold text
        let (x, y) = at(&drawn, "51.6°F");
        assert_eq!(
            copied(&drawn.map, (x, y), (x + 5, y)).unwrap(),
            "**51.6°F**"
        );

        let (to, y) = at(&drawn, "56°F");
        assert_eq!(
            copied(&drawn.map, (x, y), (to + 3, y)).unwrap(),
            "**51.6°F** now, with a high of **56°F**"
        );

        // nothing hidden, nothing added
        assert_eq!(
            drag(&drawn, "now,", "skies"),
            "now, with a high of **56°F**; expect cloudy skies"
        );
    }

    #[tokio::test]
    async fn a_copy_doesnt_take_text_scrolled_off_screen() {
        let long = "one two three four five six seven eight nine ten eleven twelve \
                    thirteen fourteen fifteen sixteen seventeen eighteen nineteen";
        let events = vec![
            text_event("$long:example.org", 1000, long),
            text_event("$newer:example.org", 2000, "newer"),
        ];

        // the newer message's three rows, then the long one's last two
        let drawn = draw_with(events, 0, 5, &mut Pictures::default(), None).await;
        let top = drawn.rows[0].trim();
        let first = top.split(' ').next().unwrap();
        let last = top.split(' ').next_back().unwrap();

        assert!(!drawn.rows.iter().any(|row| row.contains("one two")));
        assert_eq!(drag(&drawn, first, last), top);
    }

    #[tokio::test]
    async fn a_copy_doesnt_take_lines_cut_off_by_the_line_limit() {
        // one source line, wrapped past message_line_limit's 20 rows. A
        // notice takes the plain path, which trims the space at each wrap,
        // so nothing drawn stops the copy short of the cut-off rows.
        let long = (1..=200)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");

        let events = vec![notice_event("$notice:example.org", 1000, &long)];
        let drawn = draw_with(events, 0, 20, &mut Pictures::default(), None).await;
        let overflow = drawn
            .rows
            .iter()
            .position(|r| r.contains("overflow"))
            .unwrap();
        let last = drawn.rows[overflow - 1].trim().trim_start_matches("> ");
        let first = last.split(' ').next().unwrap();
        let end_word = last.split(' ').next_back().unwrap();

        // with the trimmed spaces on either side, but nothing more
        assert_eq!(drag(&drawn, first, end_word), format!(" {last} "));
    }

    #[tokio::test]
    async fn a_replys_gutter_cant_be_selected_and_its_text_copies_from_it() {
        let drawn = draw_with(
            vec![
                text_event("$text:example.org", 1000, "hello"),
                reply_event(
                    "$reply:example.org",
                    2000,
                    "$text:example.org",
                    "> <@alice:example.org> hello\n\nhi back",
                ),
            ],
            0,
            HEIGHT,
            &mut Pictures::default(),
            None,
        )
        .await;

        assert!(!hits(&drawn, "│ hi"));
        assert_eq!(drag(&drawn, "hi", "back"), "hi back");

        // dragged up into the message it replies to, it stops at its start
        assert_eq!(
            copied(&drawn.map, end(&drawn, "back"), at(&drawn, "hello")).unwrap(),
            "hi back"
        );
    }

    fn notice_event(id: &str, ts: u64, body: &str) -> Value {
        json!({
            "type": "m.room.message",
            "event_id": id,
            "sender": "@alice:example.org",
            "origin_server_ts": ts,
            "content": { "msgtype": "m.notice", "body": body }
        })
    }

    #[tokio::test]
    async fn the_plain_path_maps_the_same_way() {
        let notice = notice_event("$notice:example.org", 1000, "a notice here");
        let text = text_event("$text:example.org", 2000, "hello world");

        let drawn = draw_with(
            vec![notice.clone()],
            0,
            HEIGHT,
            &mut Pictures::default(),
            None,
        )
        .await;
        assert_eq!(drag(&drawn, "notice", "here"), "notice here");

        // during a search, which splits the line around its matches
        let items = timeline_items(vec![notice, text]).await;
        let messages = make_message_list(&items, &vec![], "wor", None);
        let drawn = draw_messages(&messages, 0, HEIGHT, &mut Pictures::default(), None);

        assert_eq!(drag(&drawn, "hello", "world"), "hello world");
    }

    #[tokio::test]
    async fn a_pictures_caption_maps_into_its_text() {
        let drawn = draw_with(
            vec![dog(Some("Look at this dog"))],
            0,
            20,
            &mut ready(400, 50),
            Some(&kitty()),
        )
        .await;

        assert_eq!(drag(&drawn, "this", "dog"), "this dog");
    }

    #[tokio::test]
    async fn an_emoji_maps_both_its_cells_and_the_text_after_lines_up() {
        let drawn = draw_text("a 😀 b").await;
        let (x, y) = at(&drawn, "😀");

        assert_eq!(x, 4);
        assert_eq!(drawn.map.hit(x, y), drawn.map.hit(x + 1, y));
        assert_eq!(
            copied(&drawn.map, (x + 1, y), at(&drawn, "b")).unwrap(),
            "😀 b"
        );
        assert_eq!(drag(&drawn, "b", "b"), "b");
    }

    #[tokio::test]
    async fn a_scrolled_list_maps_rows_to_their_messages() {
        let events: Vec<_> = (1..=5)
            .map(|i| {
                text_event(
                    &format!("${i}:example.org"),
                    i * 1000,
                    &format!("message {i}"),
                )
            })
            .collect();

        // scrolled up to the oldest, as in selecting_an_older_row_scrolls_it_into_view
        let drawn = draw_with(events, 13, HEIGHT, &mut Pictures::default(), None).await;

        for text in ["message 1", "message 2", "message 3"] {
            assert_eq!(drag(&drawn, text, text), text);
        }
    }
}
