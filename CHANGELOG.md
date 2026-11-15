# Changelog

## [Unreleased]

- Drag across a message to select and copy part of its text. The copy is the message's own text, markdown included, without the line breaks Matui wraps at or the markers and gutters around it, and the selection stays inside the message. Double-click to select a word, or triple-click for a line, and drag from there to select a word or line at a time. The room's header says "Copied!", as it does for `y`. Shift+drag still selects anything else in most terminals. Links are underlined, and clicking one opens just that link.
- Click in the chat to move the cursor there, and click a picture to open it.
- Show a play button over video thumbnails in the chat.
- Click in a text field to move its cursor there.
- Copy the whole text of the selected message with `y`, including lines cut off by `message_line_limit`. The room's header says "Copied!" for three seconds. Copying uses OSC 52, so tmux needs `set -s set-clipboard on`.
- Mention people by typing their user ID (`@bob:example.com`) in any message, or pick a room member with `@`. Replies now mention the person you're replying to, and edits keep their mentions.
- Don't mark a room as read when it's opened while the terminal is out of focus, such as by clicking a notification.
- Start in the room switcher instead of opening the most recent room, and refresh the switcher when the terminal regains focus. Until a room is open, the switcher comes back whenever nothing else is showing, such as after verifying, instead of leaving a blank screen.
- Send a small JPEG thumbnail with every uploaded image and video, so other clients can show a preview.

## [1.0.2] - 2026-09-19

- Fix macOS release builds by including the URL parsing dependency on every platform.

## [1.0.1] - 2026-09-19

- Add optional captions to file uploads and accept pasted file paths and `file://` URIs.
- Allow pasted or directly typed emoji in the reaction dialog.
- Add a Cachix binary cache and reduce unnecessary inputs to Nix builds.
