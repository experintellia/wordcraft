# WordCraft as a webxdc app

_Last updated: 2026-10-16_

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

Below 600 pt (`PHONE_WIDTH`) the chrome keeps what fits: no rulers, the title bar's search shrinks to
its icon, the tab strip drops Share/Editing/Comments so the tabs fit, the status bar keeps the
page, word count and zoom, dialogs and File's pages stay on screen (and scroll), panes cover the
page, and the web shell starts with the page fitted to the width.
