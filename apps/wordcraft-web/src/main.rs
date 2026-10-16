//! WordCraft in the browser.
//!
//! Runs the same [`wordcraft_ui_egui::WordApp`] as the desktop app through eframe's web runner
//! (WebGL2, through glow). Build with `trunk build --release` from this directory (output in
//! `dist/web`); `trunk build webxdc.html` builds the webxdc app (`packaging/webxdc/package.sh`).
//!
//! Differences from the desktop app: no TCP control channel; Open and Insert › Pictures use the
//! browser file picker (bytes arrive through `Services::inbox`); Save and Export download.
//! Preferences (theme, interface size, editing options…) are kept in `localStorage` (`prefs`).
//! URL flags: `?sample` opens the sample document. Inside a messenger (webxdc) the app is one
//! document shared in the chat (`web::webxdc`).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(any(target_arch = "wasm32", test))]
mod prefs;
#[cfg(any(target_arch = "wasm32", test))]
mod save_fallback;

#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(target_arch = "wasm32")]
fn main() {
    web::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("wordcraft-web only runs in the browser: build it with `trunk build --release` in apps/wordcraft-web");
}
