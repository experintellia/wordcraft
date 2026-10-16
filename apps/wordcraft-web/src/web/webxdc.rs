//! Running as a webxdc app (Delta Chat and other messengers): the page loads the messenger's
//! `webxdc.js`, which sets `window.webxdc`. The app is then one document
//! ([`Services::single_document`]): files come from the chat or the device through
//! `webxdc.importFiles` (File › Replace with File…, Insert › Pictures), and Save As and Export
//! hand a copy to the chat through `webxdc.sendToChat`. There is no printing (the messenger's
//! sandbox has no print dialog) and no added fonts: every member lays the text out with the
//! fonts bundled in the app.
//!
//! Everyone in the chat edits that one document ([`crate::collab`]). Edits travel as `sendUpdate`
//! payloads `{y: <base64 Yjs update>}`, which the messenger stores and replays on every start
//! (sent at most every few seconds, merged), and at once over the realtime channel where the
//! messenger has one. The realtime channel also carries each member's selection (presence), drawn
//! on the others' pages; it isn't stored. Until the stored edits are in, nothing is editable.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Function, Object, Reflect, Uint8Array};
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use wordcraft_engine::cmd::insert::{base64_decode, base64_encode};
use wordcraft_engine::{Selection, Session};
use wordcraft_ui_egui::async_saves::SaveOutcome;
use wordcraft_ui_egui::peers::Peer;
use wordcraft_ui_egui::{Inbox, Services, WordApp};

use crate::collab::Collab;

/// The realtime channel's limit per message; larger updates go by `sendUpdate` alone.
const REALTIME_MAX: usize = 128_000;
/// Realtime messages start with one of these.
const TAG_UPDATE: u8 = 0;
const TAG_PRESENCE: u8 = 1;
/// Stored updates are merged and sent at most this often (ms): each one is a chat message.
const STORE_EVERY_MS: f64 = 3_000.0;
/// A moving selection is sent at most this often; a still one is repeated every `HEARTBEAT_MS`,
/// and a member not heard from for `GONE_MS` disappears.
const PRESENCE_EVERY_MS: f64 = 150.0;
const HEARTBEAT_MS: f64 = 5_000.0;
const GONE_MS: f64 = 15_000.0;
/// Caps on what one member's presence can make every frame draw.
const MAX_PEERS: usize = 50;
const MAX_NAME: usize = 40;
const MAX_PATH: usize = 64;
/// `sendUpdateMaxSize` when the messenger doesn't say.
const DEFAULT_MAX_UPDATE: usize = 128_000;
/// Where this member and device's client id is kept between runs.
const CLIENT_KEY: &str = "wordcraft-client";

/// `window.webxdc`, when the page runs as a webxdc app (`webxdc.js` loaded).
pub fn api() -> Option<JsValue> {
    let api = Reflect::get(&web_sys::window()?.into(), &"webxdc".into()).ok()?;
    api.is_object().then_some(api)
}

