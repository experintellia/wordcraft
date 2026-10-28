# Changelog: WordCraft for webxdc

Releases of this fork (`experintellia/wordcraft`), which ships WordCraft as a [webxdc](https://webxdc.org/)
app: send `wordcraft.xdc` into a Delta Chat (or other messenger) chat, and everyone who opens it
edits the same document. Tags are `v<upstream version>-webxdc.<n>`; each release's notes are its
section here (`.github/workflows/release-webxdc.yml`). Upstream WordCraft's own progress is in
[ROADMAP.md](ROADMAP.md).

## v0.9.0-webxdc.1 — 2026-10-28

The first release. It is based on upstream WordCraft 0.9.0 and later (`main` at `2451b5a`, "The
I-beam pointer shows on white paper on Windows").

### One document per chat
- `wordcraft.xdc` runs inside the messenger, sandboxed and offline. It is about 10 MB, with the wasm
  gzipped (DecompressionStream: Chrome 80, Safari 16.4 or Firefox 113 and later).
- In a chat the app holds one document:
  - **Bringing a file in:** File › Replace with File… and Insert › Pictures pick files through
    the messenger.
  - **Sending a copy out:** Save As and Export (Word, PDF, …) send it to the chat. Export's Print
    Markup box puts comments and tracked changes in it.
  - **Left out, because a chat can't do them:** New, Open, Save, Print, Close, Recover, recent
    files, AutoSave and the Start screen.
- The author of comments and tracked changes is your chat name.
- Every member uses the fonts bundled in the app, so the same text lays out the same way for
  everyone.

### Shared editing
- **Everyone edits at once.** Each change is shared with the chat: right away to members who have
  the app open, and through the chat to everyone else.
  - Every member replays the same changes in the same order, so all copies end up identical.
  - Members who open the app later catch up from the chat's stored edits.
- **Concurrent typing merges.**
  - Every character has an identity, so each member's words stay where they typed them, even at
    the same spot.
  - What others type just in front of a selected range isn't deleted or formatted with it.
  - Pastes and Paste Formatting merge like typing. The clipboard travels only when the paste
    read it.
- **Undo takes back only your own changes.** It is refused, with a message, when someone has
  changed the same paragraph since.
- **Presence:** you see the other members' carets and selections, with their names, on the page.
- **Comments and Track Changes** work across the chat. Track Changes is a setting of the document,
  so it is on for everyone.
- **Quick Parts and AutoText** travel with the document.
- **Malformed or hostile edits from other members are refused.** An edit, or a shared update,
  that crashed the app is skipped on that device from then on.

### Touch screens
- **A tap opens the on-screen keyboard** on iOS and Android, on the page (where it places the caret)
  and in text fields: dialogs, the font box, Find, comments and Options. Formatting from the
  toolbar or the ribbon while typing keeps the keyboard up.
- **A finger drag scrolls the page,** up, down and sideways, and coasts on after a flick; a new
  touch stops it. Scrolling never moves the caret.
- **A double tap selects a word; a long press selects the word under the finger,** and dragging on
  extends the selection. Lifted without moving, a long press opens the context menu.
- **Pinching zooms; in Read Mode a sideways swipe turns the page.** Panes, dialogs and File's pages
  scroll with a finger too.
- **A mouse, trackpad, pen or hardware keyboard** on a tablet works as on a desktop.

### Phone width
- Below 600 pt the app keeps what fits:
  - no rulers;
  - a compact title bar and status bar;
  - ribbon tabs that fit, with Share, Editing and Comments as icons;
  - dialogs and File's pages that stay on screen and scroll, with Options' panes as a menu and
    Export's buttons fitting the width;
  - panes over the page.
- The page starts fitted to the screen's width.

### Known limits
- **Long chats replay every edit when the app opens,** so a long-edited document opens slowly
  ([#10](https://github.com/experintellia/wordcraft/issues/10)).
- **Enter just in front of a range someone deletes at the same time** can lose its new empty
  paragraph. Every member still ends up with the same document.
- **WebP and TIFF pictures** can't be inserted, and ones inside an opened document don't show.
- **Undoing an automatic capital** doesn't add an AutoCorrect exception in a chat.
- **Number boxes** (spacing, indents and margins in Paragraph, Page Setup and Options) may not open
  the on-screen keyboard on iOS; their arrows still work.
- **Add Font…** and **Load System Installed Fonts…** still show in the font box, and only report
  that they aren't available.
- **Comments and parts created at the same time:** when one member deletes a comment (or a header,
  note or text box) while another creates one, the new one can take the deleted one's number, and
  a later edit of it by its author can miss. Every member still ends up with the same document.
- **The log isn't append-only:** a modified app could remove other members' edits from the
  chat's log.
- **A finger drag on a picture's handles or a table border** scrolls the page; a mouse or pen still
  drags them.
- **A second pane on a phone** (say, Comments while Navigation is open) gets almost no width
  until the first one is closed.
