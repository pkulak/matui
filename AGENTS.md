# Matui

Run cargo, tmux and the scripts below through `nix develop -c`; wrap a
sequence of them in one `nix develop -c bash -c '...'`. Before handing
over a change, run:

```sh
nix develop -c bash -c 'cargo test && cargo clippy --all-targets -- --deny warnings && cargo fmt --check'
```

These are the same checks as `nix flake check`, but they reuse `target/`. The
flake check builds in release mode from scratch, which takes over 15 minutes.

## Reproducing a bug against Continuwuity or Tuwunel

Matui is used daily against Synapse, so server-specific bugs usually come
from these two. Both run locally, with nothing shared, so set them up however
the bug needs. Everything runs in tmux on the socket `matui`, which outlives
each `nix develop -c` call. Clean up with `tmux -L matui kill-server` and by
deleting the state directories.

1. Start a server in tmux, with its state in a directory you pick:

   ```sh
   tmux -L matui new -d -s server scripts/test-server continuwuity 6167 /tmp/repro-server
   ```

   Read its log with `tmux -L matui capture-pane -t server -p -S -`. `@admin:localhost`,
   password `admin`, is the server admin and is in the admin room. It's ready
   when that user can log in; the server answers other requests a moment
   earlier.

2. Put the server into the state the bug needs, with the Client-Server API:

   ```sh
   hs=http://localhost:6167
   login='{"type":"m.login.password","identifier":{"type":"m.id.user","user":"admin"},"password":"admin"}'
   until curl -sf $hs/_matrix/client/v3/login -d "$login" >/dev/null; do sleep 0.5; done
   tok=$(curl -s $hs/_matrix/client/v3/login -d "$login" | jq -r .access_token)
   auth="Authorization: Bearer $tok"

   # anyone else registers with the token "matui"
   curl -s $hs/_matrix/client/v3/register -d '{"username":"bob","password":"bob","auth":{"type":"m.login.registration_token","token":"matui"}}'
   room=$(curl -s -H "$auth" $hs/_matrix/client/v3/createRoom -d '{"name":"Test","invite":["@bob:localhost"]}' | jq -r .room_id)
   curl -s -X PUT -H "$auth" "$hs/_matrix/client/v3/rooms/$room/send/m.room.message/$(date +%s%N)" -d '{"msgtype":"m.text","body":"hello"}'
   ```

   Send any event type and content the same way, such as JSON copied from the
   bug report. For server-side state, send `!admin <command>` as a message to
   `#admins:localhost` (look up its ID at
   `$hs/_matrix/client/v3/directory/room/%23admins:localhost`). The server
   answers with an `m.notice` from `@conduit:localhost`; `!admin --help`
   lists the commands.

3. Start matui, and sign in by typing the homeserver URL:

   ```sh
   scripts/test-matui m /tmp/repro-matui
   tmux -L matui send-keys -t m -l http://localhost:6167
   tmux -L matui send-keys -t m Enter Enter
   until grep -q 'sign-in: Password' /tmp/repro-matui/test.log; do sleep 0.2; done
   tmux -L matui send-keys -t m -l admin
   tmux -L matui send-keys -t m Enter
   tmux -L matui send-keys -t m -l admin
   tmux -L matui send-keys -t m Enter Enter
   ```

   Send text with `-l`, so tmux doesn't read words such as `Enter` as keys.
   Keys sent before a screen or popup appears go to the one before it, so
   wait for it before typing into it. Each sign-in step logs
   `sign-in: <step>` to `test.log`, ending with `sign-in: done`; for
   anything else, capture the screen. The keybindings are in `README.md`;
   `C-c` quits. Run `scripts/test-matui` again with the same
   directory to come back signed in.

4. Read the screen with `tmux -L matui capture-pane -t m -p`, adding `-e`
   for colours. Images don't show up in a capture. Matui's log is
   `test.log` in its directory.

5. Reproducing is done when the screen shows what the report describes, with
   the events behind it fetched from
   `$hs/_matrix/client/v3/rooms/$room/messages?dir=b&limit=5`; or when you've
   followed every step in the report and matui behaves correctly.

6. Turn a reproduced bug into the smallest test that goes red before the fix,
   at the layer where the bug lives: a minimal event through `Message` or
   `make_message_list` for how events display, and `MatrixMockServer` with
   the exact events you fetched when the bug is in the exchange with the
   server or depends on the events' exact shape.

`scripts/test-server` turns off Continuwuity's OAuth, which it offers by
default and matui would use, so this setup doesn't exercise OAuth login.
