//! Typing on touch screens (`wordcraft_ui_egui::touch`; ported from GridCraft's web shell).
//!
//! iOS opens the on-screen keyboard only for a text input the user taps, and eframe focuses its
//! hidden text input from script, a frame after a widget asks for text input. So on touch screens
//! eframe's (transparent) text input lies over the visible pages and the text fields (the
//! [`wordcraft_ui_egui::text_zones`]): a tap there lands on it, the browser focuses it natively and
//! the keyboard comes up; once it has the focus the shell reports the tap
//! ([`wordcraft_ui_egui::tap_text_zone`]), and the next frames place the caret there or focus the
//! field. Drags, long presses and pinches that begin on the input are forwarded to egui as eframe
//! sends them for the canvas ([`forward_gestures`]); a mouse, trackpad or pen makes the input step
//! aside until a finger touches again. While the keyboard is up the input shrinks
//! back to eframe's 1px field at the text cursor, and iOS's pan to keep it in view is undone once
//! the keyboard is gone ([`unpan_after_keyboard`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

/// Events for egui that eframe didn't see, handed over in `raw_input_hook`.
pub type Gestures = Rc<Cell<Vec<egui::Event>>>;

/// On a touch screen (`pointer: coarse`), lays the text input over the text zones and listens for
/// taps and gestures on it. Returns the forwarded events for the shell to hand egui. Off
/// elsewhere: desktops (also with a touchscreen) keep eframe's own handling.
pub fn install(ctx: &egui::Context, canvas_id: &'static str) -> Gestures {
    let gestures = Gestures::default();
    let Some(window) = web_sys::window() else { return gestures };
    if !window.match_media("(pointer: coarse)").ok().flatten().is_some_and(|m| m.matches()) {
        return gestures;
    }
    wordcraft_ui_egui::set_touch_screen(ctx, true);
    // Without `path()` clips (Android WebView before Chrome 88) the input would cover everything
    // between the zones: then it lies over the pages only.
    let clips = web_sys::css::supports_with_value("clip-path", "path('M0 0')").unwrap_or(false);
    ctx.on_end_pass("wordcraft.text_input", std::sync::Arc::new(move |ui: &mut egui::Ui| place_text_input(ui.ctx(), canvas_id, clips)));
    // First: a drag or long press it claims (its `touchend` prevented) is no tap for the other.
    forward_gestures(ctx, canvas_id, &gestures);
    keyboard_on_tap(ctx, canvas_id);
    unpan_after_keyboard();
    gestures
}

