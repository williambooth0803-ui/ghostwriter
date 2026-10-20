//! Study information stays off the page. Hover an anchor, or open the persistent history.
use crate::WordApp;
use egui::{Id, Rect, Ui, pos2};
use serde_json::json;
use wordcraft_engine::cmd::study::{self, Note};

fn card(app: &mut WordApp, ui: &mut Ui, note: &Note) {
    ui.label(egui::RichText::new(format!("{} · {}", note.kind, note.status)).strong());
    ui.label(egui::RichText::new(&note.quote).italics());
    ui.add(egui::Label::new(&note.information).selectable(true).wrap());
    for url in note.information.split_whitespace().filter_map(|w| w.find("https://").or_else(|| w.find("http://")).and_then(|i| w.get(i..))).take(12)
    {
        ui.hyperlink_to("Open source", url.trim_end_matches([')', ']', '.', ',', ';']));
    }
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        if note.status != "kept" && ui.button("Keep as hover note").clicked() {
            action = Some("study.keep");
        }
        if note.status != "inserted" && ui.button("Insert into document").clicked() {
            action = Some("study.insert");
        }
        if note.status != "dismissed" && ui.button("Dismiss").on_hover_text("Keep this information in Study Notes history").clicked() {
            action = Some("study.dismiss");
        }
        if note.kind == "answer" && note.status != "queued" && ui.button("Ask again").clicked() {
            action = Some("study.reask");
        }
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(note.information.clone());
        }
    });
    if let Some(command) = action {
        if let Err(e) = app.run(command, json!({"id":note.id})) {
            ui.label(egui::RichText::new(e.to_string()).color(crate::theme::Tokens::get(ui.ctx()).red));
        }
        ui.ctx().request_repaint();
    }
}

pub fn show(app: &mut WordApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let area = ui.max_rect();
    let cache_key = Id::new("study_notes_cache");
    let stamp = (app.session.document_id(), app.session.rev());
    let cached: Option<((u64, u64), Vec<Note>)> = ctx.data(|d| d.get_temp(cache_key));
    let all = match cached {
        Some((s, notes)) if s == stamp => notes,
        _ => {
            let notes = study::notes(&app.session.doc);
            ctx.data_mut(|d| d.insert_temp(cache_key, (stamp, notes.clone())));
            notes
        }
    };
    if all.is_empty() && !app.session.view.study_notes {
        return;
    }
    let mut open = app.session.view.study_notes;
    let pending = all.iter().filter(|n| n.status == "pending").count();
    egui::Area::new(Id::new("study_history_button"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos2((area.right() - 185.0).max(area.left()), area.top() + 8.0))
        .show(&ctx, |ui| {
            if ui.button(format!("Study Notes ({pending} new)")).on_hover_text("Answers, hover notes and dismissed information").clicked() {
                let _ = app.run("view.studyNotes", json!({"value":!open}));
                open = app.session.view.study_notes;
            }
        });
    if open {
        egui::Window::new("Study Notes and History")
            .id(Id::new("study_history_window"))
            .open(&mut open)
            .default_width(420.0)
            .default_height(480.0)
            .show(&ctx, |ui| {
                ui.label("Dismissing a proposal keeps its information here. Save as DOCX to retain notes and history.");
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for note in all.iter().rev() {
                        ui.push_id(note.id, |ui| {
                            egui::Frame::group(ui.style()).show(ui, |ui| card(app, ui, note));
                            if study::anchor(&app.session.doc, note).is_none() {
                                ui.weak("Anchor changed or is ambiguous; information is still available here.");
                            }
                            ui.add_space(6.0);
                        });
                    }
                });
            });
    } else if !app.session.view.read_mode {
        let hover = ctx.pointer_hover_pos().filter(|p| area.contains(*p));
        let last: Option<(u32, Rect)> = ctx.data(|d| d.get_temp(Id::new("study_hover_card")));
        let kept = last.filter(|(_, r)| hover.is_some_and(|p| r.expand(8.0).contains(p))).map(|(id, _)| id);
        let pos = hover.and_then(|p| crate::canvas::pos_from_screen(app, p)).unwrap_or_else(|| app.session.sel.focus.clone());
        let picked = all.iter().filter(|n| n.status == "pending" || n.status == "kept").find(|n| {
            kept == Some(n.id)
                || study::anchor(&app.session.doc, n)
                    .is_some_and(|(a, b)| a.story == pos.story && a.path == pos.path && (a.off..=b.off).contains(&pos.off))
        });
        if let Some(note) = picked
            && let Some((a, b)) = study::anchor(&app.session.doc, note)
        {
            let layout = app.session.layout();
            if let Some((page, r)) = layout.selection_rects(&app.session.doc, &a, &b, app.session.page_hint).into_iter().next()
                && let Some(p) = crate::canvas::page_to_screen(app, page, r.x, r.y + r.h)
            {
                let popup = egui::Area::new(Id::new("study_information_popup"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos2(p.x.clamp(area.left(), (area.right() - 380.0).max(area.left())), p.y + 4.0))
                    .constrain_to(area)
                    .show(&ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_width(360.0);
                            egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| card(app, ui, note));
                        });
                    });
                ctx.data_mut(|d| d.insert_temp(Id::new("study_hover_card"), (note.id, popup.response.rect)));
            }
        } else {
            ctx.data_mut(|d| d.remove::<(u32, Rect)>(Id::new("study_hover_card")));
        }
    }
    app.session.view.study_notes = open;
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    #[test]
    fn study_history_keeps_dismissed_information_and_the_page_has_no_comment_margin() {
        let mut app = WordApp::new(wordcraft_engine::Session::new(wordcraft_doc::Document::from_text("Why? What?")), Default::default());
        app.session.view.proofing = false;
        let a = app.run("study.add", json!({"anchor":"Why?","information":"First independent answer.","kind":"answer"})).unwrap();
        app.run("study.add", json!({"anchor":"What?","information":"Second independent answer.","kind":"answer"})).unwrap();
        let body = app.session.doc.body.clone();
        assert_eq!(crate::canvas::markup_width(&app), 0.0);
        app.run("view.studyNotes", json!({"value":true})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        for _ in 0..5 {
            h.step();
        }
        assert!(h.query_all_by_label("First independent answer.").next().is_some());
        assert!(h.query_all_by_label("Second independent answer.").next().is_some());
        h.state_mut().run("study.dismiss", a).unwrap();
        h.step();
        h.step();
        assert!(h.query_all_by_label("First independent answer.").next().is_some(), "dismissal retains the answer in history");
        assert_eq!(h.state().session.doc.body, body);
    }
}
