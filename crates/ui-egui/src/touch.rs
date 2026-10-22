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
//! the page asks for no text input on a touch screen, so that eframe doesn't focus its input from
//! script with no keyboard to show; a key typed or a mouse click on the page asks for it too (a
//! hardware keyboard is there: dead keys and input methods compose). A page partly under a window,
//! menu or popup takes taps around it ([`subtract`]).
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

/// Where a tap types this frame, in points: the visible pages (in pieces around the windows, menus
/// and popups over them) and the text fields none of those covers. None while the keyboard is up
/// (the input then follows the text cursor).
pub fn text_zones(ctx: &egui::Context) -> Vec<egui::Rect> {
    shown_zones(ctx).into_iter().map(|z| z.rect).collect()
}

/// The [`text_zones`] over the pages that nothing covers, even in part: for a browser that can't
/// clip the input to the zones, which then lies over their bounding box.
pub fn page_zones(ctx: &egui::Context) -> Vec<egui::Rect> {
    shown_zones(ctx).into_iter().filter(|z| z.page && z.whole).map(|z| z.rect).collect()
}

/// Whether a tap at `p` (points) that reached the canvas instead of the text input lands on a page
/// taking taps (the input stood aside for a mouse, or the keyboard was up): the web shell then
/// focuses its input from the touch handler, which opens the keyboard there.
pub fn tap_opens_keyboard(ctx: &egui::Context, p: egui::Pos2) -> bool {
    placed_zones(ctx).iter().any(|z| z.page && z.rect.contains(p))
}

/// Notes a key typed on the page from a hardware keyboard (a tablet's): on a touch screen the page
/// asks for the platform text input from then on, as the keyboard is there, so dead keys and input
/// methods compose. A mouse click on the page counts the same: no finger, so a keyboard is likely.
pub(crate) fn note_keyboard(ctx: &egui::Context) {
    if touch_screen(ctx) && !keyboard_up(ctx) {
        set_keyboard_up(ctx, true);
    }
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
        add_zone(&resp.ctx, Zone { rect: resp.interact_rect, id: resp.id, layer: resp.layer_id, page: false, whole: true });
    }
    resp
}

/// Publishes the visible part of a page (in `layer`) as one of the [`text_zones`].
pub(crate) fn page_zone(ctx: &egui::Context, rect: egui::Rect, layer: egui::LayerId) {
    if rect.is_positive() && touch_screen(ctx) {
        add_zone(ctx, Zone { rect, id: egui::Id::new(ZONES), layer, page: true, whole: true });
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
    /// Nothing covers any of it (a page's pieces around what covers it are not whole).
    whole: bool,
}

fn add_zone(ctx: &egui::Context, zone: Zone) {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<Zone>>(egui::Id::new(ZONES)).push(zone));
}

/// The zones shown: a text field when no window, menu or popup covers it, even in part (its layer
/// is the one at its centre and no interactable layer above it overlaps it); a page in the pieces
/// around those. None while the keyboard is up.
fn shown_zones(ctx: &egui::Context) -> Vec<Zone> {
    if keyboard_up(ctx) {
        return Vec::new();
    }
    placed_zones(ctx)
}