/// A tap on the text input over the text zones: once the browser has focused it (after
/// `touchend`, where the tap lands then: acting at once would move the input away from under it
/// and the keyboard would stay down), the tap places the caret or focuses the field. The keyboard
/// counts as gone once the input loses the focus for good.
fn keyboard_on_tap(ctx: &egui::Context, canvas_id: &'static str) {
    let Some(window) = web_sys::window() else { return };
    let Some(document) = window.document() else { return };
    // Where and when the touch began, and whether a second finger joined it.
    let start = Rc::new(Cell::new((0, 0, 0.0, false)));
    let start_set = start.clone();
    // Where the input was tapped (in points), waiting for the browser to focus it.
    let tapped = Rc::new(Cell::new(None));
    let tapped_start = tapped.clone();
    let tapped_end = tapped.clone();
    let on_start = Closure::<dyn FnMut(web_sys::TouchEvent)>::new(move |e: web_sys::TouchEvent| {
        tapped_start.set(None);
        if let Some(t) = e.changed_touches().get(0) {
            start_set.set((t.client_x(), t.client_y(), e.time_stamp(), e.touches().length() > 1));
        }
    });
    let end_ctx = ctx.clone();
    // Held longer, it's a long press (`forward_gestures`), which prevents its `touchend`.
    let hold_ms = hold_ms(ctx);
    let on_end = Closure::<dyn FnMut(web_sys::TouchEvent)>::new(move |e: web_sys::TouchEvent| {
        // One finger, lifted soon and near where it went down: a tap, not a drag, a pinch or a
        // long press (which `forward_gestures` took).
        let Some(t) = e.changed_touches().get(0).filter(|_| e.touches().length() == 0 && !e.default_prevented()) else { return };
        let (x, y, at, multi) = start.get();
        if multi || e.time_stamp() - at > hold_ms || (t.client_x() - x).abs() > TAP_SLOP || (t.client_y() - y).abs() > TAP_SLOP {
            return;
        }
        let Some((canvas, input)) = canvas_and_input(canvas_id) else { return };
        let p = canvas_point(&end_ctx, &canvas, t.client_x(), t.client_y());
        // A tap on a page that reached the canvas (the first after a mouse or pen, which the input
        // stood aside for): focus the input from this handler, which opens the keyboard there;
        // eframe's own listeners place the caret.
        if e.target().as_ref() == Some(canvas.as_ref()) && wordcraft_ui_egui::tap_opens_keyboard(&end_ctx, p) {
            let options = web_sys::FocusOptions::new();
            options.set_prevent_scroll(true);
            let _ = input.focus_with_options(&options);
            wordcraft_ui_egui::set_keyboard_up(&end_ctx, true);
            return;
        }
        if e.target().as_ref() != Some(input.as_ref()) {
            return;
        }
        // Already focused (from script, with no keyboard): no focus event follows.
        if is_active(&input) {
            wordcraft_ui_egui::tap_text_zone(&end_ctx, p);
        } else {
            tapped_end.set(Some(p));
        }
    });
    let focus_ctx = ctx.clone();
    let on_focus = Closure::<dyn FnMut(web_sys::FocusEvent)>::new(move |e: web_sys::FocusEvent| {
        let Some((canvas, input)) = canvas_and_input(canvas_id).filter(|(_, input)| e.target().as_ref() == Some(input.as_ref())) else { return };
        // Focused now: the input can shrink to the zone tapped (iOS keeps a focused input in view).
        if let Some(zone) = tapped.take().and_then(|p| wordcraft_ui_egui::tap_text_zone(&focus_ctx, p)) {
            cover(&focus_ctx, &canvas, &input, &[zone]);
        }
    });
    // eframe blurs and refocuses its input within its own `touchend` (its iOS fix): only a focus
    // still lost once the handler is done means the keyboard went down.
    let blur_ctx = ctx.clone();
    let check = Closure::<dyn FnMut()>::new(move || {
        if canvas_and_input(canvas_id).is_some_and(|(_, input)| !is_active(&input)) && wordcraft_ui_egui::keyboard_up(&blur_ctx) {
            wordcraft_ui_egui::set_keyboard_up(&blur_ctx, false);
        }
    });
    let check_fn: js_sys::Function = check.as_ref().unchecked_ref::<js_sys::Function>().clone();
    let on_blur = Closure::<dyn FnMut(web_sys::FocusEvent)>::new(move |e: web_sys::FocusEvent| {
        if canvas_and_input(canvas_id).is_some_and(|(_, input)| e.target().as_ref() == Some(input.as_ref()))
            && let Some(w) = web_sys::window()
        {
            w.queue_microtask(&check_fn);
        }
    });
    // Capturing: eframe stops touches on the canvas from bubbling.
    for (name, f) in
        [("touchstart", on_start.as_ref()), ("touchend", on_end.as_ref()), ("focusin", on_focus.as_ref()), ("focusout", on_blur.as_ref())]
    {
        let _ = document.add_event_listener_with_callback_and_bool(name, f.unchecked_ref(), true);
    }
    // Listening for the page's lifetime.
    on_start.forget();
    on_end.forget();
    on_focus.forget();
    on_blur.forget();
    check.forget();
}

/// Touches that began on the text input, followed here (eframe's canvas listeners never see them).
struct Forwarded {
    /// Their ids, the first finger's first.
    ids: Vec<i32>,
    /// Where the first finger landed, in CSS pixels.
    x: i32,
    y: i32,
    /// Still maybe a tap, which stays the input's own: focusing it natively opens the keyboard.
    tap: bool,
    /// egui has its touch: from the start on a page (whose fingers egui follows); on a text field
    /// only once it's no tap (a drag, as eframe would report it).
    sent: bool,
    /// The timeout after which it's no tap but a long press.
    timer: i32,
}

/// How far (CSS pixels) a tap may move: farther, it's a drag.
const TAP_SLOP: i32 = 10;

