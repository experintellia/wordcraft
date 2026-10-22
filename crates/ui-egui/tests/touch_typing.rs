//! Typing on touch screens (the web build on phones and tablets, `touch.rs`): the visible pages and
//! the text fields are text zones, where the web shell lays its text input so that a tap opens the
//! on-screen keyboard; the tap then places the caret or focuses the field. Gestures it forwards
//! still reach the page, and desktops are unchanged.

use egui::{Pos2, TouchId, TouchPhase};
use serde_json::json;
use wordcraft_engine::Session;
use wordcraft_ui_egui::{Services, WordApp, canvas};

const TEXT: &str = "Kilns fire the clay overnight.\nGlazes go on in the morning.\nThe studio opens at noon.";

fn harness(touch: bool) -> egui_kittest::Harness<'static, WordApp> {
    let app = WordApp::new(Session::new(wordcraft_doc::Document::from_text(TEXT)), Services::default());
    let mut h = egui_kittest::Harness::builder().with_step_dt(1.0 / 60.0).with_size(egui::vec2(1200.0, 900.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    wordcraft_ui_egui::set_touch_screen(&h.ctx, touch);
    h.run_steps(6);
    h
}

/// A frame, with the input the web shell's `raw_input_hook` adds (as eframe runs it).
fn step(h: &mut egui_kittest::Harness<'_, WordApp>) {
    let mut raw = std::mem::take(h.input_mut());
    h.state_mut().raw_input_hook(&mut raw);
    *h.input_mut() = raw;
    h.step();
}

fn ime(h: &egui_kittest::Harness<'_, WordApp>) -> bool {
    h.output().platform_output.ime.is_some()
}

/// The zones as the web shell reads them, at the end of the pass.
fn zones(h: &mut egui_kittest::Harness<'_, WordApp>) -> (Vec<egui::Rect>, Vec<egui::Rect>) {
    let id = egui::Id::new("test.zones");
    h.ctx.on_end_pass(
        "test.zones",
        std::sync::Arc::new(move |ui: &mut egui::Ui| {
            let zones = (wordcraft_ui_egui::text_zones(ui.ctx()), wordcraft_ui_egui::page_zones(ui.ctx()));
            ui.ctx().data_mut(|d| d.insert_temp(id, zones));
        }),
    );
    h.step();
    h.ctx.data(|d| d.get_temp(id)).unwrap_or_default()
}

/// The screen point of the end of the document's text, with the caret left at the start.
fn end_of_text(h: &mut egui_kittest::Harness<'_, WordApp>) -> Pos2 {
    h.state_mut().run("caret.docEnd", json!({})).unwrap();
    h.step();
    let (p, height) = canvas::caret_screen(h.state_mut()).unwrap();
    h.state_mut().run("caret.docStart", json!({})).unwrap();
    h.step();
    p + egui::vec2(-2.0, height / 2.0)
}

fn caret(h: &egui_kittest::Harness<'_, WordApp>) -> wordcraft_doc::Pos {
    h.state().session.sel.focus.clone()
}

#[test]
fn a_tap_on_the_page_places_the_caret_with_the_keyboard_up() {
    let mut h = harness(true);
    let p = end_of_text(&mut h);
    let start = caret(&h);
    let (text, pages) = zones(&mut h);
    assert!(pages.iter().any(|z| z.contains(p)), "the web shell's input lies over the page: {pages:?}");
    assert!(pages.iter().all(|z| text.contains(z)));
    assert!(!ime(&h), "no keyboard before a tap on the page");

    // The input over the page took the tap (and the focus): the shell reports it.
    assert!(wordcraft_ui_egui::tap_text_zone(&h.ctx, p).is_some());
    assert!(wordcraft_ui_egui::keyboard_up(&h.ctx));
    step(&mut h);
    assert!(ime(&h), "the page asks for the keyboard from the frame after the tap");
    for _ in 0..3 {
        step(&mut h);
        assert!(ime(&h));
    }
    assert_ne!(caret(&h), start, "the caret moved to where the page was tapped");
    assert_eq!(caret(&h).path, wordcraft_doc::Path(vec![2]), "into the last paragraph, tapped");
    assert!(zones(&mut h).0.is_empty(), "while typing the input follows the caret");

    // Typing goes into the page.
    h.event(egui::Event::Text("!".into()));
    h.run_steps(2);
    assert!(h.state().session.doc.plain_text(wordcraft_doc::StoryRef::Body).ends_with("noon.!"));

    // The keyboard went down: the page takes a tap again, and asks for no keyboard.
    wordcraft_ui_egui::set_keyboard_up(&h.ctx, false);
    let (_, pages) = zones(&mut h);
    assert!(!pages.is_empty());
    assert!(!ime(&h));
}

#[test]
fn a_tap_on_a_text_field_focuses_it_with_the_keyboard_up() {
    let mut h = harness(true);
    h.state_mut().run("view.navigationPane", json!({"value": true})).unwrap();
    h.run_steps(3);
    let (text, pages) = zones(&mut h);
    let fields: Vec<_> = text.into_iter().filter(|z| !pages.contains(z)).collect();
    assert!(!fields.is_empty(), "the Navigation pane's search box takes a tap");
    let field = fields[0];
    assert_eq!(wordcraft_ui_egui::tap_text_zone(&h.ctx, field.center()), Some(field));
    h.step();
    let focused = h.ctx.memory(|m| m.focused()).and_then(|id| h.ctx.read_response(id)).map(|r| r.interact_rect);
    assert_eq!(focused, Some(field), "the field tapped has the keyboard");
    assert!(ime(&h));
    h.event(egui::Event::Text("clay".into()));
    h.run_steps(2);
    assert_eq!(caret(&h).off, 0, "typing went into the field, not the page");
    assert!(!h.state().session.doc.plain_text(wordcraft_doc::StoryRef::Body).contains("clayKilns"));
}

#[test]
fn no_text_input_over_a_page_a_dialog_covers() {
    let mut h = harness(true);
    let before = zones(&mut h);
    h.state_mut().run("format.fontDialog", json!({})).unwrap();
    h.run_steps(3);
    assert!(h.state().dialog.is_some());
    let (text, pages) = zones(&mut h);
    assert!(pages.len() < before.1.len() || pages.is_empty(), "the page under the dialog takes no tap: {pages:?}");
    assert!(text.iter().any(|z| !pages.contains(z)), "the dialog's fields do");
}

#[test]
fn a_drag_on_the_page_still_selects() {
    let mut h = harness(true);
    let to = end_of_text(&mut h);
    let from = canvas::caret_screen(h.state_mut()).unwrap().0 + egui::vec2(2.0, 4.0);
    let mid = from.lerp(to, 0.5);
    for (phase, pos) in [
        (TouchPhase::Start, from),
        (TouchPhase::Move, from + egui::vec2(12.0, 0.0)),
        (TouchPhase::Move, mid),
        (TouchPhase::Move, to),
        (TouchPhase::End, to),
    ] {
        h.input_mut().events.extend(wordcraft_ui_egui::finger_events(phase, TouchId(1), pos));
        h.step();
    }
    h.step();
    assert!(!h.state().session.sel.is_collapsed(), "the drag selected text");
    assert!(!wordcraft_ui_egui::keyboard_up(&h.ctx), "and brought no keyboard up");
}

#[test]
fn a_long_press_on_the_page_opens_the_context_menu() {
    let mut h = harness(true);
    let p = end_of_text(&mut h);
    h.input_mut().events.extend(wordcraft_ui_egui::long_press_events(TouchPhase::Start, p));
    h.step();
    h.input_mut().events.extend(wordcraft_ui_egui::long_press_events(TouchPhase::End, p));
    h.run_steps(2);
    assert!(h.state().canvas.context_menu_open);
}

#[test]
fn desktops_are_unchanged() {
    let mut h = harness(false);
    assert!(ime(&h), "the focused page asks for IME at the caret");
    let (text, pages) = zones(&mut h);
    assert!(text.is_empty() && pages.is_empty(), "no text zones without a touch screen");
    h.state_mut().run("view.navigationPane", json!({"value": true})).unwrap();
    h.run_steps(3);
    assert!(zones(&mut h).0.is_empty());
}
