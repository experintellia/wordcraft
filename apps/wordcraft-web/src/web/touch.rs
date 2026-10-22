//! Typing on touch screens (`wordcraft_ui_egui::touch`; ported from GridCraft's web shell).
//!
//! iOS opens the on-screen keyboard only for a text input the user taps, and eframe focuses its
//! hidden text input from script, a frame after a widget asks for text input. So on touch screens
//! eframe's (transparent) text input lies over the visible pages and the text fields (the
//! [`wordcraft_ui_egui::text_zones`]): a tap there lands on it, the browser focuses it natively and
//! the keyboard comes up; once it has the focus the shell reports the tap
//! ([`wordcraft_ui_egui::tap_text_zone`]), and the next frames place the caret there or focus the
//! field. Drags, long presses and pinches that begin on the input are forwarded to egui as eframe
//! sends them for the canvas ([`forward_gestures`]). While the keyboard is up the input shrinks
//! back to eframe's 1px field at the text cursor, and iOS's pan to keep it in view is undone once
//! the keyboard is gone ([`unpan_after_keyboard`]).

use std::cell::Cell;
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
    let on_end = Closure::<dyn FnMut(web_sys::TouchEvent)>::new(move |e: web_sys::TouchEvent| {
        // One finger, lifted soon and near where it went down: a tap, not a drag, a pinch or a
        // long press (which `forward_gestures` took).
        let Some(t) = e.changed_touches().get(0).filter(|_| e.touches().length() == 0 && !e.default_prevented()) else { return };
        let (x, y, at, multi) = start.get();
        if multi || e.time_stamp() - at > 500.0 || (t.client_x() - x).abs() > TAP_SLOP || (t.client_y() - y).abs() > TAP_SLOP {
            return;
        }
        let Some((canvas, input)) = canvas_and_input(canvas_id) else { return };
        if e.target().as_ref() != Some(input.as_ref()) {
            return;
        }
        let p = canvas_point(&end_ctx, &canvas, t.client_x(), t.client_y());
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

/// What a touch that began on the text input has turned into so far.
#[derive(Clone, Copy, PartialEq)]
enum Gesture {
    /// Maybe a tap, which stays the input's own: focusing it natively is what opens the keyboard.
    Tap,
    Drag,
    LongPress,
    /// Two fingers or more: egui sees the touches only (a pinch zooms).
    Pinch,
}

#[derive(Clone, Copy)]
struct InputTouch {
    id: i32,
    /// Where it began (a drag: where it is now), in CSS pixels.
    x: i32,
    y: i32,
    gesture: Gesture,
    /// The timeout that makes it a long press.
    timer: i32,
}

/// How far (CSS pixels) a tap may move: farther, it's a drag.
const TAP_SLOP: i32 = 10;

/// Touches that begin on the text input go to the input, not to eframe's canvas listeners. Once one
/// turns into a drag, a long press or a pinch it belongs to egui: its events go there as eframe
/// sends a finger on the canvas (a drag selects, as on the canvas), a long press as a held
/// secondary press (lifted, the page opens its context menu), a pinch as touches; the browser's
/// own handling (text selection, the text menu, the focus) is prevented. A mouse or trackpad (an
/// iPad's) presses egui directly, without focusing the input.
fn forward_gestures(ctx: &egui::Context, canvas_id: &'static str, events: &Gestures) {
    let Some(window) = web_sys::window() else { return };
    let Some(document) = window.document() else { return };
    let touch: Rc<Cell<Option<InputTouch>>> = Rc::default();
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
    // This touch, among the event's changed ones.
    let find = |e: &web_sys::TouchEvent, id: i32| {
        (0..e.changed_touches().length()).filter_map(|i| e.changed_touches().get(i)).find(|t| t.identifier() == id)
    };
    // Every finger of the event's `list`, as egui touches.
    let touches = {
        let at = at.clone();
        Rc::new(move |list: web_sys::TouchList, phase: egui::TouchPhase| {
            (0..list.length())
                .filter_map(|i| list.get(i))
                .map(|t| egui::Event::Touch {
                    device_id: egui::TouchDeviceId(0),
                    id: t.identifier().into(),
                    phase,
                    pos: at(t.client_x(), t.client_y()),
                    force: None,
                })
                .collect::<Vec<_>>()
        })
    };

    // A waiting tap held still turns into a long press.
    let long_press = {
        let (touch, send, at) = (touch.clone(), send.clone(), at.clone());
        Rc::new(move || {
            let Some(t) = touch.get().filter(|t| t.gesture == Gesture::Tap) else { return };
            touch.set(Some(InputTouch { gesture: Gesture::LongPress, ..t }));
            send(wordcraft_ui_egui::long_press_events(egui::TouchPhase::Start, at(t.x, t.y)));
        })
    };
    // Held still as long as egui waits for a long press on the canvas.
    let on_hold = Closure::<dyn FnMut()>::new({
        let long_press = long_press.clone();
        move || long_press()
    });
    let hold_ms = ctx.options(|o| o.input_options.max_click_duration * 1000.0).clamp(0.0, 10_000.0) as i32;
    let on_start = Closure::<dyn FnMut(web_sys::TouchEvent)>::new({
        let (touch, window, send, at, touches) = (touch.clone(), window.clone(), send.clone(), at.clone(), touches.clone());
        let hold: js_sys::Function = on_hold.as_ref().unchecked_ref::<js_sys::Function>().clone();
        move |e: web_sys::TouchEvent| {
            // The only finger down: a touch still followed lost its end (to another element, the
            // system), and is done.
            if e.touches().length() == 1
                && let Some(t) = touch.take()
            {
                window.clear_timeout_with_handle(t.timer);
                let pos = at(t.x, t.y);
                match t.gesture {
                    Gesture::Drag => send(wordcraft_ui_egui::finger_events(egui::TouchPhase::Cancel, t.id.into(), pos)),
                    Gesture::LongPress => send(wordcraft_ui_egui::long_press_events(egui::TouchPhase::Cancel, pos)),
                    Gesture::Tap | Gesture::Pinch => {}
                }
            }
            if let Some(t) = touch.get() {
                // Another finger makes a waiting tap or a drag a pinch (a long press keeps on).
                if matches!(t.gesture, Gesture::Tap | Gesture::Drag) {
                    window.clear_timeout_with_handle(t.timer);
                    if t.gesture == Gesture::Drag {
                        send(wordcraft_ui_egui::finger_events(egui::TouchPhase::Cancel, t.id.into(), at(t.x, t.y)));
                    }
                    send(touches(e.touches(), egui::TouchPhase::Start));
                    touch.set(Some(InputTouch { gesture: Gesture::Pinch, ..t }));
                } else if t.gesture == Gesture::Pinch {
                    send(touches(e.changed_touches(), egui::TouchPhase::Start));
                }
                e.prevent_default();
                e.stop_propagation();
                return;
            }
            if let Some(t) = e.changed_touches().get(0).filter(|_| on_input(&e) && e.touches().length() == 1) {
                let timer = window.set_timeout_with_callback_and_timeout_and_arguments_0(&hold, hold_ms).unwrap_or(0);
                touch.set(Some(InputTouch { id: t.identifier(), x: t.client_x(), y: t.client_y(), gesture: Gesture::Tap, timer }));
            }
        }
    });
    let on_move = Closure::<dyn FnMut(web_sys::TouchEvent)>::new({
        let (touch, window, send, at, touches) = (touch.clone(), window.clone(), send.clone(), at.clone(), touches.clone());
        move |e: web_sys::TouchEvent| {
            let Some(mut t) = touch.get() else { return };
            if t.gesture == Gesture::Pinch {
                send(touches(e.changed_touches(), egui::TouchPhase::Move));
                e.prevent_default();
                e.stop_propagation();
                return;
            }
            let Some(now) = find(&e, t.id) else { return };
            if t.gesture == Gesture::Tap {
                if (now.client_x() - t.x).abs() <= TAP_SLOP && (now.client_y() - t.y).abs() <= TAP_SLOP {
                    return;
                }
                window.clear_timeout_with_handle(t.timer);
                t.gesture = Gesture::Drag;
                send(wordcraft_ui_egui::finger_events(egui::TouchPhase::Start, t.id.into(), at(t.x, t.y)));
            }
            let pos = at(now.client_x(), now.client_y());
            match t.gesture {
                Gesture::Drag => send(wordcraft_ui_egui::finger_events(egui::TouchPhase::Move, t.id.into(), pos)),
                Gesture::LongPress => send(wordcraft_ui_egui::long_press_events(egui::TouchPhase::Move, pos)),
                Gesture::Tap | Gesture::Pinch => {}
            }
            touch.set(Some(InputTouch { x: now.client_x(), y: now.client_y(), ..t }));
            e.prevent_default();
            e.stop_propagation();
        }
    });
    let on_end = Closure::<dyn FnMut(web_sys::TouchEvent)>::new({
        let (touch, window, send, at, touches) = (touch.clone(), window.clone(), send.clone(), at.clone(), touches.clone());
        move |e: web_sys::TouchEvent| {
            let Some(t) = touch.get() else { return };
            let cancel = e.type_() == "touchcancel";
            if t.gesture == Gesture::Pinch {
                send(touches(e.changed_touches(), if cancel { egui::TouchPhase::Cancel } else { egui::TouchPhase::End }));
                if e.touches().length() == 0 {
                    touch.set(None);
                }
                e.prevent_default();
                e.stop_propagation();
                return;
            }
            let Some(now) = find(&e, t.id) else { return };
            touch.set(None);
            let pos = at(now.client_x(), now.client_y());
            match t.gesture {
                Gesture::Tap => {
                    window.clear_timeout_with_handle(t.timer);
                    return;
                }
                Gesture::Drag => {
                    let phase = if cancel { egui::TouchPhase::Cancel } else { egui::TouchPhase::End };
                    send(wordcraft_ui_egui::finger_events(phase, t.id.into(), pos));
                }
                Gesture::LongPress => send(wordcraft_ui_egui::long_press_events(egui::TouchPhase::End, pos)),
                Gesture::Pinch => {}
            }
            // No focus (and keyboard) or text menu after it, and eframe's own listeners on the
            // document skip it.
            e.prevent_default();
            e.stop_propagation();
        }
    });
    // Android's own long press comes sooner than egui's (a `contextmenu`, then the text field's
    // popup): it makes a waiting tap a long press at once, without the popup.
    let on_menu = Closure::<dyn FnMut(web_sys::Event)>::new({
        let (touch, window) = (touch.clone(), window.clone());
        move |e: web_sys::Event| {
            let Some(t) = touch.get().filter(|_| on_input(&e)) else { return };
            window.clear_timeout_with_handle(t.timer);
            long_press();
            e.prevent_default();
            e.stop_propagation();
        }
    });
    // A mouse or trackpad button held on the input (which the browser would focus, or select
    // text in): egui gets the press, and the release from here, as eframe can't tell it's its own.
    let mouse: Rc<Cell<Option<egui::PointerButton>>> = Rc::default();
    let on_down = Closure::<dyn FnMut(web_sys::PointerEvent)>::new({
        let (mouse, send, at) = (mouse.clone(), send.clone(), at.clone());
        move |e: web_sys::PointerEvent| {
            if e.pointer_type() == "touch" || !on_input(&e) {
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
    // gesture ahead of its `touchend` (with moves still queued here). The `touchend` alone releases it.
    let on_up = Closure::<dyn FnMut(web_sys::PointerEvent)>::new({
        let (touch, send, at) = (touch.clone(), send.clone(), at.clone());
        move |e: web_sys::PointerEvent| {
            if e.pointer_type() == "touch" {
                if touch.get().is_some_and(|t| t.gesture != Gesture::Tap) {
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
    // Listening for the page's lifetime.
    on_hold.forget();
    on_start.forget();
    on_move.forget();
    on_end.forget();
    on_menu.forget();
    on_down.forget();
    on_up.forget();
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