/// Touches that begin on the text input go to egui as eframe sends a finger on the canvas
/// (`wordcraft_ui_egui::finger_events`): on a page from the start, where `touch_gestures` makes
/// them a scroll, a long press or a pinch; on a text field once they're no tap. A tap stays the
/// input's own (the browser focuses it, which opens the keyboard, and the tap clicks the page or
/// focuses the field from there): egui gets the touch cancelled, or none. For anything but a tap the
/// browser's handling (the focus, text selection, the text menu) is prevented. eframe's own
/// `pointerdown` for a finger on the canvas is held back: it comes before the touch, which tells
/// egui it's a finger. A mouse or trackpad (an iPad's) presses egui directly.
fn forward_gestures(ctx: &egui::Context, canvas_id: &'static str, events: &Gestures) {
    let Some(window) = web_sys::window() else { return };
    let Some(document) = window.document() else { return };
    let touch: Rc<RefCell<Option<Forwarded>>> = Rc::default();
    let send = {
        let (events, ctx) = (events.clone(), ctx.clone());
        Rc::new(move |more: Vec<egui::Event>| {
            let mut all = events.take();
            all.extend(more);
            events.set(all);
            ctx.request_repaint();
        })
    };
    // Points, from CSS pixels.
    let at = {
        let ctx = ctx.clone();
        Rc::new(move |x: i32, y: i32| match canvas_and_input(canvas_id) {
            Some((canvas, _)) => canvas_point(&ctx, &canvas, x, y),
            None => egui::pos2(x as f32, y as f32),
        })
    };
    let on_input = move |e: &web_sys::Event| canvas_and_input(canvas_id).is_some_and(|(_, input)| e.target().as_ref() == Some(input.as_ref()));
    let on_canvas = move |e: &web_sys::Event| canvas_and_input(canvas_id).is_some_and(|(canvas, _)| e.target().as_ref() == Some(canvas.as_ref()));
    let changed = |e: &web_sys::TouchEvent| (0..e.changed_touches().length()).filter_map(|i| e.changed_touches().get(i)).collect::<Vec<_>>();
    // A finger's events for egui.
    let finger = {
        let at = at.clone();
        Rc::new(move |phase: egui::TouchPhase, t: &web_sys::Touch| {
            wordcraft_ui_egui::finger_events(phase, t.identifier().into(), at(t.client_x(), t.client_y()))
        })
    };

    // Held still: a long press (egui sees it by the time), no tap.
    let on_hold = Closure::<dyn FnMut()>::new({
        let touch = touch.clone();
        move || {
            if let Some(f) = touch.borrow_mut().as_mut() {
                f.tap = false;
            }
        }
    });
    let hold_ms = hold_ms(ctx) as i32;
    let on_start = Closure::<dyn FnMut(web_sys::TouchEvent)>::new({
        let (touch, window, send, at, finger, ctx) = (touch.clone(), window.clone(), send.clone(), at.clone(), finger.clone(), ctx.clone());
        let hold: js_sys::Function = on_hold.as_ref().unchecked_ref::<js_sys::Function>().clone();
        move |e: web_sys::TouchEvent| {
            let mut followed = touch.borrow_mut();
            // The only finger down: touches still followed lost their end (to another element, the
            // system), and are done.
            if e.touches().length() == 1
                && let Some(f) = followed.take()
            {
                window.clear_timeout_with_handle(f.timer);
                for id in f.ids {
                    send(wordcraft_ui_egui::finger_events(egui::TouchPhase::Cancel, id.into(), at(f.x, f.y)));
                }
            }
            // Another finger joins ours: no tap (a pinch).
            if let Some(f) = followed.as_mut() {
                window.clear_timeout_with_handle(f.timer);
                f.tap = false;
                if !f.sent {
                    f.sent = true;
                    send(wordcraft_ui_egui::finger_events(egui::TouchPhase::Start, f.ids.first().copied().unwrap_or(0).into(), at(f.x, f.y)));
                }
                for t in changed(&e) {
                    f.ids.push(t.identifier());
                    send(finger(egui::TouchPhase::Start, &t));
                }
                e.prevent_default();
                e.stop_propagation();
                return;
            }
            if !on_input(&e) {
                return;
            }
            // Another finger on the input while egui follows one that began elsewhere (a pinch from
            // the ribbon or between the pages): egui gets its touch; eframe's own listeners on the
            // document carry on with its moves and its end.
            if e.touches().length() > 1 {
                for t in changed(&e) {
                    send(vec![egui::Event::Touch {
                        device_id: egui::TouchDeviceId(0),
                        id: t.identifier().into(),
                        phase: egui::TouchPhase::Start,
                        pos: at(t.client_x(), t.client_y()),
                        force: None,
                    }]);
                }
                e.prevent_default();
                return;
            }
            if let Some(t) = e.changed_touches().get(0) {
                let page = wordcraft_ui_egui::tap_opens_keyboard(&ctx, at(t.client_x(), t.client_y()));
                if page {
                    send(finger(egui::TouchPhase::Start, &t));
                }
                let timer = window.set_timeout_with_callback_and_timeout_and_arguments_0(&hold, hold_ms).unwrap_or(0);
                *followed = Some(Forwarded { ids: vec![t.identifier()], x: t.client_x(), y: t.client_y(), tap: true, sent: page, timer });
            }
        }
    });
    let on_move = Closure::<dyn FnMut(web_sys::TouchEvent)>::new({
        let (touch, window, send, finger, at) = (touch.clone(), window.clone(), send.clone(), finger.clone(), at.clone());
        move |e: web_sys::TouchEvent| {
            let mut followed = touch.borrow_mut();
            let Some(f) = followed.as_mut() else { return };
            let ours: Vec<web_sys::Touch> = changed(&e).into_iter().filter(|t| f.ids.contains(&t.identifier())).collect();
            for t in ours {
                if f.tap && ((t.client_x() - f.x).abs() > TAP_SLOP || (t.client_y() - f.y).abs() > TAP_SLOP) {
                    window.clear_timeout_with_handle(f.timer);
                    f.tap = false;
                }
                // A drag from a text field: egui gets it from where it began.
                if !f.sent && !f.tap {
                    f.sent = true;
                    send(wordcraft_ui_egui::finger_events(egui::TouchPhase::Start, t.identifier().into(), at(f.x, f.y)));
                }
                if f.sent {
                    send(finger(egui::TouchPhase::Move, &t));
                }
            }
            if !f.tap {
                e.prevent_default();
            }
            e.stop_propagation();
        }
    });
    let on_end = Closure::<dyn FnMut(web_sys::TouchEvent)>::new({
        let (touch, window, send, finger) = (touch.clone(), window.clone(), send.clone(), finger.clone());
        move |e: web_sys::TouchEvent| {
            let mut followed = touch.borrow_mut();
            let Some(f) = followed.as_mut() else { return };
            let ended = if e.type_() == "touchcancel" { egui::TouchPhase::Cancel } else { egui::TouchPhase::End };
            let tap = f.tap && ended == egui::TouchPhase::End;
            let ours: Vec<web_sys::Touch> = changed(&e).into_iter().filter(|t| f.ids.contains(&t.identifier())).collect();
            for t in ours {
                f.ids.retain(|id| *id != t.identifier());
                // A tap is the input's: egui forgets the touch, the focus clicks the page.
                if f.sent {
                    send(finger(if tap { egui::TouchPhase::Cancel } else { ended }, &t));
                }
            }
            if !tap {
                // No focus (and keyboard) or text menu after it.
                e.prevent_default();
            }
            e.stop_propagation();
            if f.ids.is_empty() {
                window.clear_timeout_with_handle(f.timer);
                *followed = None;
            }
        }
    });
    // Android's own long press (a `contextmenu`, then the text field's popup): no tap, no popup.
    let on_menu = Closure::<dyn FnMut(web_sys::Event)>::new({
        let touch = touch.clone();
        move |e: web_sys::Event| {
            let mut followed = touch.borrow_mut();
            let Some(f) = followed.as_mut().filter(|_| on_input(&e)) else { return };
            f.tap = false;
            e.prevent_default();
            e.stop_propagation();
        }
    });
    // A mouse, trackpad or pen in use (an iPad's, a tablet's) goes straight to the canvas: the
    // input takes no pointer events (wheel, hover, pinch, pen strokes reach eframe) until a finger
    // touches the screen again. That first touch lands on the canvas (a tap on a page still opens
    // the keyboard: `keyboard_on_tap`).
    let pointer_mode = Rc::new(Cell::new(false));
    let on_pointer = Closure::<dyn FnMut(web_sys::PointerEvent)>::new(move |e: web_sys::PointerEvent| {
        let finger = e.pointer_type() == "touch";
        if finger == pointer_mode.get()
            && let Some((_, input)) = canvas_and_input(canvas_id)
        {
            let style = input.style();
            if finger {
                let _ = style.remove_property("pointer-events");
            } else {
                let _ = style.set_property("pointer-events", "none");
            }
            pointer_mode.set(!finger);
        }
    });
    // A mouse button pressed on the input before any move took it out of the way (which the
    // browser would focus, or select text in): egui gets the press, and the release from here, as
    // eframe can't tell it's its own.
    let mouse: Rc<Cell<Option<egui::PointerButton>>> = Rc::default();
    let on_down = Closure::<dyn FnMut(web_sys::PointerEvent)>::new({
        let (mouse, send, at) = (mouse.clone(), send.clone(), at.clone());
        move |e: web_sys::PointerEvent| {
            // A finger on the canvas: its touch events follow, and tell egui it's a finger.
            if e.pointer_type() == "touch" && on_canvas(&e) {
                e.stop_propagation();
            }
            if e.pointer_type() != "mouse" || !on_input(&e) {
                return;
            }
            let Some(button) = mouse_button(e.button()) else { return };
            let pos = at(e.client_x(), e.client_y());
            // ponytail: Shift only (extends the selection); other modifiers when a tablet with a keyboard needs them.
            let modifiers = egui::Modifiers { shift: e.shift_key(), ..Default::default() };
            mouse.set(Some(button));
            send(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button, pressed: true, modifiers }]);
            e.prevent_default();
            e.stop_propagation();
        }
    });
    // Pointer events come first: eframe's `pointerup` on the document would release a forwarded
    // touch ahead of its `touchend`. The `touchend` alone releases it.
    let on_up = Closure::<dyn FnMut(web_sys::PointerEvent)>::new({
        let (touch, send, at) = (touch.clone(), send.clone(), at.clone());
        move |e: web_sys::PointerEvent| {
            if e.pointer_type() == "touch" {
                if touch.borrow().is_some() {
                    e.stop_propagation();
                }
                return;
            }
            if let Some(button) = mouse.take() {
                let pos = at(e.client_x(), e.client_y());
                send(vec![egui::Event::PointerButton { pos, button, pressed: false, modifiers: egui::Modifiers::NONE }]);
                e.stop_propagation();
            }
        }
    });
    // Capturing, ahead of eframe's listeners on the document, and not passive, so the browser's
    // handling can be prevented.
    let options = web_sys::AddEventListenerOptions::new();
    options.set_capture(true);
    options.set_passive(false);
    for (name, f) in [("touchstart", &on_start), ("touchmove", &on_move), ("touchend", &on_end), ("touchcancel", &on_end)] {
        let _ = document.add_event_listener_with_callback_and_add_event_listener_options(name, f.as_ref().unchecked_ref(), &options);
    }
    let _ = document.add_event_listener_with_callback_and_add_event_listener_options("contextmenu", on_menu.as_ref().unchecked_ref(), &options);
    let _ = document.add_event_listener_with_callback_and_add_event_listener_options("pointerdown", on_down.as_ref().unchecked_ref(), &options);
    let _ = document.add_event_listener_with_callback_and_add_event_listener_options("pointerup", on_up.as_ref().unchecked_ref(), &options);
    for name in ["pointerdown", "pointermove"] {
        let _ = document.add_event_listener_with_callback_and_add_event_listener_options(name, on_pointer.as_ref().unchecked_ref(), &options);
    }
    // Listening for the page's lifetime.
    on_hold.forget();
    on_start.forget();
    on_move.forget();
    on_end.forget();
    on_menu.forget();
    on_down.forget();
    on_up.forget();
    on_pointer.forget();
}

