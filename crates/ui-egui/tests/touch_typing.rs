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
    long_press(&mut h, p);
    lift(&mut h, p);
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

/// A finger's events at `pos`, as eframe sends them, and a frame.
fn finger(h: &mut egui_kittest::Harness<'_, WordApp>, phase: TouchPhase, pos: Pos2) {
    h.input_mut().events.extend(wordcraft_ui_egui::finger_events(phase, TouchId(1), pos));
    step(h);
}

/// A finger held still at `p` until it's a long press.
fn long_press(h: &mut egui_kittest::Harness<'_, WordApp>, p: Pos2) {
    finger(h, TouchPhase::Start, p);
    for _ in 0..60 {
        step(h);
    }
}

/// The finger lifts at `p`, and the frames after.
fn lift(h: &mut egui_kittest::Harness<'_, WordApp>, p: Pos2) {
    finger(h, TouchPhase::End, p);
    for _ in 0..4 {
        step(h);
    }
}

/// A WordCraft window on a document taller than the window.
fn long_document(touch: bool) -> egui_kittest::Harness<'static, WordApp> {
    let text = (1..=300).map(|i| format!("Line {i} of the glaze notes.")).collect::<Vec<_>>().join("\n");
    let app = WordApp::new(Session::new(wordcraft_doc::Document::from_text(&text)), Services::default());
    let mut h = egui_kittest::Harness::builder().with_step_dt(1.0 / 60.0).with_size(egui::vec2(1200.0, 900.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    wordcraft_ui_egui::set_touch_screen(&h.ctx, touch);
    for _ in 0..6 {
        step(&mut h);
    }
    h
}

fn page_middle(h: &egui_kittest::Harness<'_, WordApp>) -> Pos2 {
    h.state().canvas.viewport.unwrap().center()
}

#[test]
fn a_finger_drag_scrolls_the_page_and_leaves_the_caret() {
    let mut h = long_document(true);
    let p = page_middle(&h);
    let (caret_before, offset) = (caret(&h), h.state().canvas.scroll_offset);
    finger(&mut h, TouchPhase::Start, p);
    // Slowly, so it doesn't fling: the page follows the finger 1:1.
    for i in 1..=40 {
        finger(&mut h, TouchPhase::Move, p - egui::vec2(0.0, 5.0 * i as f32));
    }
    step(&mut h);
    assert!((h.state().canvas.scroll_offset.y - offset.y - 200.0).abs() < 1.0, "{:?} -> {:?}", offset, h.state().canvas.scroll_offset);
    lift(&mut h, p - egui::vec2(0.0, 200.0));
    assert_eq!(caret(&h), caret_before, "the caret stays");
    assert!(h.state().session.sel.is_collapsed(), "nothing selected");
    assert!(!wordcraft_ui_egui::keyboard_up(&h.ctx) && !ime(&h), "and no keyboard");
}

#[test]
fn a_finger_drag_scrolls_sideways_on_a_page_wider_than_the_window_and_keeps_the_keyboard() {
    let mut h = long_document(true);
    h.state_mut().run("view.zoom", json!({"value": 300})).unwrap();
    for _ in 0..4 {
        step(&mut h);
    }
    wordcraft_ui_egui::set_keyboard_up(&h.ctx, true);
    step(&mut h);
    assert!(ime(&h));
    let p = page_middle(&h);
    let offset = h.state().canvas.scroll_offset;
    finger(&mut h, TouchPhase::Start, p);
    for i in 1..=20 {
        finger(&mut h, TouchPhase::Move, p - egui::vec2(4.0 * i as f32, 2.0 * i as f32));
    }
    step(&mut h);
    let now = h.state().canvas.scroll_offset;
    assert!((now.x - offset.x - 80.0).abs() < 1.0 && (now.y - offset.y - 40.0).abs() < 1.0, "{offset:?} -> {now:?}");
    assert!(wordcraft_ui_egui::keyboard_up(&h.ctx) && ime(&h), "the keyboard stays up while the page scrolls");
    finger(&mut h, TouchPhase::Cancel, p);
}

#[test]
fn a_fling_coasts_on_and_a_new_touch_stops_it() {
    let mut h = long_document(true);
    let p = page_middle(&h);
    finger(&mut h, TouchPhase::Start, p);
    for i in 1..=6 {
        finger(&mut h, TouchPhase::Move, p - egui::vec2(0.0, 30.0 * i as f32));
    }
    finger(&mut h, TouchPhase::End, p - egui::vec2(0.0, 180.0));
    let lifted = h.state().canvas.scroll_offset.y;
    for _ in 0..10 {
        step(&mut h);
    }
    let coasted = h.state().canvas.scroll_offset.y;
    assert!(coasted > lifted + 50.0, "the page coasts on after the finger lifts: {lifted} -> {coasted}");
    finger(&mut h, TouchPhase::Start, p);
    let stopped = h.state().canvas.scroll_offset.y;
    for _ in 0..10 {
        step(&mut h);
    }
    assert!((h.state().canvas.scroll_offset.y - stopped).abs() < 0.5, "a touch stops the fling");
    finger(&mut h, TouchPhase::Cancel, p);
}

#[test]
fn a_finger_tap_on_the_page_places_the_caret() {
    let mut h = harness(true);
    let p = end_of_text(&mut h);
    let start = caret(&h);
    finger(&mut h, TouchPhase::Start, p);
    step(&mut h);
    assert_eq!(caret(&h), start, "not on the press: it may turn into a scroll");
    lift(&mut h, p);
    assert_eq!(caret(&h).path, wordcraft_doc::Path(vec![2]), "the tap placed the caret");
    assert!(h.state().session.sel.is_collapsed());
}

#[test]
fn a_tap_the_text_input_takes_leaves_the_caret_to_it() {
    let mut h = harness(true);
    let p = end_of_text(&mut h);
    let start = caret(&h);
    // The web shell cancels a tap its text input takes (the focus then clicks the page).
    finger(&mut h, TouchPhase::Start, p);
    finger(&mut h, TouchPhase::Cancel, p);
    for _ in 0..4 {
        step(&mut h);
    }
    assert_eq!(caret(&h), start);
}

#[test]
fn a_long_press_selects_the_word_and_a_drag_extends_it() {
    let mut h = harness(true);
    let to = end_of_text(&mut h);
    let from = canvas::caret_screen(h.state_mut()).unwrap().0 + egui::vec2(12.0, 6.0);
    long_press(&mut h, from);
    let word = h.state().session.selected_text();
    assert_eq!(word.trim(), "Kilns", "the word under the finger");
    for i in 1..=10 {
        finger(&mut h, TouchPhase::Move, from.lerp(to, i as f32 / 10.0));
    }
    lift(&mut h, to);
    let text = h.state().session.selected_text();
    assert!(text.starts_with("Kilns") && text.contains("noon"), "the drag extended it: {text:?}");
    assert!(!h.state().canvas.context_menu_open, "no menu after a drag");
}

#[test]
fn a_long_press_lifted_opens_the_context_menu_on_the_word() {
    let mut h = harness(true);
    let p = canvas::caret_screen(h.state_mut()).unwrap().0 + egui::vec2(12.0, 6.0);
    long_press(&mut h, p);
    lift(&mut h, p);
    assert!(h.state().canvas.context_menu_open);
    assert_eq!(h.state().session.selected_text().trim(), "Kilns", "the menu is for the word");
}

#[test]
fn a_mouse_drag_still_selects_on_desktops() {
    let mut h = harness(false);
    let to = end_of_text(&mut h);
    let from = canvas::caret_screen(h.state_mut()).unwrap().0 + egui::vec2(2.0, 4.0);
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.input_mut().events.extend([egui::Event::PointerMoved(from), button(from, true)]);
    step(&mut h);
    for i in 1..=8 {
        h.input_mut().events.push(egui::Event::PointerMoved(from.lerp(to, i as f32 / 8.0)));
        step(&mut h);
    }
    h.input_mut().events.push(button(to, false));
    step(&mut h);
    assert!(!h.state().session.sel.is_collapsed(), "the drag selected text");
}

/// Finger `id`'s events at `pos`, as eframe sends them, and a frame.
fn finger_id(h: &mut egui_kittest::Harness<'_, WordApp>, id: u64, phase: TouchPhase, pos: Pos2) {
    h.input_mut().events.extend(wordcraft_ui_egui::finger_events(phase, TouchId(id), pos));
    step(h);
}

fn drag_up(h: &mut egui_kittest::Harness<'_, WordApp>, id: u64, p: Pos2, by: f32) {
    finger_id(h, id, TouchPhase::Start, p);
    for i in 1..=10 {
        finger_id(h, id, TouchPhase::Move, p - egui::vec2(0.0, by * i as f32 / 10.0));
    }
}

#[test]
fn a_finger_whose_end_was_lost_doesnt_lock_the_page() {
    let mut h = long_document(true);
    let p = page_middle(&h);
    // A drag whose end nobody reported (lifted over the keyboard, outside the view). egui has its
    // touch all along, so eframe goes on reporting the finger wherever it is.
    drag_up(&mut h, 1, p, 100.0);
    assert!(touches(&h), "egui follows the scrolling finger's touch");
    // The web shell then sees a finger land alone on the screen: the lost one is dropped.
    land_alone(&mut h, 9, LANDING);
    finger_id(&mut h, 9, TouchPhase::Cancel, LANDING);
    assert!(!touches(&h) && !pressed(&h), "nothing of it is left in egui");
    let before = h.state().canvas.scroll_offset.y;
    drag_up(&mut h, 2, p, 100.0);
    step(&mut h);
    assert!(h.state().canvas.scroll_offset.y > before + 90.0, "the next drag scrolls");
    finger_id(&mut h, 2, TouchPhase::End, p);
    for _ in 0..30 {
        step(&mut h);
    }
    // And a mouse click reaches the page.
    h.state_mut().run("caret.docStart", json!({})).unwrap();
    step(&mut h);
    let start = caret(&h);
    let click = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let at = p + egui::vec2(0.0, 30.0);
    h.input_mut().events.extend([egui::Event::PointerMoved(at), click(at, true)]);
    step(&mut h);
    h.input_mut().events.push(click(at, false));
    step(&mut h);
    assert_ne!(caret(&h), start, "the mouse click placed the caret");
}

/// Where a later finger lands, on the page.
const LANDING: Pos2 = egui::pos2(600.0, 500.0);

/// A finger lands alone on the screen: the web shell tells egui (`finger_alone`) in the frame of
/// the finger's own touchstart.
fn land_alone(h: &mut egui_kittest::Harness<'_, WordApp>, id: u64, pos: Pos2) {
    wordcraft_ui_egui::touch_gestures::finger_alone(&h.ctx);
    h.input_mut().events.extend(wordcraft_ui_egui::finger_events(TouchPhase::Start, TouchId(id), pos));
    step(h);
}

fn touches(h: &egui_kittest::Harness<'_, WordApp>) -> bool {
    h.ctx.input(|i| i.any_touches())
}

fn pressed(h: &egui_kittest::Harness<'_, WordApp>) -> bool {
    h.ctx.input(|i| i.pointer.any_down())
}

#[test]
fn a_long_press_is_a_pointer_press_for_egui_not_a_touch() {
    let mut h = harness(true);
    let p = canvas::caret_screen(h.state_mut()).unwrap().0 + egui::vec2(12.0, 6.0);
    long_press(&mut h, p);
    assert!(!touches(&h), "egui's own long touch would open the menu");
    assert!(pressed(&h), "the word is held, to drag it on");
    // Its end lost, and a finger landing alone later drops it.
    land_alone(&mut h, 9, LANDING);
    finger_id(&mut h, 9, TouchPhase::Cancel, LANDING);
    step(&mut h);
    assert!(!touches(&h) && !pressed(&h));
}

#[test]
fn a_finger_landing_right_after_a_tap_leaves_no_button_pressed() {
    let mut h = harness(true);
    let p = end_of_text(&mut h);
    finger(&mut h, TouchPhase::Start, p);
    // The tap's release is queued for the next frame; a finger lands alone before it.
    finger(&mut h, TouchPhase::End, p);
    land_alone(&mut h, 9, LANDING);
    finger_id(&mut h, 9, TouchPhase::Cancel, LANDING);
    for _ in 0..3 {
        step(&mut h);
    }
    assert!(!pressed(&h), "the release still reached egui");
    assert_eq!(caret(&h).path, wordcraft_doc::Path(vec![2]), "and the tap placed the caret");
}

#[test]
fn a_finger_landing_right_after_a_long_press_leaves_no_button_pressed() {
    let mut h = harness(true);
    let p = canvas::caret_screen(h.state_mut()).unwrap().0 + egui::vec2(12.0, 6.0);
    long_press(&mut h, p);
    finger(&mut h, TouchPhase::End, p);
    step(&mut h);
    // The menu's secondary click is still queued when a finger lands alone.
    land_alone(&mut h, 9, LANDING);
    finger_id(&mut h, 9, TouchPhase::Cancel, LANDING);
    for _ in 0..3 {
        step(&mut h);
    }
    assert!(!pressed(&h) && !touches(&h));
}

#[test]
fn a_pinch_whose_ends_were_lost_leaves_no_touches() {
    let mut h = long_document(true);
    let p = page_middle(&h);
    finger_id(&mut h, 1, TouchPhase::Start, p);
    finger_id(&mut h, 2, TouchPhase::Start, p + egui::vec2(100.0, 0.0));
    assert!(touches(&h));
    // The app went to the background: no ends. A finger lands alone afterwards.
    land_alone(&mut h, 9, LANDING);
    finger_id(&mut h, 9, TouchPhase::Cancel, LANDING);
    assert!(!touches(&h), "no phantom fingers left");
    let before = h.state().canvas.scroll_offset.y;
    drag_up(&mut h, 3, p, 100.0);
    step(&mut h);
    assert!(h.state().canvas.scroll_offset.y > before + 90.0, "one finger scrolls");
    finger_id(&mut h, 3, TouchPhase::End, p);
}

#[test]
fn a_pinch_ends_and_one_finger_scrolls_again() {
    let mut h = long_document(true);
    let p = page_middle(&h);
    finger_id(&mut h, 1, TouchPhase::Start, p);
    finger_id(&mut h, 2, TouchPhase::Start, p + egui::vec2(100.0, 0.0));
    for i in 1..=5 {
        finger_id(&mut h, 2, TouchPhase::Move, p + egui::vec2(100.0 + 10.0 * i as f32, 0.0));
    }
    finger_id(&mut h, 2, TouchPhase::End, p + egui::vec2(150.0, 0.0));
    finger_id(&mut h, 1, TouchPhase::End, p);
    for _ in 0..4 {
        step(&mut h);
    }
    let before = h.state().canvas.scroll_offset.y;
    drag_up(&mut h, 3, p, 100.0);
    step(&mut h);
    assert!(h.state().canvas.scroll_offset.y > before + 90.0, "back to one finger: it scrolls");
    finger_id(&mut h, 3, TouchPhase::End, p);
}

#[test]
fn a_finger_on_the_ribbon_passes_through() {
    let mut h = harness(true);
    let tab = {
        use egui_kittest::kittest::Queryable;
        h.get_by_label("Layout").rect().center()
    };
    finger(&mut h, TouchPhase::Start, tab);
    finger(&mut h, TouchPhase::End, tab);
    step(&mut h);
    assert_eq!(h.state().ui.tab, "Layout", "the ribbon tab took the tap");
}

#[test]
fn a_finger_passing_through_whose_end_was_lost_leaves_the_page_to_scroll() {
    let mut h = long_document(true);
    let p = page_middle(&h);
    // A finger on the ribbon presses egui's pointer, then its end is lost.
    finger_id(&mut h, 1, TouchPhase::Start, egui::pos2(p.x, 40.0));
    assert!(pressed(&h), "the finger passing through pressed the pointer");
    land_alone(&mut h, 9, LANDING);
    finger_id(&mut h, 9, TouchPhase::Cancel, LANDING);
    assert!(!touches(&h) && !pressed(&h), "nothing of it is left in egui");
    let before = h.state().canvas.scroll_offset.y;
    drag_up(&mut h, 2, p, 100.0);
    step(&mut h);
    assert!(h.state().canvas.scroll_offset.y > before + 90.0, "the next finger on the page scrolls");
    finger_id(&mut h, 2, TouchPhase::End, p);
}

#[test]
fn a_cancelled_long_press_opens_no_menu() {
    let mut h = harness(true);
    let p = canvas::caret_screen(h.state_mut()).unwrap().0 + egui::vec2(12.0, 6.0);
    long_press(&mut h, p);
    assert!(wordcraft_ui_egui::touch_gestures::long_pressed(&h.ctx));
    finger(&mut h, TouchPhase::Cancel, p);
    for _ in 0..4 {
        step(&mut h);
    }
    assert!(!h.state().canvas.context_menu_open);
    assert_eq!(h.state().session.selected_text().trim(), "Kilns", "the word stays selected");
    // A tap afterwards still places the caret.
    let q = end_of_text(&mut h);
    finger(&mut h, TouchPhase::Start, q);
    lift(&mut h, q);
    assert!(h.state().session.sel.is_collapsed());
    assert_eq!(caret(&h).path, wordcraft_doc::Path(vec![2]));
}

#[test]
fn a_double_tap_with_the_keyboard_down_selects_the_word() {
    let mut h = harness(true);
    let p = canvas::caret_screen(h.state_mut()).unwrap().0 + egui::vec2(40.0, 6.0);
    // The first tap lands on the web shell's input (the keyboard comes up), the second on the
    // canvas, a few frames later and a little off.
    assert!(wordcraft_ui_egui::tap_text_zone(&h.ctx, p).is_some());
    for _ in 0..3 {
        step(&mut h);
    }
    let q = p + egui::vec2(6.0, 4.0);
    finger(&mut h, TouchPhase::Start, q);
    for _ in 0..8 {
        step(&mut h);
    }
    lift(&mut h, q);
    assert_eq!(h.state().session.selected_text().trim(), "fire", "the double tap selected the word");
}

#[test]
fn a_swipe_turns_the_page_in_read_mode() {
    let mut h = long_document(true);
    h.state_mut().run("view.readMode", json!({})).unwrap();
    for _ in 0..30 {
        step(&mut h);
    }
    let p = page_middle(&h);
    let before = h.state().canvas.scroll_offset.x;
    finger(&mut h, TouchPhase::Start, p);
    for i in 1..=8 {
        finger(&mut h, TouchPhase::Move, p - egui::vec2(10.0 * i as f32, 0.0));
    }
    lift(&mut h, p - egui::vec2(80.0, 0.0));
    for _ in 0..60 {
        step(&mut h);
    }
    let after = h.state().canvas.scroll_offset.x;
    assert!(after > before + 50.0, "a swipe to the left turns to the next page: {before} -> {after}");
    // And back.
    finger(&mut h, TouchPhase::Start, p);
    for i in 1..=8 {
        finger(&mut h, TouchPhase::Move, p + egui::vec2(10.0 * i as f32, 0.0));
    }
    lift(&mut h, p + egui::vec2(80.0, 0.0));
    for _ in 0..60 {
        step(&mut h);
    }
    assert!((h.state().canvas.scroll_offset.x - before).abs() < 1.0, "and back");
    // A drag that turns into a pinch turns no page.
    finger_id(&mut h, 1, TouchPhase::Start, p);
    for i in 1..=8 {
        finger_id(&mut h, 1, TouchPhase::Move, p - egui::vec2(10.0 * i as f32, 0.0));
    }
    finger_id(&mut h, 2, TouchPhase::Start, p + egui::vec2(60.0, 40.0));
    finger_id(&mut h, 2, TouchPhase::End, p + egui::vec2(60.0, 40.0));
    finger_id(&mut h, 1, TouchPhase::End, p - egui::vec2(80.0, 0.0));
    for _ in 0..60 {
        step(&mut h);
    }
    assert!((h.state().canvas.scroll_offset.x - before).abs() < 1.0, "a pinch is no swipe");
}

/// A phone-sized window on a document of `lines` lines, fitted to the page's width as the web
/// shell starts it on a phone.
fn phone(lines: usize) -> egui_kittest::Harness<'static, WordApp> {
    let text = (1..=lines).map(|i| format!("Line {i} of the glaze notes.")).collect::<Vec<_>>().join("\n");
    let app = WordApp::new(Session::new(wordcraft_doc::Document::from_text(&text)), Services::default());
    let mut h = egui_kittest::Harness::builder().with_step_dt(1.0 / 60.0).with_size(egui::vec2(390.0, 844.0)).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    wordcraft_ui_egui::set_touch_screen(&h.ctx, true);
    for _ in 0..6 {
        step(&mut h);
    }
    h.state_mut().run("view.pageWidth", json!({})).unwrap();
    for _ in 0..6 {
        step(&mut h);
    }
    h
}

