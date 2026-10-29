//! Turns user IDs typed into a message into Matrix mentions, and turns the
//! mention pills of a sent message back into IDs for editing.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::LazyLock;

use matrix_sdk::ruma::events::Mentions;
use matrix_sdk::ruma::events::room::message::{FormattedBody, RoomMessageEventContent};
use matrix_sdk::ruma::matrix_uri::MatrixId;
use matrix_sdk::ruma::{MatrixToUri, OwnedUserId, UserId};
use regex::Regex;

// The server name has to end in a letter or digit, so a full stop after an ID
// isn't part of it.
static USER_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"@[A-Za-z0-9._=/+\-]+:(?:[A-Za-z0-9.\-]*[A-Za-z0-9]|\[[0-9A-Fa-f:.]+\])(?::[0-9]{1,5})?",
    )
    .unwrap()
});

static PILL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<a\s[^>]*?href\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a>"#).unwrap()
});

static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());

/// The user IDs typed into `text`, with where they are.
pub fn find_user_ids(text: &str) -> Vec<(Range<usize>, OwnedUserId)> {
    USER_ID
        .find_iter(text)
        // skip anything glued to the end of a word, like an email address
        .filter(|m| {
            text[..m.start()]
                .chars()
                .last()
                .is_none_or(|c| !c.is_alphanumeric() && !"._=/+-@".contains(c))
        })
        .filter_map(|m| UserId::parse(m.as_str()).ok().map(|id| (m.range(), id)))
        .collect()
}

/// Builds a text message from markdown, mentioning each ID found in `names`,
/// which holds the name to show for each member. The plain body shows that
/// name, and the HTML shows it as a pill.
pub fn message_content(
    text: &str,
    names: &HashMap<OwnedUserId, String>,
) -> RoomMessageEventContent {
    let mut body = String::new();
    let mut markdown = String::new();
    let mut mentioned = vec![];
    let mut last = 0;

    for (range, id) in find_user_ids(text) {
        let Some(name) = names.get(&id) else {
            continue;
        };

        body.push_str(&text[last..range.start]);
        body.push_str(name);
        markdown.push_str(&text[last..range.start]);
        markdown.push_str(&format!(
            "[{}](<{}>)",
            escape_markdown(name),
            id.matrix_to_uri()
        ));

        mentioned.push(id);
        last = range.end;
    }

    let content = if mentioned.is_empty() {
        RoomMessageEventContent::text_markdown(text)
    } else {
        body.push_str(&text[last..]);
        markdown.push_str(&text[last..]);

        match FormattedBody::markdown(&markdown) {
            Some(html) => RoomMessageEventContent::text_html(body, html.body),
            None => RoomMessageEventContent::text_plain(body),
        }
    };

    // always say who we mean, even nobody, so servers don't fall back to
    // matching display names
    content.add_mentions(Mentions::with_user_ids(mentioned))
}

/// Swaps the names of the user pills in `html` back to their IDs in `body`,
/// so editing a message keeps its mentions.
pub fn restore_user_ids(body: &str, html: &str) -> String {
    let mut pills: Vec<(String, OwnedUserId)> = vec![];

    for cap in PILL.captures_iter(html) {
        let Ok(uri) = MatrixToUri::parse(&cap[1]) else {
            continue;
        };

        let MatrixId::User(id) = uri.id() else {
            continue;
        };

        let name = unescape_html(&TAG.replace_all(&cap[2], ""));

        if !name.is_empty() && !pills.iter().any(|(n, _)| n == &name) {
            pills.push((name, id.clone()));
        }
    }

    // longest first, so "Sam Smith" wins over "Sam"
    pills.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));

    let mut ret = String::new();
    let mut i = 0;

    'outer: while let Some(c) = body[i..].chars().next() {
        let starts_word = body[..i]
            .chars()
            .last()
            .is_none_or(|c| !c.is_alphanumeric());

        if starts_word {
            for (name, id) in &pills {
                let end = i + name.len();

                if body[i..].starts_with(name.as_str())
                    && body[end..]
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_alphanumeric())
                {
                    ret.push_str(id.as_str());
                    i = end;
                    continue 'outer;
                }
            }
        }

        ret.push(c);
        i += c.len_utf8();
    }

    ret
}