/// How long (ms) a touch held still takes to be a long press: as long as egui waits for one.
fn hold_ms(ctx: &egui::Context) -> f64 {
    ctx.options(|o| o.input_options.max_click_duration * 1000.0).clamp(0.0, 10_000.0)
}

fn mouse_button(button: i16) -> Option<egui::PointerButton> {
    match button {
        0 => Some(egui::PointerButton::Primary),
        1 => Some(egui::PointerButton::Middle),
        2 => Some(egui::PointerButton::Secondary),
        _ => None,
    }
}

/// iOS pans the page to keep a natively focused text input above the keyboard, and doesn't always
/// pan back once the keyboard hides: the canvas stays pushed up with the page background below it.
/// The page itself never scrolls, so any pan left once no text input has focus is undone (not on
/// `focusout`: eframe's own iOS fix blurs and refocuses the input on every tap while typing).
fn unpan_after_keyboard() {
    let Some(window) = web_sys::window() else { return };
    let unpan = Closure::<dyn FnMut()>::new(|| {
        let Some(w) = web_sys::window() else { return };
        let typing = w.document().and_then(|d| d.active_element()).is_some_and(|el| el.dyn_ref::<web_sys::HtmlInputElement>().is_some());
        if !typing && (w.scroll_x().unwrap_or(0.0) != 0.0 || w.scroll_y().unwrap_or(0.0) != 0.0) {
            w.scroll_to_with_x_and_y(0.0, 0.0);
        }
    });
    let f = unpan.as_ref().unchecked_ref();
    let _ = window.add_event_listener_with_callback("scroll", f);
    // The keyboard hiding resizes the visual viewport, also when the pan itself fired no scroll event.
    if let Some(vv) = window.visual_viewport() {
        let _ = vv.add_event_listener_with_callback("resize", f);
        let _ = vv.add_event_listener_with_callback("scroll", f);
    }
    // Listening for the page's lifetime.
    unpan.forget();
}

