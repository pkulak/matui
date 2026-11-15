use super::*;
use crossterm::event::KeyModifiers;

pub(crate) fn mouse(kind: MouseEventKind, (x, y): (u16, u16)) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

pub(crate) fn press(at: (u16, u16)) -> MouseEvent {
    mouse(MouseEventKind::Down(MouseButton::Left), at)
}

pub(crate) fn drag_to(at: (u16, u16)) -> MouseEvent {
    mouse(MouseEventKind::Drag(MouseButton::Left), at)
}

pub(crate) fn release(at: (u16, u16)) -> MouseEvent {
    mouse(MouseEventKind::Up(MouseButton::Left), at)
}

/// The cell a text starts at on screen, topmost first.
pub(crate) fn at(buf: &Buffer, text: &str) -> (u16, u16) {
    let area = buf.area;

    for y in area.top()..area.bottom() {
        let mut row = String::new();
        let mut starts = vec![];

        for x in area.left()..area.right() {
            starts.push(row.len());
            row.push_str(buf[(x, y)].symbol());
        }

        if let Some(i) = row.find(text) {
            let x = starts.iter().position(|&s| s == i).unwrap();
            return (area.x + x as u16, y);
        }
    }

    panic!("{text:?} isn't on screen");
}

/// The cell an ASCII text ends at on screen, topmost first.
pub(crate) fn end(buf: &Buffer, text: &str) -> (u16, u16) {
    let (x, y) = at(buf, text);
    (x + text.len() as u16 - 1, y)
}

/// What a drag from one cell to another copies.
pub(crate) fn copied<Id: Clone + PartialEq>(
    map: &TextMap<Id>,
    from: (u16, u16),
    to: (u16, u16),
) -> Option<String> {
    let mut selection = Selection::default();
    let now = Instant::now();

    selection.mouse_event(map, &press(from), now);
    selection.mouse_event(map, &drag_to(to), now);

    match selection.mouse_event(map, &release(to), now) {
        Some(Action::Copy(text)) => Some(text),
        _ => None,
    }
}

const TEXT: &str = "hello world\nsecond line";
const LINKED: &str = "see https://example.org now";

/// "hello world" at (2, 2), "second line" under it, another message
/// further down, and one with a link at (2, 8).
fn map() -> TextMap<&'static str> {
    let mut map = TextMap::new(Rect::new(0, 0, 40, 10));

    for (y, range) in [(2, 0..11), (3, 12..23)] {
        let runs = vec![Run { column: 0, range }];
        map.add_row(
            2,
            y,
            &BodyRow {
                message: "a",
                text: TEXT,
                links: &[],
                runs,
            },
        );
    }

    let runs = vec![Run {
        column: 0,
        range: 0..5,
    }];
    map.add_row(
        2,
        6,
        &BodyRow {
            message: "b",
            text: "other",
            links: &[],
            runs,
        },
    );

    let links = links(LINKED);
    let runs = vec![Run {
        column: 0,
        range: 0..LINKED.len(),
    }];
    map.add_row(
        2,
        8,
        &BodyRow {
            message: "c",
            text: LINKED,
            links: &links,
            runs,
        },
    );

    map
}

#[test]
fn a_drag_copies_from_one_end_to_the_other() {
    assert_eq!(
        copied(&map(), (2, 2), (7, 3)).as_deref(),
        Some("hello world\nsecond")
    );
}

#[test]
fn a_backwards_drag_copies_the_same_text() {
    assert_eq!(
        copied(&map(), (7, 3), (2, 2)).as_deref(),
        Some("hello world\nsecond")
    );
}

#[test]
fn a_drag_that_leaves_the_message_clamps_to_it() {
    let map = map();

    // above the message, its start
    assert_eq!(copied(&map, (8, 2), (15, 0)).as_deref(), Some("hello w"));

    // below it, into the next message, its end
    assert_eq!(
        copied(&map, (8, 2), (3, 6)).as_deref(),
        Some("world\nsecond line")
    );

    // past either end of a row, that end
    assert_eq!(copied(&map, (8, 2), (19, 2)).as_deref(), Some("world"));
    assert_eq!(copied(&map, (9, 3), (0, 3)).as_deref(), Some("second l"));
}