/// The zones as [`shown_zones`] lays them out, whether or not the keyboard is up.
fn placed_zones(ctx: &egui::Context) -> Vec<Zone> {
    let zones: Vec<Zone> = ctx.data(|d| d.get_temp(egui::Id::new(ZONES))).unwrap_or_default();
    // Back to front. The root layer (the page, the ribbon) is no area, so isn't listed: every
    // higher order is above it.
    let layers: Vec<egui::LayerId> = ctx.memory(|m| m.layer_ids().filter(|l| m.areas().is_visible(l)).collect());
    let over = |r: egui::Rect, layer: egui::LayerId| -> Vec<egui::Rect> {
        let at = layers.iter().position(|l| *l == layer);
        layers
            .iter()
            .enumerate()
            .filter(|(i, l)| at.map_or(l.order > layer.order, |at| *i > at))
            .filter_map(|(_, l)| egui::AreaState::load(ctx, l.id))
            .filter(|a| a.interactable && a.rect().intersects(r))
            .map(|a| a.rect())
            .collect()
    };
    // A menu, drop-down or popup is open (they lie in the foreground; windows and the mini toolbar
    // in the middle): a tap beside it dismisses it, so the pages take none.
    let popup = egui::Popup::is_any_open(ctx)
        || layers.iter().any(|l| l.order >= egui::Order::Foreground && egui::AreaState::load(ctx, l.id).is_some_and(|a| a.interactable));
    let mut shown = Vec::new();
    for z in zones {
        let r = z.rect.shrink(1.0);
        if !r.is_positive() {
            continue;
        }
        let holes = over(r, z.layer);
        if z.page && popup {
            continue;
        }
        if z.page {
            let whole = holes.is_empty();
            shown.extend(subtract(z.rect, &holes).into_iter().map(|rect| Zone { rect, whole, ..z }));
        } else if holes.is_empty() && ctx.layer_id_at(r.center()).unwrap_or(egui::LayerId::background()) == z.layer {
            shown.push(z);
        }
    }
    shown
}

/// Most pieces a page is cut into around what covers it.
const MAX_PIECES: usize = 64;

/// `rect` less `holes`, as rectangles that don't overlap (at most [`MAX_PIECES`]).
pub(crate) fn subtract(rect: egui::Rect, holes: &[egui::Rect]) -> Vec<egui::Rect> {
    let mut pieces = vec![rect];
    for h in holes {
        pieces = pieces
            .into_iter()
            .flat_map(|r| {
                let i = r.intersect(*h);
                if !i.is_positive() {
                    return vec![r];
                }
                // Above and below it the whole width; beside it its height.
                [
                    egui::Rect::from_min_max(r.min, egui::pos2(r.max.x, i.min.y)),
                    egui::Rect::from_min_max(egui::pos2(r.min.x, i.max.y), r.max),
                    egui::Rect::from_min_max(egui::pos2(r.min.x, i.min.y), egui::pos2(i.min.x, i.max.y)),
                    egui::Rect::from_min_max(egui::pos2(i.max.x, i.min.y), egui::pos2(r.max.x, i.max.y)),
                ]
                .into_iter()
                .filter(|p| p.is_positive())
                .collect()
            })
            .take(MAX_PIECES)
            .collect();
    }
    pieces
}

const TOUCH_SCREEN: &str = "wordcraft.touch_screen";
const KEYBOARD_UP: &str = "wordcraft.keyboard_up";
const ZONES: &str = "wordcraft.text_zones";
const TAPPED: &str = "wordcraft.text_zone_tapped";

#[cfg(test)]
mod tests {
    use egui::{Rect, pos2};

    #[test]
    fn a_page_is_cut_around_what_covers_it() {
        let page = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0));
        let toolbar = Rect::from_min_max(pos2(20.0, 30.0), pos2(60.0, 50.0));
        let menu = Rect::from_min_max(pos2(50.0, 40.0), pos2(150.0, 70.0));
        let pieces = super::subtract(page, &[toolbar, menu]);
        let area: f32 = pieces.iter().map(|p| p.area()).sum();
        let covered = toolbar.area() + menu.intersect(page).area() - toolbar.intersect(menu).area();
        assert!((area - (page.area() - covered)).abs() < 0.01, "{pieces:?}");
        for (i, a) in pieces.iter().enumerate() {
            assert!(!a.intersects(toolbar.shrink(0.1)) && !a.intersects(menu.shrink(0.1)), "no piece under a hole");
            assert!(pieces.iter().skip(i + 1).all(|b| !a.shrink(0.1).intersects(b.shrink(0.1))), "pieces don't overlap");
        }
        assert_eq!(super::subtract(page, &[]), vec![page]);
        assert!(super::subtract(page, &[page.expand(5.0)]).is_empty(), "a modal's backdrop leaves nothing");
    }
}
