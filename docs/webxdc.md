# WordCraft as a webxdc app

_Last updated: 2026-10-28 (the first release; touch screens: a finger scrolls the page, a long press selects)_

This fork (`experintellia/wordcraft`, branch `webxdc`) ships WordCraft as a [webxdc](https://webxdc.org/)
app: one `.xdc` file sent into a Delta Chat (or other messenger) chat, run inside the messenger,
sandboxed and offline.

## Building

`packaging/webxdc/package.sh` (needs [`trunk`](https://trunkrs.dev/) and the `wasm32-unknown-unknown`
target) builds `apps/wordcraft-web/webxdc.html` and writes `dist/release/wordcraft-<version>.xdc`:

- `index.html`: the page, which loads the messenger's `webxdc.js` (never packaged);
- the wasm, gzipped and stored uncompressed in the zip (`zip -n .gz`), loaded through
  `DecompressionStream`: messengers inflate zip entries slowly;
- `icon.png`, `manifest.toml` (`name = "WordCraft"`, `source_code_url` naming this fork) and the licences.

CI builds a preview `wordcraft.xdc` for every pull request (`webxdc-preview.yml`) and attaches
`wordcraft.xdc` to releases (`release-webxdc.yml`).

## Inside a messenger

When `window.webxdc` exists, `apps/wordcraft-web/src/web/webxdc.rs` makes the app one document
(`Services::single_document`):

- File › Replace with File… and Insert › Pictures pick files through `webxdc.importFiles`;
- Save As and Export (Word document, PDF…) send a copy to the chat through `webxdc.sendToChat`;
- File keeps Info, Save As, Export and Options; New, Open, Save, Print, Close, Recover, recent files,
  AutoSave and the Start screen are left out (`NOT_IN_CHAT` in `crates/ui-egui/src/lib.rs`), and so
  is the Discord link (the sandbox can't open it);
- fonts: only those bundled in the wasm (`assets/fonts`, never `CRAFT_FONTS_DIR`); adding fonts and
  loading system fonts are off, so every member lays the same text out the same way;
- the author of comments and tracked changes is the member's chat name (`selfName`).

## Shared editing

Everyone in the chat edits one document (`apps/wordcraft-web/src/collab`, wired up in
`src/web/webxdc.rs`):

- **Ops in a Yjs array.** Each change becomes an op appended to a `yrs` array; updates travel by
  `sendUpdate` (stored by the messenger, replayed on every start; merged up to `sendUpdateMaxSize`
  and sent at most every 3 s, or the messenger's `sendUpdateInterval` when longer, and when the page
  is hidden) and at once over the realtime channel.
  An update leaves the queue only once handed over; another member's edit that arrived only over
  the realtime channel and nobody stored within 20 s (its author left) is stored by whoever has it.
  Every member replays the same op order from a blank document, each op on a fresh replica session,
  rolling back to a cached state (the last 256 ops, then checkpoints) when an op lands earlier.
- **What an op carries.** The command and its parameters, the selection and the formatting waiting
  at the caret, the author (the chat name), the clock and the preferences that differ from the
  defaults (AutoCorrect options; a member's own AutoCorrect entries and exceptions stay on their
  device, and spelling commands aren't shared). A paste carries what it read from the
  clipboard, and Paste Formatting the formatting it applies, so both run again elsewhere and
  merge like typing. The author checks that replaying the op gives the same document; when it
  doesn't (another program's clipboard, the view…), the op carries the change instead: the
  changed paragraphs and tables and the changed fields, with a hash of what they replace. An
  opened file travels as its bytes; pictures as base64. Typing is held back for 0.7 s and shared
  as one op.
- **Concurrent edits merge.** Every character has an identity: the op that typed it and which of
  its characters it was (paragraph marks, tables and cells too; `collab/ids.rs`). An op's
  selection travels as the characters just before its ends, so on replay it lands just after
  them, whatever the author hadn't seen; two members typing at the same spot each keep their own
  words together, in log order. A range's start travels as its first character, so what others
  type just in front of a range isn't deleted or formatted with it. (Known limit: Enter pressed
  just in front of a range someone deletes meanwhile can lose its new empty paragraph, depending
  on the log order; every member still agrees.) Ids are worked out from each op's documents, the
  same way on every member: typing, Backspace and Delete at the op's selection exactly, anything
  else by aligning the changed text (Myers' diff). When the character is gone, the position is carried through the
  ops its author hadn't seen, mapped from the documents themselves. Parts and comments made concurrently get other ids in
  the final order, and ids an op names are moved past them.
- **Undo takes back only your own changes**: Undo finds what your step left in the document as it
  is now and puts back what was there before; refused (with a message) when someone changed one of
  the same paragraphs, tables or fields since.
- **Presence.** Each member's selection, name and colour travel over the realtime channel and are
  drawn on the others' pages (`crates/ui-egui/src/peers.rs`).
- **Comments and track changes** are stamped with the chat name; Review › Track Changes is a
  setting of the document, so it is on for everyone.
- **Hostile ops** are refused: commands that touch files or name a path, macros, unknown commands,
  positions deeper than 64 levels, patches whose hashes don't match or that are oversized. An edit
  too big for the messenger (`sendUpdateMaxSize`) is taken back with a message. An op that crashes
  the app while being replayed, or a Yjs update that crashes it while being applied, is
  remembered on that device and skipped from then on. Paragraphs a patch or a pasted clipboard
  brings get their runs repaired when they don't cover their text.
- **Op ids stay unique:** a device keeps its next op number between runs, so ops it shared only
  over the realtime channel before closing (and others stored) are never renumbered.

Tests: `cargo test -p wordcraft-web` (a convergence fuzz with three members and shuffled delivery,
`WORDCRAFT_COLLAB_SEEDS=100` to soak it; undo and redo per member; hostile ops; layout-dependent
results such as a table of contents replayed by a late joiner).

## Size

| Build | wasm raw | wasm gzipped |
|---|---|---|
| upstream web build (release profile, wgpu) | 40.3 MB | 15.7 MB |
| `wasm-release` profile (opt-level s, fat LTO) | 27 MB | 11 MB |
| + glow instead of wgpu, UI fonts embedded once, no TIFF/WebP decoders | 24 MB | 9.9 MB |

What is left (code, before wasm-opt): ui-egui 3.0 MB, doc 2.0 MB (mostly serde), engine 1.9 MB,
font parsing and shaping (read-fonts, skrifa, harfrust) 2.2 MB, DOCX 0.8 MB, PDF export 0.8 MB,
egui 0.9 MB. Zotero, metafile pictures and `.doc` import are under 0.2 MB each and stay. Data:
bundled fonts 4.2 MB, interface translations 2.5 MB, hyphenation 0.6 MB.

The webxdc build has no WebP or TIFF decoders (`more-images` feature of `wordcraft-web`): such
pictures can't be inserted, and ones inside an opened document don't show.

## Phone width

In the web app (never the desktop one), below 600 pt (`PHONE_WIDTH`) the chrome keeps what fits:
no rulers, the title bar's search shrinks to its icon, upstream's narrow ribbon shows Share/Editing/
Comments as icons, the status bar keeps the page, word count and zoom, dialogs and File's pages
stay on screen (and scroll), Options lists its panes as a menu, panes cover the page, and the web
shell starts with the page fitted to the width.

## Touch screens

iOS opens the on-screen keyboard only for a text input the user taps, while eframe focuses its
hidden input from script a frame later. On touch screens (`pointer: coarse`) the web shell
(`apps/wordcraft-web/src/web/touch.rs`, ported from GridCraft's) lays eframe's transparent input
over the visible pages and every text field (dialogs, the ribbon's boxes, the Navigation pane,
comments, File, Options: `crates/ui-egui/src/touch.rs`), with holes where a window or the mini
toolbar lies over a page, and over no page while a menu or popup is open (a tap beside it only
dismisses it). A tap there focuses it natively, the keyboard comes up, and the next frames place the
caret where the page was tapped or focus the field. A tap on a page that reaches the canvas instead
(the first after a mouse) focuses the input from the touch handler.

A finger on the page (`crates/ui-egui/src/touch_gestures.rs`, reading eframe's touches and those the
shell forwards from its input):

- **drag**: scrolls the document, both ways when the page is wider than the window, following the
  finger 1:1 and coasting on after a flick; a new touch stops the coast. A drag never moves the
  caret or brings the keyboard up, and while the keyboard is up it stays up;
- **tap**: places the caret (and brings the keyboard up); a double tap selects the word;
- **long press**: selects the word under the finger, and dragging on without lifting extends the
  selection word by word (scrolling on near the page's edges); lifted without moving, the context
  menu opens on the word;
- **two fingers**: pinch to zoom;
- **in Read Mode**, a swipe sideways turns the page.

Only egui decides when a touch held still is a long press (the shell asks it before taking a lift
for a tap). A finger whose end got lost (lifted over the keyboard, outside the view) is dropped when
the next finger lands alone on the screen.

Scroll areas elsewhere (panes, dialogs, File, the ribbon) get the finger as eframe reports it and
drag-scroll by themselves (egui's own touch scrolling), and so does the page's scroll bar (a strip
along the view's right and bottom edges). Known limit: a finger drag on a picture's handles or a
table border scrolls the page; a mouse or pen still drags them. A pen (Safari's `touchType`
"stylus") selects like a mouse. A mouse, trackpad or pen makes the input step aside
(`pointer-events: none`) until a finger touches the screen again, so wheel, hover and strokes reach
the page, and they select as on a desktop. While the keyboard is up the input follows the caret and
taps reach the page as usual. Until a tap, a key typed or a mouse click on the page, the page asks for
no keyboard; from a hardware key or click on it asks for the platform text input, so dead keys and
input methods compose. Double-tap zoom is off (`touch-action: manipulation`), and iOS's pan to keep
the input above the keyboard is undone once it hides. Desktops are unchanged.