#[test]
fn a_click_clears_the_selection_and_does_nothing() {
    let map = map();
    let mut selection = Selection::default();
    let now = Instant::now();

    selection.mouse_event(&map, &press((2, 2)), now);
    selection.mouse_event(&map, &drag_to((5, 2)), now);
    selection.mouse_event(&map, &release((5, 2)), now);
    assert_eq!(selection.highlighted(), Some((&"a", 0..4)));

    // on text
    assert_eq!(selection.mouse_event(&map, &press((3, 3)), now), None);
    assert_eq!(selection.mouse_event(&map, &release((3, 3)), now), None);
    assert_eq!(selection.state, State::Idle);
}

#[test]
fn a_click_on_a_link_opens_it() {
    let map = map();
    let mut selection = Selection::default();
    let now = Instant::now();

    // on the link's last character, "g"
    selection.mouse_event(&map, &press((24, 8)), now);

    assert_eq!(
        selection.mouse_event(&map, &release((24, 8)), now),
        Some(Action::Open("https://example.org".to_string()))
    );
    assert_eq!(selection.state, State::Idle);

    // but not on the space after it
    selection.mouse_event(&map, &press((25, 8)), now);
    assert_eq!(selection.mouse_event(&map, &release((25, 8)), now), None);
}

#[test]
fn a_click_on_a_picture_opens_its_message() {
    let mut map = map();
    map.add_picture(Rect::new(2, 4, 10, 2), "p");
    let mut selection = Selection::default();
    let now = Instant::now();

    selection.mouse_event(&map, &press((11, 5)), now);

    assert_eq!(
        selection.mouse_event(&map, &release((11, 5)), now),
        Some(Action::OpenPicture("p"))
    );
    assert_eq!(selection.state, State::Idle);

    // but not after a drag
    selection.mouse_event(&map, &press((2, 4)), now);
    selection.mouse_event(&map, &drag_to((3, 4)), now);
    assert_eq!(selection.mouse_event(&map, &release((3, 4)), now), None);

    // or off it
    selection.mouse_event(&map, &press((12, 5)), now);
    assert_eq!(selection.mouse_event(&map, &release((12, 5)), now), None);
}

/// What a press and release at a cell does.
fn click(
    selection: &mut Selection<&'static str>,
    map: &TextMap<&'static str>,
    at: (u16, u16),
    now: Instant,
) -> Option<Action<&'static str>> {
    selection.mouse_event(map, &press(at), now);
    selection.mouse_event(map, &release(at), now)
}

fn copy(text: &str) -> Option<Action<&'static str>> {
    Some(Action::Copy(text.to_string()))
}

#[test]
fn a_double_click_selects_and_copies_a_word() {
    let map = map();
    let mut selection = Selection::default();
    let now = Instant::now();

    // the "o" of "world", then its "d"
    assert_eq!(click(&mut selection, &map, (9, 2), now), None);
    assert_eq!(
        click(&mut selection, &map, (12, 2), now + MULTI_CLICK),
        copy("world")
    );
    assert_eq!(selection.highlighted(), Some((&"a", 6..11)));
}

#[test]
fn a_double_click_is_soon_after_on_the_same_word() {
    let map = map();
    let mut selection = Selection::default();
    let now = Instant::now();
    let late = now + MULTI_CLICK + Duration::from_millis(1);

    click(&mut selection, &map, (8, 2), now);
    assert_eq!(click(&mut selection, &map, (8, 2), late), None);
    assert_eq!(click(&mut selection, &map, (2, 2), late), None);

    // or after a drag
    selection.mouse_event(&map, &press((2, 2)), late);
    selection.mouse_event(&map, &drag_to((3, 2)), late);
    selection.mouse_event(&map, &release((3, 2)), late);
    assert_eq!(click(&mut selection, &map, (2, 2), late), None);
}