/// Two fingers land at `a` and `b` and spread apart; returns the zoom factor it made.
fn pinch(h: &mut egui_kittest::Harness<'_, WordApp>, a: Pos2, b: Pos2) -> f32 {
    let before = h.state().canvas.scale;
    let away = (b - a).normalized() * 6.0;
    finger_id(h, 11, TouchPhase::Start, a);
    finger_id(h, 12, TouchPhase::Start, b);
    for i in 1..=10 {
        let k = i as f32;
        h.input_mut().events.extend(wordcraft_ui_egui::finger_events(TouchPhase::Move, TouchId(11), a - away * k));
        h.input_mut().events.extend(wordcraft_ui_egui::finger_events(TouchPhase::Move, TouchId(12), b + away * k));
        step(h);
    }
    finger_id(h, 12, TouchPhase::End, b + away * 10.0);
    finger_id(h, 11, TouchPhase::End, a - away * 10.0);
    for _ in 0..4 {
        step(h);
    }
    h.state().canvas.scale / before
}

/// On a page, and below the last one (grey canvas), in a fitted one-page document.
fn page_and_below(h: &egui_kittest::Harness<'_, WordApp>) -> (Pos2, Pos2) {
    let page = h.state().canvas.page_rects[0];
    let view = h.state().canvas.viewport.unwrap();
    let on = egui::pos2(view.center().x, (page.top().max(view.top()) + 120.0).min(view.bottom() - 200.0));
    let below = egui::pos2(view.center().x, (page.bottom() + 40.0).min(view.bottom() - 20.0));
    (on, below)
}

