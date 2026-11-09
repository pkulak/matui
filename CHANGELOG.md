# Changelog

## [Unreleased]

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
