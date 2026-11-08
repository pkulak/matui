use crate::widgets::chat::TimelineItem;
use crate::widgets::message::LineType;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{List, ListDirection, ListItem, ListState, StatefulWidget};

// the chat's message list, drawn bottom to top
pub struct Timeline<'a> {
    items: Vec<ListItem<'a>>,
}

impl<'a> Timeline<'a> {
    // lays out the visible messages, recording what each list row is in
    // line_types, so the caller can place the selection before drawing
    pub fn new(messages: &'a [TimelineItem], width: usize, line_types: &mut Vec<LineType>) -> Self {
        let mut items = vec![];

        // each item reverses its own rows, so collect them one at a time
        let mut buffer = vec![];

        for m in messages {
            items.append(&mut m.to_list_items(width, &mut buffer));
            line_types.append(&mut buffer);
        }

        Self { items }
    }
}

impl StatefulWidget for Timeline<'_> {
    type State = ListState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut ListState) {
        let list = List::new(self.items)
            .highlight_symbol("> ")
            .direction(ListDirection::BottomToTop);

        StatefulWidget::render(list, area, buf, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::chat::{OrderedEvent, make_message_list};
    use crate::widgets::message::tests::{
        join_event, reaction_event, reply_event, text_event, thread_event,
    };
    use crate::widgets::receipts::Receipts;
    use matrix_sdk::ruma::events::AnyTimelineEvent;
    use matrix_sdk::ruma::owned_user_id;
    use std::collections::BTreeSet;

    const WIDTH: u16 = 40;
    const HEIGHT: u16 = 10;

    // draws the timeline for some events with the given row selected,
    // returning each screen row as text, top to bottom
    fn draw(events: Vec<AnyTimelineEvent>, selected: usize) -> (Vec<String>, Vec<LineType>) {
        let timeline: BTreeSet<OrderedEvent> = events.into_iter().map(OrderedEvent::new).collect();
        let receipts = Receipts::new(owned_user_id!("@me:example.org"));
        let messages = make_message_list(&timeline, &vec![], &receipts, "", None);

        let area = Rect::new(0, 0, WIDTH, HEIGHT);
        let mut buf = Buffer::empty(area);
        let mut line_types = vec![];
        let mut state = ListState::default().with_selected(Some(selected));

        Timeline::new(&messages, (WIDTH - 2).into(), &mut line_types)
            .render(area, &mut buf, &mut state);

        let rows = (0..HEIGHT)
            .map(|y| {
                let row: String = (0..WIDTH).map(|x| buf[(x, y)].symbol()).collect();
                without_elapsed(row.trim_end())
            })
            .collect();

        (rows, line_types)
    }

    // swap an author line's "56 years ago" for a stable placeholder
    fn without_elapsed(row: &str) -> String {
        if !row.ends_with(" ago") {
            return row.to_string();
        }

        let author = row.rsplitn(4, ' ').last().unwrap();
        format!("{} <time>", author)
    }

    fn bottom(rows: &[String], n: usize) -> &[String] {
        &rows[rows.len() - n..]
    }

    #[test]
    fn text_message_lands_on_the_bottom_rows() {
        let events = vec![text_event("$text:example.org", 1000, "hello")];

        // the newest row sits at the bottom, with the author above it
        let (rows, line_types) = draw(events.clone(), 0);

        assert_eq!(
            bottom(&rows, 3),
            ["", "  @alice:example.org <time>", "> hello"]
        );

        assert!(matches!(line_types[0], LineType::MessageContent));
        assert!(matches!(line_types[1], LineType::MessageStart(_)));
        assert!(matches!(line_types[2], LineType::DeadSpace));

        // moving the selection up one row lands on the author line
        let (rows, _) = draw(events, 1);

        assert_eq!(bottom(&rows, 2), ["> @alice:example.org <time>", "  hello"]);
    }

    #[test]
    fn newer_messages_sit_below_older_ones() {
        let (rows, _) = draw(
            vec![
                text_event("$older:example.org", 1000, "older"),
                text_event("$newer:example.org", 2000, "newer"),
            ],
            0,
        );

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

    #[test]
    fn long_text_wraps_to_the_width() {
        let events = vec![text_event("$text:example.org", 1000, &"word ".repeat(20))];
        let (rows, line_types) = draw(events.clone(), 0);

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
            let (rows, _) = draw(events.clone(), selected);
            let marker = rows.iter().position(|row| row.starts_with("> "));

            assert_eq!(marker, Some(rows.len() - 1 - selected));
        }
    }

    #[test]
    fn reactions_show_under_the_body() {
        let (rows, _) = draw(
            vec![
                text_event("$text:example.org", 1000, "hello"),
                reaction_event("$reaction:example.org", 2000, "$text:example.org", "👍"),
            ],
            0,
        );

        assert_eq!(
            bottom(&rows, 3),
            [
                "  @alice:example.org <time>",
                "  hello",
                "> 👍  (+1) @alice:example.org",
            ]
        );
    }

    #[test]
    fn replies_are_indented_with_a_gutter() {
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
        );

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
        assert!(matches!(&line_types[1], LineType::MessageStart(id) if id == "$reply:example.org"));
    }

    #[test]
    fn threads_show_a_reply_count() {
        let (rows, _) = draw(
            vec![
                text_event("$root:example.org", 1000, "root"),
                thread_event("$thread:example.org", 2000, "$root:example.org", None),
            ],
            0,
        );

        assert_eq!(
            bottom(&rows, 3),
            [
                "  @alice:example.org <time>",
                "  root",
                "> 1 reply (enter to view)",
            ]
        );
    }

    #[test]
    fn room_events_sit_between_messages() {
        let (rows, line_types) = draw(
            vec![
                text_event("$older:example.org", 1000, "older"),
                join_event("$join:example.org", 1500, "Bob"),
                text_event("$newer:example.org", 2000, "newer"),
            ],
            0,
        );

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

    #[test]
    fn selecting_an_older_row_scrolls_it_into_view() {
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
        let (rows, line_types) = draw(events, 13);

        assert_eq!(line_types.len(), 15);
        assert!(matches!(&line_types[13], LineType::MessageStart(id) if id == "$1:example.org"));

        assert_eq!(rows[0], "> @alice:example.org <time>");
        assert_eq!(rows[1], "  message 1");
        assert!(!rows.contains(&"  message 5".to_string()));
    }
}
