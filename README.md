# Matui

A very opinionated [Matrix](https://matrix.org/) TUI. Text entry is done
externally, as is opening of all attachements. The UI is kept as simple as
possible and there's no mouse support (yet, at least).

![Matui](https://github.com/pkulak/matui/blob/main/screenshot.png?raw=true "The main chat window.")

# Keybindings

Modal UIs can be a bit overwhelming, but thankfully chat isn't terribly
complicated. Especially if you don't implement too many features.

## Chat Window

| Key   | Description                                            |
|-------|--------------------------------------------------------|
| Space | Show the room switcher.                                |
| j*    | Select one message down.                               | 
| k*    | Select one message up.                                 | 
| i     | Create a new message using the external editor.        | 
| Enter | Open the selected message (images, videos, urls, etc). | 
| c     | Edit the selected message in the external editor.      | 
| r     | React to the selected message.                         | 
| v     | View the selected message in the external editor.      | 
| u     | Upload a file.                                         | 

\* arrow keys are fine too

## Rooms Window

| Key | Description                                     |
|-----|-------------------------------------------------|
| i   | Search for a room.                              | 
| esc | Leave search mode.                              | 

# External Applications

The only requirement is an editor, and the $EDITOR environmental variable should
be set. Vim is highly recommended, as Matui is optimized for it. When using Vim,
the editor is started in insert mode for new messages and Enter sends them.

## File Viewing

You will probably want to view attachements and should make sure xdg-open works
with all the files you care about. I recommend [mpv](https://mpv.io/) and
[imv](https://sr.ht/~exec64/imv/) at a minimum.

## File Uploading

KDialog and/or Zenity is required to show the file picker.

# Windows/Mac Support

There's nothing explicitly preventing this, but it's untested and Linux is
currently assumed.

# See Also

There's a much more mature Matrix TUI here:

[https://github.com/tulir/gomuks](gomuks)