/// Fits `services` to the messenger and names the user after their chat name.
pub fn host(api: &JsValue, session: &mut Session, services: &mut Services, inbox: Inbox, ctx: egui::Context) {
    if let Some(name) = Reflect::get(api, &"selfName".into()).ok().and_then(|n| n.as_string()).filter(|n| !n.trim().is_empty()) {
        session.author = name;
    }
    let repaint = ctx.clone();
    let import = api.clone();
    services.open_async = Some(Box::new(move |purpose: &str| {
        let exts = match purpose {
            // No WebP or TIFF decoders in this build (`more-images`).
            "picture" => &["png", "jpg", "jpeg", "gif", "bmp", "wmf", "emf"],
            "recipients" => super::RECIPIENT_EXTS,
            _ => super::DOC_EXTS,
        };
        let extensions: Array = exts.iter().map(|e| JsValue::from(format!(".{e}"))).collect();
        let filter = object(&[("extensions", extensions.into()), ("multiple", false.into())]);
        let (inbox, ctx) = (inbox.clone(), ctx.clone());
        let Ok(promise) = call(&import, "importFiles", &[filter]).and_then(|p| p.dyn_into::<js_sys::Promise>()) else {
            log::error!("webxdc.importFiles is unavailable");
            return;
        };
        wasm_bindgen_futures::spawn_local(async move {
            let files = match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(files) => Array::from(&files),
                Err(e) => return log::error!("couldn't import a file: {e:?}"),
            };
            for file in files.iter().filter_map(|f| f.dyn_into::<web_sys::File>().ok()) {
                match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                    Ok(buf) => inbox.lock().unwrap_or_else(|e| e.into_inner()).push((file.name(), Uint8Array::new(&buf).to_vec())),
                    Err(e) => log::error!("couldn't read {}: {e:?}", file.name()),
                }
            }
            ctx.request_repaint();
        });
    }));
    let to_chat = api.clone();
    // The copy counts as saved only once the messenger has taken it (#1315's tickets), so a send
    // that fails or is cancelled leaves the document unsaved and Replace with File still asks.
    let saves = services.async_saves.clone();
    services.download = Some(Box::new(move |path: &str, bytes: &[u8]| {
        let name = std::path::Path::new(path).file_name().map_or_else(|| "document.docx".into(), |n| n.to_string_lossy().to_string());
        let file = object(&[("name", name.as_str().into()), ("base64", base64_encode(bytes).into())]);
        let promise = call(&to_chat, "sendToChat", &[object(&[("file", file)])]).map_err(|e| format!("{e:?}"))?;
        let Ok(promise) = promise.dyn_into::<js_sys::Promise>() else { return Ok(()) };
        let ticket = saves.as_ref().map(|s| (s.clone(), s.start()));
        let repaint = repaint.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let outcome = match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(_) => SaveOutcome::Saved,
                Err(e) => {
                    log::warn!("couldn't send {name} to the chat: {e:?}");
                    SaveOutcome::Failed(format!("{e:?}"))
                }
            };
            if let Some((saves, ticket)) = ticket {
                saves.finish(ticket, outcome);
            }
            repaint.request_repaint();
        });
        Ok(())
    }));
    services.print = None;
    services.store_font = None;
    services.forget_font = None;
    services.load_system_fonts = None;
    services.single_document = true;
    // The chat keeps every edit: nothing is lost by leaving.
    services.on_dirty = None;
}

/// Where a member is: their selection in the shared document.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Presence {
    /// `selfAddr`: who it is.
    id: String,
    name: String,
    sel: Selection,
}

/// The shared document: edits to and from the chat, and everyone's presence.
pub struct Shared {
    api: JsValue,
    channel: Option<JsValue>,
    collab: Collab,
    ctx: egui::Context,
    me: String,
    /// Received since the last frame.
    received: Rc<RefCell<Vec<Vec<u8>>>>,
    /// The messenger has handed over the updates it stored.
    replayed: Rc<Cell<bool>>,
    loading: bool,
    /// Updates not yet stored, and when the last went.
    unsent: Rc<RefCell<Vec<Vec<u8>>>>,
    stored_at: f64,
    heard: Rc<RefCell<Vec<Presence>>>,
    /// The other members online, by `selfAddr`, with when they were last heard.
    peers: HashMap<String, (Presence, f64)>,
    sent: Option<(Presence, f64)>,
    announce: bool,
    /// The document's name as last sent to the messenger.
    named: Option<String>,
}

impl Shared {
    /// Shares `app`'s document with the chat and starts listening for edits.
    pub fn new(api: JsValue, app: &mut WordApp, ctx: egui::Context) -> Shared {
        let me = Reflect::get(&api, &"selfAddr".into()).ok().and_then(|a| a.as_string()).unwrap_or_default();
        let mut collab = Collab::new(&mut app.session, client_id());
        // An op travels base64-encoded inside a Yjs update: three quarters of the limit, less room
        // for the update's own bytes.
        let max = Reflect::get(&api, &"sendUpdateMaxSize".into())
            .ok()
            .and_then(|v| v.as_f64())
            .filter(|v| *v > 0.0)
            .map_or(DEFAULT_MAX_UPDATE, |v| v as usize);
        collab.max_op = (max / 4 * 3).saturating_sub(2_000);
        app.set_loading(true);
        let mut shared = Shared {
            api,
            channel: None,
            collab,
            ctx,
            me,
            received: Rc::default(),
            replayed: Rc::default(),
            loading: true,
            unsent: Rc::default(),
            stored_at: 0.0,
            heard: Rc::default(),
            peers: HashMap::new(),
            sent: None,
            announce: false,
            named: None,
        };
        shared.listen();
        shared
    }

