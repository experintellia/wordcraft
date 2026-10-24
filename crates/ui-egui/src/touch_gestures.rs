//! A finger on the page (touch screens only, `touch::touch_screen`): a drag scrolls the document
//! and flings it on, a tap clicks, and a long press selects the word under the finger, a drag on
//! from there extending the selection (lifted without moving, the context menu opens on it).
//!
//! eframe (and the web shell for touches on its text input, `touch::finger_events`) reports a
//! finger as a primary press and touch events. [`Gestures::input`] reads them in `raw_input_hook`
//! before egui does: a single finger that lands on the page has its pointer events held back from
//! egui and replaced by what it turned into. Its touch events still reach egui (a single touch
//! does nothing there, and eframe goes on reporting a finger egui has a touch for, wherever it
//! moves), except during a long press, which egui follows as a pointer press instead (its own long
//! touch would open the menu). Two fingers go to egui as touches (a pinch zooms). In Read Mode a
//! swipe turns the page (`canvas`). Anything else (a
//! mouse, a pen, a finger on the ribbon, a pane or a dialog, whose scroll areas drag-scroll by
//! themselves) passes untouched.

use std::collections::VecDeque;

use egui::{Event, PointerButton, Pos2, TouchId, TouchPhase, Vec2};

/// How far (points) a finger may move and still tap or long-press.
const SLOP: f32 = 8.0;
/// Most frames of synthesised input waiting (a tap's release, a long press's clicks).
const MAX_QUEUED: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Idle,
    /// Down, not yet moved: a tap, a drag or a long press to be.
    Pending {
        id: TouchId,
        start: Pos2,
        at: f64,
    },
    /// Scrolling the page with the finger.
    Scroll {
        id: TouchId,
        last: Pos2,
    },
    /// A long press: the word is selected and the finger drags the selection on.
    Hold {
        id: TouchId,
        start: Pos2,
        moved: bool,
    },
    /// Two fingers or more (egui sees their touches); until the last lifts.
    Multi {
        fingers: u8,
    },
    /// Not ours (a pen, a finger off the page): passes through until it lifts.
    Pass {
        id: TouchId,
    },
}

/// The finger gestures on the page, and the scroll they give it.
#[derive(Debug)]
pub struct Gestures {
    phase: Phase,
    /// Input for the coming frames, one entry per frame (a click's release goes a frame after its
    /// press, so the page sees a click).
    queue: VecDeque<Vec<Event>>,
    /// Scroll not yet applied (screen points, positive = content moves down/right).
    scroll: Vec2,
    momentum: crate::scroll::Momentum,
    /// A primary press passed through to egui and isn't released yet (a pen's), and whether one was
    /// before this frame (a finger's own press comes in the frame of its touch).
    pointer_down: bool,
    down_before: bool,
    /// The input clock, for a platform that reports none (tests).
    clock: f64,
    /// This frame's pointer input came from a finger.
    finger: bool,
}

impl Default for Gestures {
    fn default() -> Self {
        Gestures {
            phase: Phase::Idle,
            queue: VecDeque::new(),
            scroll: Vec2::ZERO,
            momentum: Default::default(),
            pointer_down: false,
            down_before: false,
            clock: 0.0,
            finger: false,
        }
    }
}

