# Changelog

## [Unreleased]

- Show reactions as just the emoji, without its name, and put emojis from the same people on one line, such as "👀 ✅ Barnaby".
- Show the time each message was sent, such as 2:41 PM, instead of how long ago, now that the day lines give the date. A message that sits under a later day, because someone replied to it then, shows its own date too.
- Draw a line above each day's messages that says which day it is: Today, Yesterday, the weekday for the rest of the week, then the full date. A message someone replies to sits under the day of its latest reply. Once a day's line scrolls off, it stays pinned at the top of the chat until the next day's line takes its place.
- Show polls with their answers and vote counts, which update as votes come in. Polls that hide their results show the counts once they end. Voting still needs another client.
- Show shared locations as a pin with the place's name and an OpenStreetMap link, which Enter opens. They used to be left out.
- Show stickers in the chat, as pictures. They used to be left out.
- Show pictures that come with no thumbnail and no file size when they're no bigger than 800×600, as stickers often are.
- Update the room switcher as new messages arrive, including the ones sync catches up on at startup. The cursor stays on the room it's on, unless it's at the top.
- Show `/me` emotes, in italics. They used to be left out of the chat.
- Draw a "new messages" line above the messages that arrived since you last read the room. It stays put until you send a message or leave the room.
- Count mentions in encrypted rooms in the room switcher, where the server can't see them. A mention you missed while away still counts if it's among the room's last 20 messages.
- Notify by your account's notification settings, as other clients do. `:mute` and `:unmute` replace `m`, and in a room they change its setting on the server, so it holds in every client and across restarts. A room muted with `:mute` still notifies for mentions and keywords, in its threads too. The `muted` and `mentions_override_mute` settings are gone and no longer do anything: mute those rooms with `:mute`, and in place of `muted = ["*"]`, set your account to notify only for mentions in another client, such as Element.
- Drag across a message to select and copy part of its text. The copy is the message's own text, markdown included, without the line breaks Matui wraps at or the markers and gutters around it, and the selection stays inside the message. Double-click to select a word, or triple-click for a line, and drag from there to select a word or line at a time. The room's header says "Copied!", as it does for `y`. Shift+drag still selects anything else in most terminals. Links are underlined, and clicking one opens just that link.
- Click in the chat to move the cursor there, and click a picture to open it.
- Show a play button over video thumbnails in the chat.
- Click in a text field to move its cursor there.
- Copy the whole text of the selected message with `y`, including lines cut off by `message_line_limit`. The room's header says "Copied!" for three seconds. Copying uses OSC 52, so tmux needs `set -s set-clipboard on`.
- Only notify for a reply in a thread if you wrote the message that started the thread, replied in it, or are mentioned, and in a muted room only if you're mentioned. Replies notify while the room is open, since its timeline doesn't show them, but not while the thread is. Clicking one opens the thread, however old. `:mute` in a thread mutes it until restart.
- Log in by scanning a QR code with a device that's already logged in, such as Element X, when the homeserver supports it. The new session comes verified, with your keys. Esc falls back to logging in with the browser.
- Fetch each encryption key from the backup when a message first fails to decrypt, as Element X does, instead of downloading every key at once after verifying. Old messages fill in as you open rooms and scroll, and a key that wasn't in the backup yet is tried again later.
- Mention people by typing their user ID (`@bob:example.com`) in any message, or pick a room member with `@`. Replies now mention the person you're replying to, and edits keep their mentions.
- Say so in the room's header when loading earlier messages fails, and try again when you scroll up, instead of showing "Loading..." for good until you switch rooms.
- Don't mark a room as read when it's opened while the terminal is out of focus, such as by clicking a notification.
- Don't notify for messages in the open room until the terminal reports losing focus. In a terminal that doesn't report focus, the open room never notifies.
- Start in the room switcher instead of opening the most recent room. Until a room is open, the switcher comes back whenever nothing else is showing, such as after verifying, instead of leaving a blank screen.
- Send a small JPEG thumbnail with every uploaded image and video, so other clients can show a preview.
- Show encrypted messages in the open room as soon as their keys arrive, such as after verifying or recovering, instead of only after switching rooms or restarting.
- Show "Unable to decrypt this message." in place of an encrypted message whose key hasn't arrived, instead of leaving it out, so a room opened while keys download from the backup no longer looks empty. The message replaces it when its key arrives.
- Show a message, edit, deletion, or upload as soon as it's sent, marked "(sending)" until the server has it, instead of waiting behind a progress popup. A message that fails to send stays in the room with its error, and matui retries it when the problem may pass, such as a lost connection. One that can never send holds back later messages until it's deleted with `dd`. Anything else that can never send, such as an edit, deletion, or reaction, is cancelled with a popup saying why, including when it got stuck while matui wasn't running.
- Load a thread's whole history when it's opened, instead of only the messages the room had already loaded. A thread's root now stays in place in the room when the thread gets a reply, as in Element, and its reply count comes from the server. Files uploaded from a thread go to the thread.
- Show read receipts on the message each person read to, including receipts from before matui started, instead of only those that arrived since. Someone's newest message counts as read by them.
- Ask before accepting a verification request, naming the device asking, instead of accepting every request in every running session. Another running matui session no longer takes over a new session's verification.
- Start the emoji comparison once another device accepts matui's verification request, so one matui session can verify another. When the other device starts a comparison too, as Element X does, matui follows whichever one wins instead of hanging.
- Show a popup with the reason when a verification is cancelled, by either side or by two requests colliding, instead of leaving it waiting. A request answered by another session just closes its prompt.
- Ask how to verify a new session after login, instead of sending a verification request right away. Ask another verified session, which needs to be open to answer, or use the recovery key. Without another verified session, only the recovery key is offered, since the request went to every device and blocked any verification started elsewhere for 10 minutes.

## [1.1.0] - 2026-10-25

- Sync with Simplified Sliding Sync (MSC4186) instead of `/sync`. Matui now needs a homeserver that supports it.

## [1.0.2] - 2026-09-19

- Fix macOS release builds by including the URL parsing dependency on every platform.

## [1.0.1] - 2026-09-19

- Add optional captions to file uploads and accept pasted file paths and `file://` URIs.
- Allow pasted or directly typed emoji in the reaction dialog.
- Add a Cachix binary cache and reduce unnecessary inputs to Nix builds.