    fn listen(&mut self) {
        let (received, repaint) = (self.received.clone(), self.ctx.clone());
        let on_update = Closure::<dyn FnMut(JsValue)>::new(move |update: JsValue| {
            let y = Reflect::get(&update, &"payload".into()).and_then(|p| Reflect::get(&p, &"y".into())).ok().and_then(|y| y.as_string());
            if let Some(bytes) = y.as_deref().and_then(base64_decode) {
                received.borrow_mut().push(bytes);
                repaint.request_repaint();
            }
        });
        match call(&self.api, "setUpdateListener", &[on_update.as_ref().clone(), 0.into()]) {
            Ok(done) => {
                // Lives as long as the page.
                on_update.forget();
                // The promise resolves once the stored updates are in.
                let (replayed, repaint) = (self.replayed.clone(), self.ctx.clone());
                match done.dyn_into::<js_sys::Promise>() {
                    Ok(done) => wasm_bindgen_futures::spawn_local(async move {
                        let _ = wasm_bindgen_futures::JsFuture::from(done).await;
                        replayed.set(true);
                        repaint.request_repaint();
                    }),
                    Err(_) => self.replayed.set(true),
                }
            }
            Err(e) => {
                log::error!("couldn't listen for shared edits: {e:?}");
                self.replayed.set(true);
            }
        }
        let has_realtime = Reflect::get(&self.api, &"joinRealtimeChannel".into()).is_ok_and(|f| f.is_function());
        if let Some(channel) = has_realtime.then(|| call(&self.api, "joinRealtimeChannel", &[]).ok()).flatten() {
            let (received, heard, ctx) = (self.received.clone(), self.heard.clone(), self.ctx.clone());
            let on_data = Closure::<dyn FnMut(JsValue)>::new(move |data: JsValue| {
                let Some(data) = data.dyn_ref::<Uint8Array>().map(Uint8Array::to_vec) else { return };
                match data.split_first() {
                    Some((&TAG_UPDATE, update)) => received.borrow_mut().push(update.to_vec()),
                    Some((&TAG_PRESENCE, json)) => match serde_json::from_slice(json) {
                        Ok(presence) => heard.borrow_mut().push(presence),
                        Err(_) => return,
                    },
                    _ => return,
                }
                ctx.request_repaint();
            });
            if call(&channel, "setListener", &[on_data.as_ref().clone()]).is_ok() {
                on_data.forget();
                self.channel = Some(channel);
            }
        }
        // Leaving the page stores what hasn't been yet.
        let (api, unsent) = (self.api.clone(), self.unsent.clone());
        let on_hide = Closure::<dyn FnMut()>::new(move || store(&api, &mut unsent.borrow_mut(), None));
        if let Some(w) = web_sys::window()
            && w.add_event_listener_with_callback("pagehide", on_hide.as_ref().unchecked_ref()).is_ok()
        {
            on_hide.forget();
        }
    }

    /// Before the frame: applies what the others sent.
    pub fn before(&mut self, app: &mut WordApp) {
        self.hear(app);
        let received = std::mem::take(&mut *self.received.borrow_mut());
        if !received.is_empty() {
            let mine = self.collab.remote(&mut app.session, received, js_sys::Date::now());
            if let Some(update) = mine {
                self.send(update);
            }
        }
        if self.loading && self.replayed.get() {
            self.loading = false;
            app.set_loading(false);
        }
        self.after_change(app);
    }

    /// After the frame: shares the edits made in it, and where this member is.
    pub fn after(&mut self, app: &mut WordApp) {
        if self.loading {
            return;
        }
        let now = js_sys::Date::now();
        if let Some(update) = self.collab.local(&mut app.session, now) {
            self.send(update);
        }
        if let Some(why) = self.collab.refused.take() {
            app.status(why);
        }
        self.after_change(app);
        if !self.unsent.borrow().is_empty() {
            if now - self.stored_at >= STORE_EVERY_MS {
                self.stored_at = now;
                let name = self.collab.file_name();
                let document = (name != self.named).then(|| name.clone()).flatten();
                self.named = name;
                store(&self.api, &mut self.unsent.borrow_mut(), document);
            } else {
                self.ctx.request_repaint_after(std::time::Duration::from_millis(STORE_EVERY_MS as u64));
            }
        }
        // Held typing is shared once it pauses.
        self.ctx.request_repaint_after(std::time::Duration::from_millis(800));
        self.tell(&app.session);
    }

    /// Opening a file replaces everyone's document: the app asks first once there is one.
    fn after_change(&self, app: &mut WordApp) {
        app.session.dirty = !self.collab.is_empty();
    }