#[test]
fn a_pinch_zooms_a_one_page_document_wherever_the_fingers_land() {
    for (what, order) in [("page, then below it", 0), ("below it, then the page", 1), ("both below it", 2), ("both on the page", 3)] {
        let mut h = phone(12);
        let (on, below) = page_and_below(&h);
        let page = h.state().canvas.page_rects[0];
        assert!(h.state().canvas.page_rects.len() == 1 && below.y > page.bottom() && page.contains(on), "{page:?} {on:?} {below:?}");
        let (a, b) = match order {
            0 => (on, below),
            1 => (below, on),
            2 => (below - egui::vec2(60.0, 0.0), below + egui::vec2(60.0, 0.0)),
            _ => (on - egui::vec2(0.0, 50.0), on + egui::vec2(0.0, 50.0)),
        };
        let zoom = pinch(&mut h, a, b);
        assert!(zoom > 1.2, "{what}: the pinch zoomed {zoom}");
    }
}

#[test]
fn a_pinch_zooms_a_two_page_document_too() {
    let mut h = phone(120);
    let (on, _) = page_and_below(&h);
    let zoom = pinch(&mut h, on - egui::vec2(0.0, 50.0), on + egui::vec2(0.0, 50.0));
    assert!(zoom > 1.2, "the pinch zoomed {zoom}");
}

