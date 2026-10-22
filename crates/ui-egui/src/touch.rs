//! Typing on touch screens (the web build on phones and tablets).
//!
//! iOS opens the on-screen keyboard only for a text input the user taps (or one focused inside a
//! touch handler, and even then not always), while eframe focuses its hidden text input from script a
//! frame after a widget asks for text input. So on touch screens the web shell lays eframe's
//! (transparent) text input over the places that take typing, the [`text_zones`]: the visible pages
//! and the text fields. A tap there lands on the input, the browser focuses it natively (the keyboard
//! comes up), and the shell reports the tap with [`tap_text_zone`]: the next frames place the caret
//! where the page was tapped, or focus the field. Drags, long presses and pinches that begin on the
//! input reach egui as the shell forwards them ([`finger_events`], [`long_press_events`]).
//!
//! While the keyboard is up ([`keyboard_up`]) the input shrinks back to eframe's 1px field at the
//! text cursor, and taps reach the canvas as usual. Until a tap on the page brings the keyboard up,
//! the page asks for no text input on a touch screen (a hardware keyboard still types through the
//! canvas), so that eframe doesn't focus its input from script with no keyboard to show.
//! Nothing here is used where [`set_touch_screen`] is off: desktops are unchanged.

/// The web shell runs on a touch screen (a phone, a tablet): text fields and the page publish
/// [`text_zones`], and the page takes typing only while the on-screen keyboard is up.
pub fn set_touch_screen(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(TOUCH_SCREEN), on));
}

pub fn touch_screen(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp(egui::Id::new(TOUCH_SCREEN))).unwrap_or(false)
}

/// The web shell's text input has (on) or lost (off) the focus a tap gave it: the on-screen
/// keyboard is up, or gone.
pub fn set_keyboard_up(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(KEYBOARD_UP), on));
    ctx.request_repaint();
}

pub fn keyboard_up(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp(egui::Id::new(KEYBOARD_UP))).unwrap_or(false)
}

/// Whether the focused page asks for the platform text input (IME, the on-screen keyboard): always,
/// but on a touch screen only once a tap on the page brought the keyboard up.
pub(crate) fn page_takes_typing(ctx: &egui::Context) -> bool {
    !touch_screen(ctx) || keyboard_up(ctx)
}

/// Where a tap types this frame, in points: the visible pages and the text fields no window, menu
/// or popup covers. None while the keyboard is up (the input then follows the text cursor).
pub fn text_zones(ctx: &egui::Context) -> Vec<egui::Rect> {
    shown_zones(ctx).into_iter().map(|z| z.rect).collect()
}

/// The [`text_zones`] over the pages only.
pub fn page_zones(ctx: &egui::Context) -> Vec<egui::Rect> {
    shown_zones(ctx).into_iter().filter(|z| z.page).map(|z| z.rect).collect()
}

/// The text input over [`text_zones`] was tapped at `p` (points) and took the focus: the next frames
/// place the caret there (a page) or focus the text field there. Returns the zone tapped.
pub fn tap_text_zone(ctx: &egui::Context, p: egui::Pos2) -> Option<egui::Rect> {
    let zone = shown_zones(ctx).into_iter().find(|z| z.rect.contains(p))?;
    let tap = if zone.page { Tap::Page(p) } else { Tap::Field(zone.id) };
    ctx.data_mut(|d| {
        d.insert_temp(egui::Id::new(KEYBOARD_UP), true);
        d.insert_temp(egui::Id::new(TAPPED), Some(tap));
    });
    ctx.request_repaint();
    Some(zone.rect)
}

/// What [`tap_text_zone`] asks the next frame to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Tap {
    /// Place the caret at this point of the page (a click there).
    Page(egui::Pos2),
    /// Focus this text field.
    Field(egui::Id),
}

/// Starts a frame: drops the last frame's zones (this frame publishes its own) and takes the tap
/// waiting for it.
pub(crate) fn begin_pass(ctx: &egui::Context) -> Option<Tap> {
    ctx.data_mut(|d| {
        d.remove::<Vec<Zone>>(egui::Id::new(ZONES));
        d.remove_temp::<Option<Tap>>(egui::Id::new(TAPPED)).flatten()
    })
}

