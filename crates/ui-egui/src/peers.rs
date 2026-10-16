//! The other members of a shared document (webxdc): their selection, caret and name on the page,
//! each in their own colour. The host fills [`crate::WordApp::peers`] from their presence.

use egui::{Color32, Painter, Rect, Stroke, pos2, vec2};
use wordcraft_engine::Selection;
use wordcraft_layout::DocLayout;

use crate::WordApp;
use crate::theme::medium;

/// Another member: their name, a colour index ([`color`]) and their selection in this document.
#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    pub name: String,
    pub color: usize,
    pub sel: Selection,
}

/// A member's colour, distinct from the selection's and from each other's.
pub fn color(i: usize) -> Color32 {
    const PALETTE: [Color32; 8] = [
        Color32::from_rgb(0xD9, 0x48, 0x2B),
        Color32::from_rgb(0x2E, 0x8B, 0x57),
        Color32::from_rgb(0x8E, 0x44, 0xAD),
        Color32::from_rgb(0xC2, 0x7C, 0x0E),
        Color32::from_rgb(0x16, 0x8A, 0xAD),
        Color32::from_rgb(0xB0, 0x2E, 0x6E),
        Color32::from_rgb(0x55, 0x6B, 0x2F),
        Color32::from_rgb(0x6D, 0x4C, 0x41),
    ];
    PALETTE[i % PALETTE.len()]
}

/// Paints each peer on the pages at `rects` (screen rects of the laid-out pages).
pub fn paint(app: &WordApp, painter: &Painter, layout: &DocLayout, rects: &[Rect], scale: f32) {
    let doc = &app.session.doc;
    let hint = app.session.page_hint;
    let to_screen = |pi: usize, x: f32, y: f32| -> Option<(egui::Pos2, f32)> {
        let pr = rects.get(pi)?;
        let s = layout.pages.get(pi).map_or(scale, |page| crate::canvas::page_screen_scale(*pr, page, scale));
        Some((pos2(pr.min.x + x * s, pr.min.y + y * s), s))
    };
    for peer in &app.peers {
        let c = color(peer.color);
        // Hostile or stale: positions this document doesn't have are clamped into it.
        let (a, f) = (doc.clamp(&peer.sel.anchor), doc.clamp(&peer.sel.focus));
        if a != f {
            let (lo, hi) = if a <= f { (&a, &f) } else { (&f, &a) };
            for (pi, r) in layout.selection_rects(doc, lo, hi, hint).into_iter().take(2000) {
                if let Some((p, s)) = to_screen(pi, r.x, r.y) {
                    painter.rect_filled(Rect::from_min_size(p, vec2(r.w * s, r.h * s)), 0.0, c.gamma_multiply(0.22));
                }
            }
        }
        let Some(caret) = layout.caret_on(&f, hint) else { continue };
        let Some((top, s)) = to_screen(caret.page, caret.x, caret.top) else { continue };
        let bottom = top + vec2(0.0, caret.height * s);
        painter.line_segment([top, bottom], Stroke::new(2.0, c));
        // The name in a small tag above the caret.
        let galley = painter.layout_no_wrap(peer.name.clone(), medium(10.5), Color32::WHITE);
        let tag = Rect::from_min_size(top - vec2(1.0, galley.size().y + 2.0), galley.size() + vec2(6.0, 2.0));
        painter.rect_filled(tag, 2.0, c);
        painter.galley(tag.min + vec2(3.0, 1.0), galley, Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use wordcraft_doc::{Path, Pos, StoryRef};

    use super::*;

    #[test]
    fn peers_draw_even_with_positions_this_document_lacks() {
        let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("Shared text")), crate::Services::default());
        let far = Pos { story: StoryRef::Part(99), path: Path(vec![u32::MAX; 9]), off: usize::MAX };
        app.peers = vec![
            Peer { name: "Ann".into(), color: 0, sel: Selection { anchor: Pos::body(0, 0), focus: Pos::body(0, 6) } },
            Peer { name: "X".repeat(500), color: usize::MAX, sel: Selection { anchor: far.clone(), focus: far } },
        ];
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(900.0, 700.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(6);
        assert_eq!(h.state().peers.len(), 2);
    }
}