#[test]
fn a_pinch_zooms_with_the_keyboard_up_and_down() {
    for up in [false, true] {
        let mut h = phone(12);
        wordcraft_ui_egui::set_keyboard_up(&h.ctx, up);
        step(&mut h);
        let (on, below) = page_and_below(&h);
        let zoom = pinch(&mut h, on, below);
        assert!(zoom > 1.2, "keyboard up: {up}: the pinch zoomed {zoom}");
        assert_eq!(wordcraft_ui_egui::keyboard_up(&h.ctx), up);
    }
}

#[test]
fn a_finger_on_file_is_files_own() {
    let mut h = long_document(true);
    let p = page_middle(&h);
    let offset = h.state().canvas.scroll_offset;
    h.state_mut().run("ui.tab", json!({"tab": "File"})).unwrap();
    for _ in 0..4 {
        step(&mut h);
    }
    finger_id(&mut h, 1, TouchPhase::Start, p);
    assert!(pressed(&h), "egui gets the finger as eframe reports it (File's lists drag-scroll by themselves)");
    for i in 1..=10 {
        finger_id(&mut h, 1, TouchPhase::Move, p - egui::vec2(0.0, 10.0 * i as f32));
    }
    finger_id(&mut h, 1, TouchPhase::End, p - egui::vec2(0.0, 100.0));
    h.state_mut().run("ui.tab", json!({"tab": "Home"})).unwrap();
    for _ in 0..4 {
        step(&mut h);
    }
    assert_eq!(h.state().canvas.scroll_offset, offset, "the page behind File didn't scroll");
}