/// The canvas, and eframe's text input (which eframe 0.36 puts right after it).
fn canvas_and_input(canvas_id: &str) -> Option<(web_sys::HtmlCanvasElement, web_sys::HtmlInputElement)> {
    let canvas = web_sys::window()?.document()?.get_element_by_id(canvas_id)?.dyn_into::<web_sys::HtmlCanvasElement>().ok()?;
    let input = canvas.next_element_sibling()?.dyn_into::<web_sys::HtmlInputElement>().ok()?;
    Some((canvas, input))
}

fn is_active(input: &web_sys::HtmlInputElement) -> bool {
    web_sys::window().and_then(|w| w.document()).and_then(|d| d.active_element()).as_ref() == Some(input.as_ref())
}

/// A page position (CSS px) on the canvas, in points.
fn canvas_point(ctx: &egui::Context, canvas: &web_sys::HtmlCanvasElement, x: i32, y: i32) -> egui::Pos2 {
    let r = canvas.get_bounding_client_rect();
    let zoom = ctx.zoom_factor();
    egui::pos2((f64::from(x) - r.left()) as f32 / zoom, (f64::from(y) - r.top()) as f32 / zoom)
}

/// Lays eframe's text input over the text zones (over the pages only where the browser can't clip
/// it to them). After eframe's own placement, which follows in the same frame and moves the input
/// to the text cursor whenever that changes.
fn place_text_input(ctx: &egui::Context, canvas_id: &'static str, clips: bool) {
    let zones = if clips { wordcraft_ui_egui::text_zones(ctx) } else { wordcraft_ui_egui::page_zones(ctx) };
    let ctx = ctx.clone();
    let place = Closure::once_into_js(move || {
        if let Some((canvas, input)) = canvas_and_input(canvas_id) {
            cover(&ctx, &canvas, &input, &zones);
        }
    });
    if let Some(w) = web_sys::window() {
        w.queue_microtask(place.unchecked_ref());
    }
}

