# Matui

A very opinionated [Matrix](https://matrix.org/) TUI. Text entry is done
externally, as is opening of all attachements. The UI is kept as simple as
possible and there's no mouse support (yet, at least).

![Matui](https://github.com/pkulak/matui/blob/main/screenshot.png?raw=true "The main chat window.")

# Upgrade Notes

The underlying Matrix SDK that Matui uses has recently switched from Sled to
Sqlite. That's great, because it seems to have fixed the horrible data store
bloat issues, but it also means the cache has to be re-built after you upgrade
and there will be some garbage left over. If you'd like to reclaim some space,
you can blow away the `matrx-sdk-state` folder in your cache
(`.local/share/matui/{hash}/matrix-sdk-state`).

# Who should use this client?

Anyone who wants a very simple terminal Matrix client, but runs another client
somewhere else for the missing features. There are some very basic actions
that aren't supported at the moment, like joining rooms and moderation. Also,
many events are still not suported, like threads (which are still shown, but
not formatted very well). Also, this project is very early, so you need to
be tolerant of some bugs.

# Installation

## Releases

You can download the latest release, unpack it, and move `matui` to `/usr/bin`
(or anywhere else you like).

## Arch Linux

Matui is packaged for the AUR: `paru -S matui`.

## Nix

There is a `flake.nix` that can be used run temporarily locally, or to install on NixOS or a `home-manager` system.

### Shell

`nix run 'https://github.com/pkulak/matui.git'`

### OS

```nix
{
  inputs = {
    matui.url = "github:pkulak/matui";
  };
  outputs =
    inputs@{ self
    , nixpkgs-unstable
    , ...
    }:
    let
      overlays = {
        unstable = _: prev: {
          unstable = import nixpkgs-unstable
            {
              inherit (prev.stdenv) system;
            } // {
            matui = inputs.matui.packages.${prev.stdenv.system}.matui;
          };
        };
      };
    in
    {
      <snip>;
      packages = with pkgs; [
        unstable.matui
      ];
    }
}
```

# Keybindings

Modal UIs can be a bit overwhelming, but thankfully chat isn't terribly
complicated. Especially if you don't implement too many features.

| Key    | Description                                            |
|--------|--------------------------------------------------------|
| Space  | Show the room switcher.                                |
| j*     | Select one line down.                                  |
| k*     | Select one line up.                                    |
| Ctrl+d | Select half a page down.                               |
| Ctrl+u | Select half a page up.                                 |
| G      | Select latest message.                                 |
| i      | Create a new message using the external editor.        |
| Enter  | Open the selected message (images, videos, urls, etc). |
| s      | Save the selected message (images and videos).         |
| c      | Edit the selected message in the external editor.      |
| r      | React to the selected message.                         |
| R      | Reply to the selected message.                         |
| Ctrl+Alt+r | Verify this client with your passphrase.           |
| v      | View the selected message in the external editor.      |
| V      | View the current room in the external editor.          |
| u      | Upload a file.                                         |
| m      | Mute or unmute the current room (until restart).       |
| /      | Search the current room.                               |
| ?      | Show this helper.                                      |

\* arrow keys are fine too

# External Applications

The only requirement is an editor, and the $EDITOR environmental variable should
be set. Vim is highly recommended, as Matui is optimized for it. When using Vim,
the editor is started in insert mode for new messages and Enter sends them.

## Text Entry

Having Enter send a message is nice for messaging, but it then begs to have
Shift+Enter insert a new line. Unfortunately, that's not possible without
modifying your terminal config to send the key bindings that Neovim expects.
In Alacritty, it would be this:

```
key_bindings:
  - { key: Return, mods: Shift, chars: "\x1b[13;2u" }
```

Once that is setup, Matui will open Neovim (if that's your default editor)
with keys mapped such that Shift+Enter inserts a new line.

Another nice option is to map `jx` to `<Esc>:x<CR>` in insert mode.

## File Viewing

You will probably want to view attachements and should make sure xdg-open works
with all the files you care about. I recommend [mpv](https://mpv.io/) and
[imv](https://sr.ht/~exec64/imv/) at a minimum.

## File Uploading

KDialog and/or Zenity is required to show the file picker. FFMpeg is also
required to create thumbnails if you upload videos.

## Search

Search (triggered by typing / in a room) is client side, and brute force. This
uses more resources, but lets us go back as far as we like, and works in every
room, encrypted or not. There are some guardrails, detailed below, but in
general, it works great for finding recent messages, and you can go back
farther if really needed.

Searches are live as you type, and hitting Enter will let you scroll the list
of results. Hitting Enter again on a result will jump to that message in the
full timeline. Use Shift+G to go back to the latest message.

## End-to-End Encryption

After login, a verification request is sent out to your other clients. If you
don't have any other clients, or don't want to verify this way, you can
cancel/ignore the request and hit Ctrl+Alt+r from the chat window to start
a verification by passphrase.

# Configuration Example

```
# All the reactions that will show up in the picker.
reactions = [ "❤️", "👍", "👎", "😂", "‼️", "❓️"]

# Muted rooms.
muted = ["!hMPITSQBLFEleSJeVe:matrix.org"]

# Useful if your custom config is interfering with Enter key bindings
clear_vim = true

# If non-zero, send a "blur" event after that many seconds of inactivity,
# useful when blur events aren't sent reliably by your terminal.
blur_delay = 30

# What's the limit on how far back to go, in events? This is mostly to
# put a limit on how far back a search will look. If you know your
# homeserver is okay with it (and you have unlimited memory in your machine),
# you can set this to -1. Default shown below.
max_events = 8192
```

The config file is hot reloaded and can generally be found at
~/.config/matui/config.toml.

# Windows/Mac Support

There's nothing explicitly preventing this, but it's untested and Linux is
currently assumed.

# See Also

There's another really fun Matrix TUI here, with a lot of the same goals:

[iamb](https://github.com/ulyssa/iamb)
