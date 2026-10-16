//! Running as a webxdc app (Delta Chat and other messengers): the page loads the messenger's
//! `webxdc.js`, which sets `window.webxdc`. The app is then one document
//! ([`Services::single_document`]): files come from the chat or the device through
//! `webxdc.importFiles` (File › Replace with File…, Insert › Pictures), and Save As and Export
//! hand a copy to the chat through `webxdc.sendToChat`. There is no printing (the messenger's
//! sandbox has no print dialog) and no added fonts: every member lays the text out with the
//! fonts bundled in the app.

use js_sys::{Array, Function, Object, Reflect, Uint8Array};
use wasm_bindgen::{JsCast as _, JsValue};
use wordcraft_engine::Session;
use wordcraft_engine::cmd::insert::base64_encode;
use wordcraft_ui_egui::async_saves::SaveOutcome;
use wordcraft_ui_egui::{Inbox, Services};

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
    // The chat keeps the document: leaving the app loses nothing a guard could save.
    services.on_dirty = None;
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
