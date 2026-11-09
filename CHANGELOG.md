# Changelog

## [Unreleased]

- Mention people by typing their user ID (`@bob:example.com`) in any message, or pick a room member with `@`. Replies now mention the person you're replying to, and edits keep their mentions.
- Don't mark a room as read when it's opened while the terminal is out of focus, such as by clicking a notification.
- Start in the room switcher instead of opening the most recent room, and refresh the switcher when the terminal regains focus. Until a room is open, the switcher comes back whenever nothing else is showing, such as after verifying, instead of leaving a blank screen.
- Send a small JPEG thumbnail with every uploaded image and video, so other clients can show a preview.
- Show encrypted messages in the open room as soon as their keys arrive, such as after verifying or recovering, instead of only after switching rooms or restarting.
- Show "Unable to decrypt this message." in place of an encrypted message whose key hasn't arrived, instead of leaving it out, so a room opened while keys download from the backup no longer looks empty. The message replaces it when its key arrives.
- Show a message, edit, deletion, or upload as soon as it's sent, marked "(sending)" until the server has it, instead of waiting behind a progress popup. A message that fails to send stays in the room with its error, and matui retries it when the problem may pass, such as a lost connection. One that can never send holds back later messages until it's deleted with `dd`. Anything else that can never send, such as an edit, deletion, or reaction, is cancelled with a popup saying why, including when it got stuck while matui wasn't running.
- Load a thread's whole history when it's opened, instead of only the messages the room had already loaded. A thread's root now stays in place in the room when the thread gets a reply, as in Element, and its reply count comes from the server. Files uploaded from a thread go to the thread.
- Show read receipts on the message each person read to, including receipts from before matui started, instead of only those that arrived since. Someone's newest message counts as read by them.
- Ask before accepting a verification request, naming the device asking, instead of accepting every request in every running session. Another running matui session no longer takes over a new session's verification.
- Start the emoji comparison once another device accepts matui's verification request, so one matui session can verify another. When the other device starts a comparison too, as Element X does, matui follows whichever one wins instead of hanging.
- Show a popup with the reason when a verification is cancelled, by either side or by two requests colliding, instead of leaving it waiting. A request answered by another session just closes its prompt.
- Show progress while encryption keys download from the backup after verifying, instead of leaving rooms opened meanwhile empty until the download finishes.
- Ask how to verify a new session after login, instead of sending a verification request right away. Ask another verified session, which needs to be open to answer, or use the recovery key. Without another verified session, only the recovery key is offered, since the request went to every device and blocked any verification started elsewhere for 10 minutes.

## [1.1.0] - 2026-10-25

- Sync with Simplified Sliding Sync (MSC4186) instead of `/sync`. Matui now needs a homeserver that supports it.

## [1.0.2] - 2026-09-19

- Fix macOS release builds by including the URL parsing dependency on every platform.

## [1.0.1] - 2026-09-19

- Add optional captions to file uploads and accept pasted file paths and `file://` URIs.
- Allow pasted or directly typed emoji in the reaction dialog.
- Add a Cachix binary cache and reduce unnecessary inputs to Nix builds.
