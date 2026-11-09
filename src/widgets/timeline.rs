use crate::pictures::{Pictures, picture_source, source_uri};
use crate::widgets::chat::TimelineItem;
use crate::widgets::message::LineType;
use matrix_sdk::ruma::events::room::MediaSource;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::{List, ListDirection, ListItem, ListState, StatefulWidget, Widget};
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
}

// where a message with a picture source landed in the list
struct Placement {
    source: MediaSource,
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

        // each item reverses its own rows, so collect them one at a time
        let mut buffer = vec![];

        for m in messages {
            let source = picker
                .and(m.message())
                .and_then(|m| picture_source(&m.body));

            let size = picker
                .zip(source.as_ref())
                .and_then(|(picker, source)| pictures.size(source, picker, width as u16));

            items.append(&mut m.to_list_items(width, size.map(|s| s.height), &mut buffer));

            let start = line_types.len();
            line_types.append(&mut buffer);

            if let Some(source) = source {
                // the picture sits right under the author line, which is
                // the message's topmost start
                let picture = size.and_then(|size| {
                    let author = line_types[start..]
                        .iter()
                        .rposition(|l| matches!(l, LineType::MessageStart(_)))?;

                    Some((start + author - 1, size))
                });

                placements.push(Placement {
                    source: source.media,
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
        }
    }

    // draws the list and its ready pictures, returning the picture sources
    // that were drawn for the first time and need fetching
    pub fn render(self, area: Rect, buf: &mut Buffer, state: &mut ListState) -> Vec<MediaSource> {
        let list = List::new(self.items)
            .highlight_symbol(HIGHLIGHT_SYMBOL)
            .direction(ListDirection::BottomToTop);

        StatefulWidget::render(list, area, buf, state);

        let Some(picker) = self.picker else {
            return vec![];
        };

        // every row is one line high, so list rows map straight to screen
        // rows, counting up from the bottom
        let offset = state.offset();
        let visible = offset..offset + area.height as usize;

        // the list only makes room for the marker when something's selected
        let x = if state.selected().is_some() {
            HIGHLIGHT_SYMBOL.len() as i16
        } else {
            0
        };

        let mut fetches = vec![];

        for placement in self.placements {
            if placement.rows.end <= visible.start || placement.rows.start >= visible.end {
                continue;
            }

            let uri = source_uri(&placement.source);

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
            let position = SignedPosition::from((x, y as i16));

            SlicedImage::new(protocol, position).render(area, buf);
        }

        fetches
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::chat::make_message_list;
    use crate::widgets::chat::tests::timeline_items;
    use crate::widgets::message::tests::{
        image_event, join_event, reaction_event, reply_event, text_event, thread_event,
    };
    use image::DynamicImage;
    use matrix_sdk::ruma::exports::serde_json::Value;
    use matrix_sdk::ruma::{OwnedMxcUri, owned_mxc_uri};
    use matrix_sdk_ui::timeline::TimelineEventItemId;
    use ratatui_image::FontSize;
    use ratatui_image::picker::ProtocolType;

    const WIDTH: u16 = 40;
    const HEIGHT: u16 = 10;

    // what Kitty draws a picture with
    const PLACEHOLDER: char = '\u{10EEEE}';

    struct Drawn {
        // each screen row as text, top to bottom, with picture cells as #
        rows: Vec<String>,
        line_types: Vec<LineType>,
        fetches: Vec<OwnedMxcUri>,
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

        let area = Rect::new(0, 0, WIDTH, height);
        let mut buf = Buffer::empty(area);
        let mut line_types = vec![];
        let mut state = ListState::default().with_selected(Some(selected));

        let fetches = Timeline::new(
            &messages,
            (WIDTH - 2).into(),
            &mut line_types,
            pictures,
            picker,
        )
        .render(area, &mut buf, &mut state);

        let rows = (0..height)
            .map(|y| {
                let row: String = (0..WIDTH)
                    .map(|x| match buf[(x, y)].symbol() {
                        s if s.contains(PLACEHOLDER) => "#",
                        s => s,
                    })
                    .collect();

                without_elapsed(row.trim_end())
            })
            .collect();

        Drawn {
            rows,
            line_types,
            fetches: fetches.iter().map(source_uri).cloned().collect(),
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
                "> 👍  (+1) @alice:example.org",
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
}