/// Lays the input over `zones` (points), clipped to them so that a tap between them still reaches
/// the canvas; with none, shrinks it back to the 1px field eframe moves to the text cursor.
fn cover(ctx: &egui::Context, canvas: &web_sys::HtmlCanvasElement, input: &web_sys::HtmlInputElement, zones: &[egui::Rect]) {
    let style = input.style();
    let Some(all) = zones.iter().copied().reduce(egui::Rect::union) else {
        let _ = style.set_property("width", "1px");
        let _ = style.set_property("height", "1px");
        let _ = style.remove_property("clip-path");
        return;
    };
    // No iOS callout on a long press (not `user-select: none`: iOS then types nothing into the
    // input), and no text of its own showing over the page.
    let _ = style.set_property("-webkit-touch-callout", "none");
    let _ = style.set_property("color", "transparent");
    let zoom = ctx.zoom_factor();
    let _ = style.set_property("left", &format!("{}px", canvas.offset_left() as f32 + all.left() * zoom));
    let _ = style.set_property("top", &format!("{}px", canvas.offset_top() as f32 + all.top() * zoom));
    let _ = style.set_property("width", &format!("{}px", all.width() * zoom));
    let _ = style.set_property("height", &format!("{}px", all.height() * zoom));
    let path: String = zones
        .iter()
        .map(|z| {
            let (x, y, w, h) = ((z.left() - all.left()) * zoom, (z.top() - all.top()) * zoom, z.width() * zoom, z.height() * zoom);
            format!("M{x} {y}h{w}v{h}h{}z", -w)
        })
        .collect();
    let _ = style.set_property("clip-path", &format!("path('{path}')"));
}
