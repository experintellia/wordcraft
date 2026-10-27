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
    assert!(!ime(&h), "no keyboard before a tap on the page (or a key, a mouse click)");

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

/// The interactable windows, menus and popups shown over the page.
fn floating(h: &egui_kittest::Harness<'_, WordApp>) -> Vec<egui::Rect> {
    let layers: Vec<egui::LayerId> =
        h.ctx.memory(|m| m.layer_ids().filter(|l| l.order != egui::Order::Background && m.areas().is_visible(l)).collect());
    layers.into_iter().filter_map(|l| egui::AreaState::load(&h.ctx, l.id)).filter(|a| a.interactable).map(|a| a.rect()).collect()
}

/// The page's zones lie around what covers it (holes in the input), never under it.
fn assert_around(pieces: &[egui::Rect], covers: &[egui::Rect]) {
    assert!(!pieces.is_empty(), "the rest of the page still takes a tap");
    for p in pieces {
        assert!(covers.iter().all(|c| !p.intersects(c.shrink(1.0))), "{p:?} lies under one of {covers:?}");
    }
}

#[test]
fn a_dialog_over_the_page_cuts_a_hole_in_the_text_input() {
    let mut h = harness(true);
    h.state_mut().run("format.fontDialog", json!({})).unwrap();
    h.run_steps(3);
    assert!(h.state().dialog.is_some());
    let (text, whole) = zones(&mut h);
    let covers = floating(&h);
    assert!(!covers.is_empty());
    let (fields, pieces): (Vec<_>, Vec<_>) = text.into_iter().partition(|z| covers.iter().any(|c| c.contains_rect(*z)));
    assert!(!fields.is_empty(), "the dialog's fields take a tap");
    assert_around(&pieces, &covers);
    assert!(whole.is_empty(), "without clip paths the input lies over no page partly covered");
    let hole = covers[0].center();
    if !fields.iter().any(|z| z.contains(hole)) {
        assert_eq!(wordcraft_ui_egui::tap_text_zone(&h.ctx, hole), None, "a tap in the hole reaches the dialog");
    }
}

#[test]
fn a_menu_open_leaves_the_page_to_dismiss_it() {
    let mut h = harness(true);
    let p = end_of_text(&mut h);
    assert!(wordcraft_ui_egui::tap_opens_keyboard(&h.ctx, p));
    h.input_mut().events.extend(wordcraft_ui_egui::long_press_events(TouchPhase::Start, p));
    h.step();
    h.input_mut().events.extend(wordcraft_ui_egui::long_press_events(TouchPhase::End, p));
    h.run_steps(2);
    assert!(h.state().canvas.context_menu_open);
    let (text, pages) = zones(&mut h);
    assert!(pages.is_empty() && text.is_empty(), "a tap beside the menu (the page, the ribbon's boxes) only dismisses it: {text:?}");
    assert!(!wordcraft_ui_egui::tap_opens_keyboard(&h.ctx, p), "nor opens the keyboard from the canvas");
    assert_eq!(wordcraft_ui_egui::tap_text_zone(&h.ctx, p), None);
}

#[test]
fn a_tap_that_reaches_the_canvas_on_a_page_opens_the_keyboard() {
    let mut h = harness(true);
    let p = end_of_text(&mut h);
    // The keyboard (a hardware one) is up: the input follows the caret, taps reach the canvas.
    wordcraft_ui_egui::set_keyboard_up(&h.ctx, true);
    h.run_steps(2);
    assert!(zones(&mut h).0.is_empty());
    assert!(wordcraft_ui_egui::tap_opens_keyboard(&h.ctx, p), "a page under the tap");
    let ribbon = egui::pos2(p.x, 40.0);
    assert!(!wordcraft_ui_egui::tap_opens_keyboard(&h.ctx, ribbon), "not the ribbon");
}

#[test]
fn a_hardware_key_on_the_page_takes_the_platform_text_input() {
    let mut h = harness(true);
    assert!(!ime(&h));
    // A dead key (as eframe reports one: its physical key), then the letter it composes.
    h.event(egui::Event::Key {
        key: egui::Key::Quote,
        physical_key: Some(egui::Key::Quote),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    step(&mut h);
    assert!(wordcraft_ui_egui::keyboard_up(&h.ctx));
    step(&mut h);
    assert!(ime(&h), "IME from the first key on, so dead keys and input methods compose");
    h.event(egui::Event::Ime(egui::ImeEvent::Commit("é".into())));
    h.run_steps(2);
    assert!(h.state().session.doc.plain_text(wordcraft_doc::StoryRef::Body).starts_with("é"));
}

#[test]
fn a_mouse_click_on_the_page_takes_the_platform_text_input_a_finger_tap_does_not() {
    let mut h = harness(true);
    let p = end_of_text(&mut h);
    let button = |pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let touch = |phase| egui::Event::Touch { device_id: egui::TouchDeviceId(0), id: TouchId(7), phase, pos: p, force: None };
    // A finger as eframe reports it: its `pointerup` runs a frame before its `touchend`.
    h.input_mut().events.extend([egui::Event::PointerMoved(p), button(true), touch(TouchPhase::Start)]);
    step(&mut h);
    h.input_mut().events.push(button(false));
    step(&mut h);
    h.input_mut().events.extend([egui::Event::PointerGone, touch(TouchPhase::End)]);
    step(&mut h);
    assert!(!wordcraft_ui_egui::keyboard_up(&h.ctx), "a finger on the canvas brings no keyboard");
    assert!(!ime(&h));
    // A mouse click (iPad trackpad: the input takes no pointer events then).
    h.run_steps(30);
    h.input_mut().events.extend([
        egui::Event::PointerMoved(p + egui::vec2(-20.0, 0.0)),
        egui::Event::PointerButton {
            pos: p + egui::vec2(-20.0, 0.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    step(&mut h);
    h.input_mut().events.push(egui::Event::PointerButton {
        pos: p + egui::vec2(-20.0, 0.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    step(&mut h);
    assert!(wordcraft_ui_egui::keyboard_up(&h.ctx));
    step(&mut h);
    assert!(ime(&h), "a keyboard is likely there: the page asks for IME");
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