    /// Sends `update` over the realtime channel at once and queues it to be stored.
    fn send(&mut self, update: Vec<u8>) {
        if let Some(channel) = &self.channel
            && update.len() < REALTIME_MAX
            && let Err(e) = call(channel, "send", &[Uint8Array::from([&[TAG_UPDATE], update.as_slice()].concat().as_slice()).into()])
        {
            log::warn!("realtime send failed: {e:?}");
        }
        self.unsent.borrow_mut().push(update);
    }

    /// Takes in where the others are, and drops those gone quiet.
    fn hear(&mut self, app: &mut WordApp) {
        let now = js_sys::Date::now();
        for presence in std::mem::take(&mut *self.heard.borrow_mut()) {
            // Hostile input: bound what one member can make every frame draw.
            let sane = presence.sel.anchor.path.0.len() <= MAX_PATH && presence.sel.focus.path.0.len() <= MAX_PATH;
            if presence.id != self.me && sane && (self.peers.len() < MAX_PEERS || self.peers.contains_key(&presence.id)) {
                let mut presence = presence;
                presence.name = presence.name.chars().take(MAX_NAME).collect();
                self.announce |= !self.peers.contains_key(&presence.id);
                self.peers.insert(presence.id.clone(), (presence, now));
            }
        }
        self.peers.retain(|_, (_, seen)| now - *seen < GONE_MS);
        let mut peers: Vec<Peer> = self
            .peers
            .values()
            .map(|(p, _)| Peer {
                name: p.name.clone(),
                color: p.id.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize)),
                sel: p.sel.clone(),
            })
            .collect();
        peers.sort_by(|a, b| a.name.cmp(&b.name));
        app.peers = peers;
    }

    /// Sends this member's selection when it moved (throttled), when someone new arrived, and as
    /// a heartbeat.
    fn tell(&mut self, s: &Session) {
        let Some(channel) = &self.channel else { return };
        // Wake for the heartbeat and to drop members who left.
        self.ctx.request_repaint_after(std::time::Duration::from_millis(1_000));
        let here = Presence { id: self.me.clone(), name: s.author.clone(), sel: s.sel.clone() };
        let now = js_sys::Date::now();
        let due = match &self.sent {
            None => true,
            Some((sent, at)) => self.announce || now - at >= HEARTBEAT_MS || (*sent != here && now - at >= PRESENCE_EVERY_MS),
        };
        if !due {
            return;
        }
        let Ok(json) = serde_json::to_vec(&here) else { return };
        match call(channel, "send", &[Uint8Array::from([&[TAG_PRESENCE], json.as_slice()].concat().as_slice()).into()]) {
            Ok(_) => {
                self.sent = Some((here, now));
                self.announce = false;
            }
            Err(e) => log::warn!("couldn't share the selection: {e:?}"),
        }
    }
}

/// Stores the queued updates as one (merged), naming the document when `document` says so.
fn store(api: &JsValue, unsent: &mut Vec<Vec<u8>>, document: Option<String>) {
    if unsent.is_empty() {
        return;
    }
    let update = match yrs::merge_updates_v1(unsent.iter().map(Vec::as_slice)) {
        Ok(u) => u,
        Err(e) => {
            log::warn!("couldn't merge shared updates: {e}");
            return;
        }
    };
    unsent.clear();
    let mut fields = vec![("payload", object(&[("y", base64_encode(&update).into())]))];
    if let Some(name) = document {
        let stem = std::path::Path::new(&name).file_stem().map_or_else(|| name.clone(), |s| s.to_string_lossy().to_string());
        fields.push(("document", stem.into()));
    }
    if let Err(e) = call(api, "sendUpdate", &[object(&fields), "".into()]) {
        log::error!("couldn't share an edit: {e:?}");
    }
}

/// This member and device's client id: kept in the page's storage between runs (a new one each
/// run where there is none).
fn client_id() -> u64 {
    let storage = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
    if let Some(id) = storage.as_ref().and_then(|s| s.get_item(CLIENT_KEY).ok().flatten()).and_then(|v| v.parse().ok()) {
        return id;
    }
    let id = (js_sys::Math::random() * (1u64 << 53) as f64) as u64 | 1;
    if let Some(s) = storage {
        let _ = s.set_item(CLIENT_KEY, &id.to_string());
    }
    id
}

fn call(this: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let f: Function = Reflect::get(this, &method.into())?.dyn_into()?;
    f.apply(this, &args.iter().collect::<Array>())
}

fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let o = Object::new();
    for (key, value) in fields {
        let _ = Reflect::set(&o, &(*key).into(), value);
    }
    o.into()
}
