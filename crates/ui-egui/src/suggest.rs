//! Companion suggestions on the page (fork addition, see `docs/companion.md`). The layout already
//! shows a suggestion in place, on orange, with the words it replaces left out. This draws a small
//! card over the suggestion under the pointer, or else the one at the caret: Accept, Reject, and
//! the words it replaces.

use egui::{Align2, CornerRadius, Id, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::json;
use wordcraft_doc::{Path, Pos, StoryRef};
use wordcraft_engine::cmd::suggest::{Span, spans};

use crate::WordApp;
use crate::theme::{Tokens, regular};

/// Memory key for the card's last rectangle: the pointer may move onto the card and keep it.
fn card_key() -> Id {
    Id::new("companion_suggestion_card")
}

fn shorten(t: &str, max: usize) -> String {
    if t.chars().count() <= max { t.to_string() } else { format!("{}…", t.chars().take(max).collect::<String>().trim_end()) }
}

pub fn show(app: &mut WordApp, ui: &mut Ui) {
    if app.session.view.read_mode {
        return;
    }
    let all = spans(&app.session.doc);
    if all.is_empty() {
        return;
    }
    let ctx = ui.ctx().clone();
    let area = ui.max_rect();
    let hover = ctx.pointer_hover_pos().filter(|p| area.contains(*p));
    let at = |sp: &Span, p: &Pos| p.story == StoryRef::Body && p.path == Path::top(sp.block) && (sp.start..=sp.end).contains(&p.off);
    // (id, card rectangle) from the last frame: the pointer on the card keeps it.
    let last: Option<(u32, Rect)> = ctx.data(|d| d.get_temp(card_key()));
    let kept = last.filter(|(_, r)| hover.is_some_and(|p| r.expand(6.0).contains(p))).and_then(|(id, _)| all.iter().find(|sp| sp.id == id));
    let hovered = || hover.and_then(|p| crate::canvas::pos_from_screen(app, p)).and_then(|p| all.iter().find(|sp| at(sp, &p)));
    let picked = kept.or_else(hovered).or_else(|| all.iter().find(|sp| at(sp, &app.session.sel.focus))).cloned();
    let Some(sp) = picked else {
        ctx.data_mut(|d| d.remove::<(u32, Rect)>(card_key()));
        return;
    };
    // The suggestion's first piece on screen.
    let layout = app.session.layout();
    let (a, b) = (Pos::body(sp.block, sp.start), Pos::body(sp.block, sp.end));
    let Some((page, r)) = layout.selection_rects(&app.session.doc, &a, &b, app.session.page_hint).into_iter().next() else { return };
    let (Some(top_left), Some(bottom)) =
        (crate::canvas::page_to_screen(app, page, r.x, r.y), crate::canvas::page_to_screen(app, page, r.x, r.y + r.h))
    else {
        return;
    };
    let t = Tokens::get(&ctx);
    let font = regular(12.0);
    let note = (!sp.replaces.is_empty()).then(|| format!("replaces “{}”", shorten(&sp.replaces, 60)));
    let note_w = note.as_ref().map_or(0.0, |n| ctx.fonts_mut(|f| f.layout_no_wrap(n.clone(), font.clone(), t.text_dim).size().x) + 10.0);
    let size = vec2(3.0 * 26.0 + 8.0 + note_w, 30.0);
    // Above the line, or below it at the top of the view.
    let mut origin = pos2(top_left.x - 4.0, top_left.y - size.y - 4.0);
    if origin.y < area.top() {
        origin.y = bottom.y + 4.0;
    }
    origin.x = origin.x.clamp(area.left(), (area.right() - size.x).max(area.left()));
    let card = Rect::from_min_size(origin, size);
    ctx.data_mut(|d| d.insert_temp(card_key(), (sp.id, card)));
    let mut action: Option<&str> = None;
    egui::Area::new(Id::new("companion_suggestion_area")).order(egui::Order::Foreground).fixed_pos(origin).show(&ctx, |ui| {
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let p = ui.painter();
        p.rect(rect, CornerRadius::same(6), t.panel, Stroke::new(1.0, t.orange), egui::StrokeKind::Inside);
        let mut x = rect.left() + 4.0;
        for (name, cmd, tip, color) in [
            ("check", "suggest.accept", "Accept suggestion (Alt+Shift+Enter)", t.green),
            ("close", "suggest.reject", "Reject suggestion (Alt+Shift+Backspace)", t.red),
            ("comment", "suggest.footnote", "Keep as a hover note; preserve my wording", t.text_dim),
        ] {
            let b = Rect::from_min_size(pos2(x, rect.top() + 3.0), vec2(24.0, 24.0));
            let resp = ui.interact(b, Id::new(("companion_btn", name)), Sense::click()).on_hover_text(tip);
            if resp.hovered() {
                ui.painter().rect_filled(b, CornerRadius::same(4), t.hover);
            }
            crate::icons::paint(ui.painter(), b.shrink(3.0), name, color, color);
            if resp.clicked() {
                action = Some(cmd);
            }
            x += 26.0;
        }
        if let Some(n) = note {
            ui.painter().text(Pos2::new(x + 6.0, rect.center().y), Align2::LEFT_CENTER, n, font, t.text_dim);
        }
    });
    if let Some(cmd) = action {
        if let Err(e) = app.run(cmd, json!({"id": sp.id})) {
            log::warn!("{cmd}: {e}");
        }
        ctx.data_mut(|d| d.remove::<(u32, Rect)>(card_key()));
        ctx.request_repaint();
    }
}