#[test]
fn a_fling_stops_when_file_opens() {
    let mut h = long_document(true);
    let p = page_middle(&h);
    finger(&mut h, TouchPhase::Start, p);
    for i in 1..=6 {
        finger(&mut h, TouchPhase::Move, p - egui::vec2(0.0, 30.0 * i as f32));
    }
    finger(&mut h, TouchPhase::End, p - egui::vec2(0.0, 180.0));
    h.state_mut().run("ui.tab", json!({"tab": "File"})).unwrap();
    step(&mut h);
    step(&mut h);
    let at_file = h.state().canvas.scroll_offset;
    for _ in 0..10 {
        step(&mut h);
    }
    h.state_mut().run("ui.tab", json!({"tab": "Home"})).unwrap();
    for _ in 0..4 {
        step(&mut h);
    }
    assert!((h.state().canvas.scroll_offset - at_file).length() < 0.5, "no coast left for when the page comes back");
}

#[test]
fn a_pen_selects_as_with_a_mouse() {
    let mut h = harness(true);
    let to = end_of_text(&mut h);
    let from = canvas::caret_screen(h.state_mut()).unwrap().0 + egui::vec2(2.0, 4.0);
    // The web shell saw Safari's touchType "stylus".
    wordcraft_ui_egui::touch_gestures::stylus(&h.ctx, TouchId(4));
    finger_id(&mut h, 4, TouchPhase::Start, from);
    for i in 1..=8 {
        finger_id(&mut h, 4, TouchPhase::Move, from.lerp(to, i as f32 / 8.0));
    }
    finger_id(&mut h, 4, TouchPhase::End, to);
    step(&mut h);
    assert!(!h.state().session.sel.is_collapsed(), "the pen selected text");
}