/// Publishes a text field's response as one of the [`text_zones`].
pub(crate) fn text_zone(resp: egui::Response) -> egui::Response {
    if resp.enabled() && touch_screen(&resp.ctx) {
        add_zone(&resp.ctx, Zone { rect: resp.interact_rect, id: resp.id, layer: resp.layer_id, page: false });
    }
    resp
}

/// Publishes the visible part of a page (in `layer`) as one of the [`text_zones`].
pub(crate) fn page_zone(ctx: &egui::Context, rect: egui::Rect, layer: egui::LayerId) {
    if rect.is_positive() && touch_screen(ctx) {
        add_zone(ctx, Zone { rect, id: egui::Id::new(ZONES), layer, page: true });
    }
}

/// The events eframe sends for a finger on its canvas, at `pos` (points): a primary press as it
/// lands, moves, a release as it lifts. The web shell sends them itself for a drag that began on
/// its text input over the [`text_zones`], which eframe's canvas listeners never see.
pub fn finger_events(phase: egui::TouchPhase, id: egui::TouchId, pos: egui::Pos2) -> Vec<egui::Event> {
    use egui::{Event, PointerButton, TouchPhase};
    let button = |pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let touch = Event::Touch { device_id: egui::TouchDeviceId(0), id, phase, pos, force: None };
    match phase {
        TouchPhase::Start => vec![Event::PointerMoved(pos), button(true), touch],
        TouchPhase::Move => vec![Event::PointerMoved(pos), touch],
        TouchPhase::End | TouchPhase::Cancel => vec![button(false), Event::PointerGone, touch],
    }
}

/// A long press on the web shell's text input, at `pos` (points): a secondary press held as it
/// lands, a release as it lifts, so the page opens its context menu (as for a long press eframe
/// reports).
pub fn long_press_events(phase: egui::TouchPhase, pos: egui::Pos2) -> Vec<egui::Event> {
    use egui::{Event, PointerButton, TouchPhase};
    let button = |pressed| Event::PointerButton { pos, button: PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
    match phase {
        TouchPhase::Start => vec![Event::PointerMoved(pos), button(true)],
        TouchPhase::Move => vec![Event::PointerMoved(pos)],
        TouchPhase::End | TouchPhase::Cancel => vec![button(false), Event::PointerGone],
    }
}

#[derive(Clone, Copy)]
struct Zone {
    rect: egui::Rect,
    id: egui::Id,
    layer: egui::LayerId,
    page: bool,
}

fn add_zone(ctx: &egui::Context, zone: Zone) {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<Zone>>(egui::Id::new(ZONES)).push(zone));
}

/// The zones no window, menu or popup covers, even in part (their layer is the one at their
/// centre and no interactable layer above it overlaps them); none while the keyboard is up.
fn shown_zones(ctx: &egui::Context) -> Vec<Zone> {
    if keyboard_up(ctx) {
        return Vec::new();
    }
    let zones: Vec<Zone> = ctx.data(|d| d.get_temp(egui::Id::new(ZONES))).unwrap_or_default();
    // Back to front. The root layer (the page, the ribbon) is no area, so isn't listed: every
    // higher order is above it.
    let layers: Vec<egui::LayerId> = ctx.memory(|m| m.layer_ids().filter(|l| m.areas().is_visible(l)).collect());
    let covered = |r: egui::Rect, layer: egui::LayerId| {
        let at = layers.iter().position(|l| *l == layer);
        layers
            .iter()
            .enumerate()
            .filter(|(i, l)| at.map_or(l.order > layer.order, |at| *i > at))
            .filter_map(|(_, l)| egui::AreaState::load(ctx, l.id))
            .any(|a| a.interactable && a.rect().intersects(r))
    };
    zones
        .into_iter()
        .filter(|z| {
            let r = z.rect.shrink(1.0);
            let top = ctx.layer_id_at(r.center()).unwrap_or(egui::LayerId::background());
            r.is_positive() && top == z.layer && !covered(r, z.layer)
        })
        .collect()
}

const TOUCH_SCREEN: &str = "wordcraft.touch_screen";
const KEYBOARD_UP: &str = "wordcraft.keyboard_up";
const ZONES: &str = "wordcraft.text_zones";
const TAPPED: &str = "wordcraft.text_zone_tapped";