fn escape_markdown(text: &str) -> String {
    let mut ret = String::new();

    for c in text.chars() {
        if c.is_ascii_punctuation() {
            ret.push('\\');
        }
        ret.push(c);
    }

    ret
}

fn unescape_html(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::events::room::message::MessageType::Text;
    use matrix_sdk::ruma::owned_user_id;

    use super::*;

    fn names(pairs: &[(&str, &str)]) -> HashMap<OwnedUserId, String> {
        pairs
            .iter()
            .map(|(id, name)| (UserId::parse(*id).unwrap(), name.to_string()))
            .collect()
    }

    fn bodies(content: &RoomMessageEventContent) -> (String, Option<String>) {
        let Text(text) = &content.msgtype else {
            panic!("not a text message");
        };

        (
            text.body.clone(),
            text.formatted.as_ref().map(|f| f.body.clone()),
        )
    }

    fn ids(found: Vec<(Range<usize>, OwnedUserId)>) -> Vec<String> {
        found.into_iter().map(|(_, id)| id.to_string()).collect()
    }

    #[test]
    fn it_finds_user_ids() {
        assert_eq!(
            ids(find_user_ids("hey @phil:kulak.us.")),
            vec!["@phil:kulak.us"]
        );
        assert_eq!(
            ids(find_user_ids("@a:b.org, (@c_d:e.org:8448)")),
            vec!["@a:b.org", "@c_d:e.org:8448"]
        );
        assert!(find_user_ids("mail bob@kulak.us:443 or @nobody").is_empty());
    }

    #[test]
    fn it_mentions_members() {
        let names = names(&[
            ("@phil:kulak.us", "Phil Kulak"),
            ("@sam:matrix.org", "Sam_Smith"),
        ]);

        let content = message_content("@phil:kulak.us and @sam:matrix.org, hi!", &names);
        let (body, html) = bodies(&content);

        assert_eq!(body, "Phil Kulak and Sam_Smith, hi!");
        assert_eq!(
            html.unwrap(),
            "<a href=\"https://matrix.to/#/@phil:kulak.us\">Phil Kulak</a> and \
             <a href=\"https://matrix.to/#/@sam:matrix.org\">Sam_Smith</a>, hi!"
        );
        assert_eq!(
            content.mentions.unwrap().user_ids,
            [
                owned_user_id!("@phil:kulak.us"),
                owned_user_id!("@sam:matrix.org")
            ]
            .into()
        );
    }

    #[test]
    fn it_leaves_non_members_alone() {
        let names = names(&[("@phil:kulak.us", "Phil Kulak")]);
        let content = message_content("ask @bob:example.org", &names);

        assert_eq!(bodies(&content), ("ask @bob:example.org".to_string(), None));
        assert!(content.mentions.unwrap().user_ids.is_empty());
    }

    #[test]
    fn it_keeps_markdown() {
        let names = names(&[("@phil:kulak.us", "Phil Kulak")]);
        let content = message_content("**hi** @phil:kulak.us", &names);
        let (body, html) = bodies(&content);

        assert_eq!(body, "**hi** Phil Kulak");
        assert_eq!(
            html.unwrap(),
            "<strong>hi</strong> <a href=\"https://matrix.to/#/@phil:kulak.us\">Phil Kulak</a>"
        );
    }

    #[test]
    fn it_restores_user_ids() {
        let html = "<a href=\"https://matrix.to/#/%40phil%3Akulak.us\">Phil Kulak</a> and \
                    <a href='https://matrix.to/#/@sam:matrix.org'>Sam &amp; Co</a>: \
                    <a href=\"https://example.com\">a link</a>";

        assert_eq!(
            restore_user_ids("Phil Kulak and Sam & Co: Phil Kulaks a link", html),
            "@phil:kulak.us and @sam:matrix.org: Phil Kulaks a link"
        );
    }

    #[test]
    fn it_restores_what_it_sends() {
        let names = names(&[
            ("@phil:kulak.us", "Phil"),
            ("@sam:matrix.org", "Phil Smith"),
        ]);
        let text = "@sam:matrix.org, meet @phil:kulak.us";
        let (body, html) = bodies(&message_content(text, &names));

        assert_eq!(restore_user_ids(&body, &html.unwrap()), text);
    }
}