#[test]
fn a_finger_on_the_scroll_bar_drags_it() {
    let mut h = long_document(true);
    let view = h.state().canvas.viewport.unwrap();
    let bar = egui::pos2(view.right() - 4.0, view.top() + 60.0);
    finger_id(&mut h, 1, TouchPhase::Start, bar);
    assert!(pressed(&h), "the finger reaches the scroll bar as eframe reports it");
    finger_id(&mut h, 1, TouchPhase::Cancel, bar);
}

#[test]
fn a_tap_by_the_edges_of_the_view_places_a_caret() {
    let mut h = long_document(true);
    let view = h.state().canvas.viewport.unwrap();
    let (text, _) = zones(&mut h);
    // The vertical scroll bar's strip is the bar's: the web shell's input leaves it.
    let bar = egui::pos2(view.right() - 4.0, view.center().y);
    assert!(!text.iter().any(|z| z.contains(bar)), "the input leaves the scroll bar");
    // No horizontal bar: the bottom edge is the page's, and its tap is the input's.
    let bottom = egui::pos2(view.center().x, view.bottom() - 4.0);
    assert!(text.iter().any(|z| z.contains(bottom)), "the input covers the page down to the bottom");
    finger(&mut h, TouchPhase::Start, bottom);
    finger(&mut h, TouchPhase::Cancel, bottom);
    assert!(wordcraft_ui_egui::tap_text_zone(&h.ctx, bottom).is_some());
    for _ in 0..4 {
        step(&mut h);
    }
    assert!(h.state().session.sel.is_collapsed(), "one click: a caret, not a word");
}

#[test]
fn a_finger_passing_through_lifted_in_the_frame_a_page_finger_lands_releases_the_pointer() {
    let mut h = long_document(true);
    let p = page_middle(&h);
    let ribbon = egui::pos2(p.x, 40.0);
    finger_id(&mut h, 1, TouchPhase::Start, ribbon);
    assert!(pressed(&h));
    h.input_mut().events.extend(wordcraft_ui_egui::finger_events(TouchPhase::End, TouchId(1), ribbon));
    h.input_mut().events.extend(wordcraft_ui_egui::finger_events(TouchPhase::Start, TouchId(2), p));
    step(&mut h);
    assert!(!pressed(&h), "the ribbon finger's release reached egui");
    finger_id(&mut h, 2, TouchPhase::Cancel, p);
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
