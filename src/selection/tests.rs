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

/// Whether every cell of an ASCII text on screen has its colours reversed.
pub(crate) fn highlighted(buf: &Buffer, text: &str) -> bool {
    let (x, y) = at(buf, text);

    (x..x + text.len() as u16).all(|x| buf[(x, y)].modifier.contains(Modifier::REVERSED))
}

/// What a drag from one cell to another copies.
pub(crate) fn copied<Id: Clone + PartialEq>(
    map: &TextMap<Id>,
    from: (u16, u16),
    to: (u16, u16),
) -> Option<String> {
    let mut selection = Selection::default();

    selection.mouse_event(map, &press(from));
    selection.mouse_event(map, &drag_to(to));

    match selection.mouse_event(map, &release(to)) {
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

    selection.mouse_event(&map, &press((2, 2)));
    selection.mouse_event(&map, &drag_to((5, 2)));
    selection.mouse_event(&map, &release((5, 2)));
    assert_eq!(selection.highlighted(), Some((&"a", 0..4)));

    // on text
    assert_eq!(selection.mouse_event(&map, &press((3, 3))), None);
    assert_eq!(selection.mouse_event(&map, &release((3, 3))), None);
    assert_eq!(selection, Selection::Idle);
}

#[test]
fn a_click_on_a_link_opens_it() {
    let map = map();
    let mut selection = Selection::default();

    // on the link's last character, "g"
    selection.mouse_event(&map, &press((24, 8)));

    assert_eq!(
        selection.mouse_event(&map, &release((24, 8))),
        Some(Action::Open("https://example.org".to_string()))
    );
    assert_eq!(selection, Selection::Idle);

    // but not on the space after it
    selection.mouse_event(&map, &press((25, 8)));
    assert_eq!(selection.mouse_event(&map, &release((25, 8))), None);
}

#[test]
fn a_click_on_a_picture_opens_its_message() {
    let mut map = map();
    map.add_picture(Rect::new(2, 4, 10, 2), "p");
    let mut selection = Selection::default();

    selection.mouse_event(&map, &press((11, 5)));

    assert_eq!(
        selection.mouse_event(&map, &release((11, 5))),
        Some(Action::OpenPicture("p"))
    );
    assert_eq!(selection, Selection::Idle);

    // but not after a drag
    selection.mouse_event(&map, &press((2, 4)));
    selection.mouse_event(&map, &drag_to((3, 4)));
    assert_eq!(selection.mouse_event(&map, &release((3, 4))), None);

    // or off it
    selection.mouse_event(&map, &press((12, 5)));
    assert_eq!(selection.mouse_event(&map, &release((12, 5))), None);
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
    let mut selection = Selection::Selected {
        message: "a",
        range: 0..4,
    };

    assert_eq!(selection.mouse_event(&map, &press((15, 2))), None);
    assert_eq!(selection, Selection::Idle);

    // and a drag from there selects nothing
    assert_eq!(selection.mouse_event(&map, &drag_to((4, 2))), None);
    assert_eq!(selection.mouse_event(&map, &release((4, 2))), None);
    assert_eq!(selection, Selection::Idle);
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
