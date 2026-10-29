# Changelog

## [Unreleased]

- Mention people by typing their user ID (`@bob:example.com`) in any message, or pick a room member with `@`. Replies now mention the person you're replying to, and edits keep their mentions.

## [1.1.0] - 2026-10-25

- Sync with Simplified Sliding Sync (MSC4186) instead of `/sync`. Matui now needs a homeserver that supports it.

## [1.0.2] - 2026-09-19

- Fix macOS release builds by including the URL parsing dependency on every platform.

## [1.0.1] - 2026-09-19

- Add optional captions to file uploads and accept pasted file paths and `file://` URIs.
- Allow pasted or directly typed emoji in the reaction dialog.
- Add a Cachix binary cache and reduce unnecessary inputs to Nix builds.