#[test]
fn a_drag_after_a_double_click_goes_a_word_at_a_time() {
    let map = map();
    let mut selection = Selection::default();
    let now = Instant::now();

    click(&mut selection, &map, (8, 2), now);
    selection.mouse_event(&map, &press((8, 2)), now);

    // into "second"
    selection.mouse_event(&map, &drag_to((4, 3)), now);
    assert_eq!(selection.highlighted(), Some((&"a", 6..18)));

    // then back past the word pressed on, which stays selected
    selection.mouse_event(&map, &drag_to((3, 2)), now);
    assert_eq!(
        selection.mouse_event(&map, &release((3, 2)), now),
        copy("hello world")
    );
}

#[test]
fn a_triple_click_selects_a_line_and_a_drag_goes_a_line_at_a_time() {
    let map = map();
    let mut selection = Selection::default();
    let now = Instant::now();

    click(&mut selection, &map, (4, 3), now);
    click(&mut selection, &map, (4, 3), now);
    assert_eq!(
        click(&mut selection, &map, (4, 3), now),
        copy("second line")
    );

    // a fourth is a first click again
    assert_eq!(click(&mut selection, &map, (4, 3), now), None);
    assert_eq!(selection.state, State::Idle);

    // so this is the second and third
    click(&mut selection, &map, (4, 3), now);
    selection.mouse_event(&map, &press((4, 3)), now);
    selection.mouse_event(&map, &drag_to((5, 2)), now);
    assert_eq!(
        selection.mouse_event(&map, &release((5, 2)), now),
        copy(TEXT)
    );
}

#[test]
fn words_join_across_paths_and_stop_at_spaces_and_punctuation() {
    let word_at = |text: &'static str, at: &str| &text[word(text, text.find(at).unwrap())];

    assert_eq!(
        word_at("open ~/src/pi+x\\y.rs, now", "pi"),
        "~/src/pi+x\\y.rs"
    );
    assert_eq!(word_at("a kebab-case_word here", "case"), "kebab-case_word");
    assert_eq!(word_at("hi,   there", " "), "   ");
    assert_eq!(word_at("hi,   there", ","), ",");
}

#[test]
fn links_are_http_and_https_urls() {
    let text = "a HTTPS://a.org b http://b.org c ftp://c.org d d@d.org e file:///etc";
    let found: Vec<_> = links(text).into_iter().map(|l| &text[l]).collect();

    assert_eq!(found, ["HTTPS://a.org", "http://b.org"]);
}

#[test]
fn a_drag_that_starts_on_a_link_selects() {
    assert_eq!(
        copied(&map(), (14, 8), (28, 8)).as_deref(),
        Some("example.org now")
    );
}

#[test]
fn a_press_off_body_text_clears_the_selection() {
    let map = map();
    let mut selection = Selection {
        state: State::Selected {
            message: "a",
            range: 0..4,
        },
        last_press: None,
    };
    let now = Instant::now();

    assert_eq!(selection.mouse_event(&map, &press((15, 2)), now), None);
    assert_eq!(selection.state, State::Idle);

    // and a drag from there selects nothing
    assert_eq!(selection.mouse_event(&map, &drag_to((4, 2)), now), None);
    assert_eq!(selection.mouse_event(&map, &release((4, 2)), now), None);
    assert_eq!(selection.state, State::Idle);
}

#[test]
fn text_past_the_lists_edge_isnt_mapped() {
    let mut map = TextMap::new(Rect::new(10, 0, 20, 10));
    let text = "a row that runs past the edge";
    let runs = vec![Run {
        column: 0,
        range: 0..text.len(),
    }];

    map.add_row(
        12,
        2,
        &BodyRow {
            message: "a",
            text,
            links: &[],
            runs,
        },
    );

    assert!(map.hit(29, 2).is_some());
    assert_eq!(map.hit(30, 2), None);
    assert_eq!(map.cells(&"a", 0..text.len()).count(), 18);
}

#[test]
fn only_slices_of_the_text_are_runs() {
    let text = "hello world";
    let owned = "hello".to_string();
    let spans = [
        Span::raw("│ "),
        Span::raw(owned.as_str()),
        Span::raw(&text[5..]),
    ];

    let row = BodyRow::new("a", text, &[], &spans).unwrap();

    assert_eq!(row.runs.len(), 1);
    assert_eq!(row.runs[0].column, 7);
    assert_eq!(row.runs[0].range, 5..11);
}