impl Gestures {
    /// Rewrites this frame's input on a touch screen. `page`: the page's viewport (screen points)
    /// last frame, where a finger is ours if nothing lies over it.
    pub fn input(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput, page: Option<egui::Rect>) {
        let now = raw.time.unwrap_or(self.clock + f64::from(raw.predicted_dt));
        self.clock = now;
        let (hold, ours) = (ctx.options(|o| o.input_options.max_click_duration), |p: Pos2| {
            page.is_some_and(|r| r.contains(p)) && ctx.layer_id_at(p).is_none_or(|l| l.order == egui::Order::Background)
        });
        // A finger alone on the screen: a touch still followed lost its end (lifted where nothing
        // reported it), and is done.
        if ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::new(ALONE))).unwrap_or(false) {
            self.abandon(&mut raw.events);
        }
        self.down_before = self.pointer_down;
        let mut out = self.queue.pop_front().unwrap_or_default();
        self.finger = !out.is_empty();
        for event in std::mem::take(&mut raw.events) {
            match event {
                Event::Touch { id, phase, pos, .. } => self.touch(&mut out, event.clone(), id, phase, pos, &ours),
                Event::PointerButton { .. } | Event::PointerMoved(_) | Event::PointerGone if self.holds_pointer() => {}
                Event::PointerButton { button: PointerButton::Primary, pressed, .. } => {
                    self.pointer_down = pressed;
                    out.push(event);
                }
                _ => out.push(event),
            }
        }
        // Held still long enough: a long press, which selects the word (a double click held down).
        if let Phase::Pending { id, start, at } = self.phase {
            if now - at >= hold {
                self.phase = Phase::Hold { id, start, moved: false };
                // egui follows it as a pointer press from now on: its touch ends.
                out.push(touch_event(id, TouchPhase::Cancel, start));
                self.click(&mut out, start, PointerButton::Primary);
                self.later(vec![press(start, PointerButton::Primary, true)]);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64((hold - (now - at)).clamp(0.0, 10.0)));
            }
        }
        if !self.queue.is_empty() || self.momentum.is_active() {
            ctx.request_repaint();
        }
        let held = matches!(self.phase, Phase::Hold { .. });
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(HELD), held));
        raw.events = out;
    }

    /// Drops the gesture in progress, ending what egui was told of it ahead of `events`.
    fn abandon(&mut self, events: &mut Vec<Event>) {
        let ended = match self.phase {
            Phase::Pending { id, start: at, .. } | Phase::Scroll { id, last: at } => vec![touch_event(id, TouchPhase::Cancel, at)],
            Phase::Hold { start, .. } => vec![press(start, PointerButton::Primary, false), Event::PointerGone],
            Phase::Idle | Phase::Multi { .. } | Phase::Pass { .. } => Vec::new(),
        };
        self.queue.clear();
        self.momentum.cancel();
        self.phase = Phase::Idle;
        events.splice(0..0, ended);
    }

    /// The finger is dragging the page (a swipe, in Read Mode).
    pub fn scrolling(&self) -> bool {
        matches!(self.phase, Phase::Scroll { .. })
    }

    /// The scroll for this frame (`dt` seconds after the last): what the finger moved, or the fling
    /// after it lifted.
    pub fn take_scroll(&mut self, dt: f32) -> Vec2 {
        let moved = std::mem::take(&mut self.scroll);
        if matches!(self.phase, Phase::Scroll { .. }) {
            // Also a frame without movement: fingers that rest before lifting don't fling.
            self.momentum.feed(moved, dt);
            moved
        } else {
            moved + self.momentum.tick(dt)
        }
    }

    /// The page can scroll no farther: stop the fling.
    pub fn hit_edge(&mut self) {
        self.momentum.cancel();
    }

    /// This frame's pointer input stands for a finger (no mouse click).
    pub fn finger(&self) -> bool {
        self.finger
    }

    /// The finger's own pointer events (from eframe, or the web shell) don't reach egui.
    fn holds_pointer(&self) -> bool {
        matches!(self.phase, Phase::Pending { .. } | Phase::Scroll { .. } | Phase::Hold { .. } | Phase::Multi { .. })
    }

    fn touch(&mut self, out: &mut Vec<Event>, event: Event, id: TouchId, phase: TouchPhase, pos: Pos2, ours: &dyn Fn(Pos2) -> bool) {
        if phase == TouchPhase::Start {
            // A new touch stops a fling.
            self.momentum.cancel();
        }
        match (self.phase, phase) {
            (Phase::Idle, TouchPhase::Start) if !self.down_before && ours(pos) => {
                // The finger's press may have come just before its touch, in this frame.
                out.retain(|e| !matches!(e, Event::PointerButton { .. } | Event::PointerMoved(_) | Event::PointerGone));
                self.pointer_down = false;
                self.phase = Phase::Pending { id, start: pos, at: self.clock };
                out.push(event);
            }
            (Phase::Idle, TouchPhase::Start) => {
                self.phase = Phase::Pass { id };
                out.push(event);
            }
            (Phase::Pass { id: own }, TouchPhase::End | TouchPhase::Cancel) if own == id => {
                self.phase = Phase::Idle;
                out.push(event);
            }
            (Phase::Pass { .. } | Phase::Idle, _) => out.push(event),
            // A second finger: a pinch, which egui reads from the touches.
            (
                Phase::Pending { id: first, start: at, .. } | Phase::Scroll { id: first, last: at } | Phase::Hold { id: first, start: at, .. },
                TouchPhase::Start,
            ) => {
                // egui has the first finger's touch, but for a long press's.
                if let Phase::Hold { .. } = self.phase {
                    self.now_or_later(out, vec![press(at, PointerButton::Primary, false), Event::PointerGone]);
                    out.push(touch_event(first, TouchPhase::Start, at));
                }
                out.push(event);
                self.phase = Phase::Multi { fingers: 2 };
            }
            (Phase::Multi { fingers }, TouchPhase::Start) => {
                self.phase = Phase::Multi { fingers: fingers.saturating_add(1) };
                out.push(event);
            }
            (Phase::Multi { fingers }, TouchPhase::End | TouchPhase::Cancel) => {
                self.phase = if fingers <= 1 { Phase::Idle } else { Phase::Multi { fingers: fingers - 1 } };
                out.push(event);
            }
            (Phase::Multi { .. }, _) => out.push(event),
            (Phase::Pending { id: own, start, .. }, TouchPhase::Move) if own == id => {
                out.push(event);
                if (pos - start).length() > SLOP {
                    // Scrolling from where the finger landed, so the page stays under it.
                    self.scroll += pos - start;
                    self.phase = Phase::Scroll { id, last: pos };
                }
            }
            (Phase::Pending { id: own, start, .. }, TouchPhase::End) if own == id => {
                out.push(event);
                self.phase = Phase::Idle;
                self.click(out, start, PointerButton::Primary);
            }
            (Phase::Scroll { id: own, last }, TouchPhase::Move) if own == id => {
                out.push(event);
                self.scroll += pos - last;
                self.phase = Phase::Scroll { id, last: pos };
            }
            (Phase::Scroll { id: own, .. }, TouchPhase::End) if own == id => {
                out.push(event);
                self.phase = Phase::Idle;
                self.momentum.release();
            }
            (Phase::Hold { id: own, start, moved }, TouchPhase::Move) if own == id => {
                self.phase = Phase::Hold { id, start, moved: moved || (pos - start).length() > SLOP };
                self.now_or_later(out, vec![Event::PointerMoved(pos)]);
            }
            (Phase::Hold { id: own, moved, .. }, TouchPhase::End | TouchPhase::Cancel) if own == id => {
                self.phase = Phase::Idle;
                self.now_or_later(out, vec![press(pos, PointerButton::Primary, false)]);
                // Lifted where it was pressed: the context menu, on the word.
                if !moved && phase == TouchPhase::End {
                    self.later(vec![press(pos, PointerButton::Secondary, true)]);
                    self.later(vec![press(pos, PointerButton::Secondary, false), Event::PointerGone]);
                } else {
                    self.later(vec![Event::PointerGone]);
                }
            }
            // A cancelled touch (the web shell's tap, which its text input takes) does nothing.
            (Phase::Pending { id: own, .. } | Phase::Scroll { id: own, .. }, TouchPhase::Cancel) if own == id => {
                out.push(event);
                self.phase = Phase::Idle;
                self.momentum.cancel();
            }
            _ => {}
        }
    }

    /// A press now and its release next frame (the page sees one click per frame).
    fn click(&mut self, out: &mut Vec<Event>, at: Pos2, button: PointerButton) {
        self.now_or_later(out, vec![Event::PointerMoved(at), press(at, button, true)]);
        self.later(vec![press(at, button, false)]);
    }

    /// Events for this frame, after those still queued.
    fn now_or_later(&mut self, out: &mut Vec<Event>, events: Vec<Event>) {
        self.finger = true;
        match self.queue.back_mut() {
            Some(last) => last.extend(events),
            None => out.extend(events),
        }
    }

    /// Events for a frame after the queued ones.
    fn later(&mut self, events: Vec<Event>) {
        if self.queue.len() < MAX_QUEUED {
            self.queue.push_back(events);
        }
    }
}

/// Whether a long press holds the page (the web shell then takes the finger for no tap).
pub fn long_pressed(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp(egui::Id::new(HELD))).unwrap_or(false)
}

/// The web shell saw a finger land with no other on the screen: any gesture still in progress
/// lost its end.
pub fn finger_alone(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(ALONE), true));
}

fn touch_event(id: TouchId, phase: TouchPhase, pos: Pos2) -> Event {
    Event::Touch { device_id: egui::TouchDeviceId(0), id, phase, pos, force: None }
}

const HELD: &str = "wordcraft.touch_held";
const ALONE: &str = "wordcraft.touch_alone";

fn press(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE }
}
