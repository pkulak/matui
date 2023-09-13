# Matui

A very opinionated [Matrix](https://matrix.org/) TUI. Text entry is done
externally, as is opening of all attachements. The UI is kept as simple as
possible and there's no mouse support (yet, at least).

![Matui](https://github.com/pkulak/matui/blob/main/screenshot.png?raw=true "The main chat window.")

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

| Key   | Description                                            |
|-------|--------------------------------------------------------|
| Space | Show the room switcher.                                |
| j*    | Select one line down.                                  |
| k*    | Select one line up.                                    |
| i     | Create a new message using the external editor.        |
| Enter | Open the selected message (images, videos, urls, etc). |
| s     | Save the selected message (images and videos).         |
| c     | Edit the selected message in the external editor.      |
| r     | React to the selected message.                         |
| R     | Reply to the selected message.                         |
| v     | View the selected message in the external editor.      |
| V     | View the current room in the external editor.          |
| u     | Upload a file.                                         |

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

## File Viewing

You will probably want to view attachements and should make sure xdg-open works
with all the files you care about. I recommend [mpv](https://mpv.io/) and
[imv](https://sr.ht/~exec64/imv/) at a minimum.

## File Uploading

KDialog and/or Zenity is required to show the file picker.

# Configuration Example

```
# All the reactions that will show up in the picker.
reactions = [ "❤️", "👍", "👎", "😂", "‼️", "❓️"]

# Muted rooms.
muted = ["!hMPITSQBLFEleSJeVe:matrix.org"]

# Useful if your custom config is interfering with Enter key bindings
clear_vim = true
```

The config file is hot reloaded and can generally be found at
~/.config/matui/config.toml.

# Windows/Mac Support

There's nothing explicitly preventing this, but it's untested and Linux is
currently assumed.

# See Also

There's a much more mature Matrix TUI here:

[gomuks](https://github.com/tulir/gomuks)

