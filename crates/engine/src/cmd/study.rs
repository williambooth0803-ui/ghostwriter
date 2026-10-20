//! Quiet companion information. Stored as self-describing DOCX comments, without body markers:
//! no added glyphs, highlights, pagination changes or caret movement. Anchors are found again
//! from the quote and paragraph; ambiguous/missing anchors stay accessible in the history.
use serde_json::{Value, json};
use wordcraft_doc::{Block, Comment, Document, Paragraph, PartKind, Path, Pos, StoryRef, para_block};

use crate::{CmdError, CmdResult, CommandSpec, Session};

const PREFIX: &str = "WordCraft study note v1 | ";
const MAX_NOTES: usize = 2048;
const MAX_TEXT: usize = 16 * 1024;

#[derive(Clone, Debug)]
pub struct Note {
    pub id: u32,
    pub block: Option<usize>,
    pub status: String,
    pub kind: String,
    pub quote: String,
    pub paragraph: String,
    pub information: String,
}

pub fn is_note(doc: &Document, c: &Comment) -> bool {
    doc.parts.get(&c.part).and_then(|p| p.blocks.first()).is_some_and(|b| matches!(&**b, Block::Para(p) if p.text.starts_with(PREFIX)))
}

pub fn notes(doc: &Document) -> Vec<Note> {
    doc.comments
        .iter()
        .filter_map(|(id, c)| {
            if !is_note(doc, c) {
                return None;
            }
            let text = doc.plain_text(StoryRef::Part(c.part));
            let (head, information) = text.split_once("\n\n")?;
            let mut lines = head.lines();
            let (status, kind) = lines.next()?.strip_prefix(PREFIX)?.split_once(" | ")?;
            Some(Note {
                id: *id,
                status: status.into(),
                kind: kind.into(),
                quote: lines.next()?.strip_prefix("Anchor: ")?.into(),
                paragraph: lines.next()?.strip_prefix("Context: ")?.into(),
                block: lines.next().and_then(|l| l.strip_prefix("Block: ")).and_then(|l| l.parse().ok()),
                information: information.trim_end().into(),
            })
        })
        .collect()
}

fn blocks(note: &Note) -> Vec<std::sync::Arc<Block>> {
    let text = format!(
        "{PREFIX}{} | {}\nAnchor: {}\nContext: {}\nBlock: {}\n\n{}",
        note.status,
        note.kind,
        note.quote,
        note.paragraph,
        note.block.map(|b| b.to_string()).unwrap_or_default(),
        note.information
    );
    text.lines().map(|line| para_block(Paragraph::with_text(line, Default::default()))).collect()
}

/// Visible text, independent of inline proposals and invisible object markers.
pub fn paragraph_text(doc: &Document, block: usize) -> Option<String> {
    let p = doc.para(StoryRef::Body, &Path::top(block))?;
    Some(
        p.run_ranges()
            .filter(|(_, c)| c.del.is_none())
            .filter_map(|(r, _)| p.text.get(r))
            .flat_map(str::chars)
            .filter(|c| *c != wordcraft_doc::para::OBJ)
            .collect(),
    )
}

/// Only attach a hover card when the anchor is unambiguous. Do not guess after edits.
pub fn anchor(doc: &Document, note: &Note) -> Option<(Pos, Pos)> {
    if note.quote.is_empty() {
        return None;
    }
    if let Some(block) = note.block
        && paragraph_text(doc, block).as_deref() == Some(&note.paragraph)
        && let Some(p) = doc.para(StoryRef::Body, &Path::top(block))
        && p.text.matches(&note.quote).count() == 1
        && let Some(off) = p.text.find(&note.quote)
    {
        return Some((Pos::body(block, off), Pos::body(block, off + note.quote.len())));
    }
    let mut exact = Vec::new();
    let mut other = Vec::new();
    for (block, b) in doc.body.iter().enumerate() {
        let Block::Para(p) = &**b else { continue };
        // Notes never inject markers, so plain quotes have ordinary byte offsets. Tracked
        // text/objects within a quote do not match; the history still preserves the information.
        for (off, _) in p.text.match_indices(&note.quote) {
            let pair = (Pos::body(block, off), Pos::body(block, off + note.quote.len()));
            if paragraph_text(doc, block).as_deref() == Some(&note.paragraph) {
                exact.push(pair);
            } else {
                other.push(pair);
            }
            if exact.len() > 1 || other.len() > MAX_NOTES {
                return None;
            }
        }
    }
    if exact.len() == 1 {
        exact.pop()
    } else if other.len() == 1 {
        other.pop()
    } else {
        None
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("study.add", "Add Study Information", "Review › Companion", add)
            .params(r#"{"anchor": string, "information": string, "kind"?: "answer|context|fact-check|vocabulary", "block"?: n, "provider"?: "codex|claude"}"#).pure(),
        CommandSpec::new("study.list", "Study Notes and History", "Review › Companion", |s, _| {
            Ok(Value::Array(notes(&s.doc).iter().map(|n| json!({"id":n.id,"status":n.status,"kind":n.kind,"anchor":n.quote,"information":n.information,"located":anchor(&s.doc,n).is_some()})).collect()))
        }).pure(),
        CommandSpec::new("study.keep", "Keep as Hover Note", "Review › Companion", |s, v| update(s,v,"kept")),
        CommandSpec::new("study.dismiss", "Dismiss and Keep in History", "Review › Companion", |s, v| update(s,v,"dismissed")),
        CommandSpec::new("study.reask", "Ask About This Again", "Review › Companion", |s,v| update(s,v,"queued")),
        CommandSpec::new("study.insert", "Insert Study Information", "Review › Companion", insert),
    ]
}

fn writable(s: &Session) -> Result<(), CmdError> {
    if s.doc.settings.protection.is_some() { Err(CmdError::Disabled("the document is protected".into())) } else { Ok(()) }
}

fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str, CmdError> {
    let text = v
        .get(key)
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty() && t.len() <= MAX_TEXT)
        .ok_or_else(|| CmdError::Params(format!("`{key}` must be nonempty text of at most {MAX_TEXT} bytes")))?;
    if key == "anchor" && text.contains(['\n', '\r']) {
        return Err(CmdError::Params("an anchor stays in one paragraph".into()));
    }
    Ok(text)
}

fn add(s: &mut Session, v: &Value) -> CmdResult {
    writable(s)?;
    let quote = field(v, "anchor")?;
    let information = field(v, "information")?;
    let kind = v.get("kind").and_then(Value::as_str).unwrap_or("context");
    if !["answer", "context", "fact-check", "vocabulary"].contains(&kind) {
        return Err(CmdError::Params("invalid information kind".into()));
    }
    let provider = v.get("provider").and_then(Value::as_str).unwrap_or("codex");
    if !["codex", "claude"].contains(&provider) {
        return Err(CmdError::Params("invalid provider".into()));
    }
    let block = v
        .get("block")
        .map(|b| b.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| CmdError::Params("invalid block".into())))
        .transpose()?;
    let found: Vec<_> = s
        .doc
        .body
        .iter()
        .enumerate()
        .filter(|(i, _)| block.is_none_or(|b| b == *i))
        .filter_map(|(i, _)| paragraph_text(&s.doc, i).map(|t| (i, t)))
        .filter(|(_, t)| t.matches(quote).count() == 1)
        .collect();
    if found.len() != 1 {
        return Err(CmdError::Failed("the study anchor is missing or ambiguous".into()));
    }
    let (block, paragraph) = found.first().cloned().unwrap_or_default();
    let id = record(
        s,
        Note {
            id: 0,
            block: Some(block),
            status: "pending".into(),
            kind: kind.into(),
            quote: quote.into(),
            paragraph,
            information: information.into(),
        },
        provider,
    )?;
    Ok(json!({"id":id}))
}

pub(super) fn record(s: &mut Session, mut note: Note, provider: &str) -> Result<u32, CmdError> {
    note.quote = note.quote.replace(['\n', '\r'], " ");
    note.paragraph = note.paragraph.replace(['\n', '\r'], " ");
    let all = notes(&s.doc);
    let old = all.iter().find(|n| {
        n.block == note.block && n.quote == note.quote && n.paragraph == note.paragraph && n.information == note.information && n.kind == note.kind
    });
    // A new answer completes an explicit recheck. Keep older information in history.
    let target = anchor(&s.doc, &note);
    let queued: Vec<_> = all
        .iter()
        .filter(|n| {
            n.status == "queued" && n.quote == note.quote && (n.paragraph == note.paragraph || (target.is_some() && anchor(&s.doc, n) == target))
        })
        .cloned()
        .collect();
    for queued in queued {
        let mut completed = queued.clone();
        completed.status = if old.is_some_and(|o| o.id == queued.id) { note.status.clone() } else { "dismissed".into() };
        if let Some(part) = s.doc.comments.get(&queued.id).map(|c| c.part) {
            s.doc.set_story(StoryRef::Part(part), blocks(&completed))?;
        }
        s.touch();
    }
    if let Some(old) = old {
        return Ok(old.id);
    }
    if all.len() >= MAX_NOTES {
        return Err(CmdError::Failed("study note limit reached; no information was removed".into()));
    }
    if note.quote.is_empty() || note.quote.len() > MAX_TEXT || note.information.len() > MAX_TEXT || note.paragraph.len() > 64 * 1024 {
        return Err(CmdError::Params("study information is too large or has no anchor".into()));
    }
    let id = wordcraft_doc::free_id(s.doc.comments.keys().next_back().copied(), s.doc.comments.keys().copied(), 0)
        .ok_or_else(|| CmdError::Failed("no free study note id".into()))?;
    let part = s.doc.try_add_part(PartKind::Comment, blocks(&note))?;
    s.doc.comments.insert(
        id,
        Comment {
            author: format!("{provider} (study note)"),
            initials: "AI".into(),
            date: super::now_iso(),
            part,
            resolved: true,
            ..Default::default()
        },
    );
    s.touch();
    Ok(id)
}

fn get(s: &Session, v: &Value) -> Result<Note, CmdError> {
    let id = v.get("id").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| CmdError::Params("`id` required".into()))?;
    notes(&s.doc).into_iter().find(|n| n.id == id).ok_or_else(|| CmdError::Failed("no such study note".into()))
}

fn update(s: &mut Session, v: &Value, status: &str) -> CmdResult {
    writable(s)?;
    let mut n = get(s, v)?;
    n.status = status.into();
    let part = s.doc.comments.get(&n.id).map(|c| c.part).ok_or_else(|| CmdError::Failed("study note is gone".into()))?;
    s.doc.set_story(StoryRef::Part(part), blocks(&n))?;
    s.touch();
    Ok(json!({"id":n.id,"status":status}))
}

fn insert(s: &mut Session, v: &Value) -> CmdResult {
    writable(s)?;
    let n = get(s, v)?;
    if n.status == "inserted" {
        return Err(CmdError::Failed("this information was already inserted".into()));
    }
    let (_, end) =
        anchor(&s.doc, &n).ok_or_else(|| CmdError::Failed("the anchor changed or became ambiguous; copy the information from Study Notes".into()))?;
    let text = format!(" {}", n.information.replace(['\n', '\r'], " "));
    let p = s.doc.para_mut(StoryRef::Body, &end.path)?;
    let mut props = p.props_of_char(end.off.saturating_sub(1)).clone();
    props.ins = None;
    props.del = None;
    props.fmt_change = None;
    let added = p.insert_text(end.off, &text, &props)?;
    super::suggest::shift_selection(s, usize::try_from(*end.path.0.first().unwrap_or(&0)).unwrap_or(usize::MAX), end.off, 0, added);
    update(s, v, "inserted")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> Session {
        let mut s = Session::new(Document::new());
        s.view.proofing = false;
        s.run("document.setText", &json!({"text":"Why? What is Schadenfreude?"})).unwrap();
        s
    }
    fn add(s: &mut Session, quote: &str, info: &str) -> Value {
        s.run("study.add", &json!({"anchor":quote,"information":info,"kind":"answer","block":0})).unwrap()
    }
    #[test]
    fn two_answers_preserve_body_caret_layout_and_survive_docx_and_dismissal() {
        let mut s = session();
        let before = s.doc.body.clone();
        let sel = s.sel.clone();
        let a = add(&mut s, "Why?", "Because of the context.");
        let b = add(&mut s, "What is Schadenfreude?", "Pleasure at another person's misfortune.\nCompare Mitgefühl.");
        assert_ne!(a["id"], b["id"]);
        assert_eq!(s.doc.body, before);
        assert_eq!(s.sel, sel);
        assert_eq!(notes(&s.doc).len(), 2);
        assert!(super::super::review::comment_list(&s).is_empty());
        let c = s.run("suggest.context", &json!({})).unwrap();
        assert_eq!(c["blocks"][0]["answered"].as_array().unwrap().len(), 2);
        s.run("study.keep", &a).unwrap();
        s.run("study.dismiss", &b).unwrap();
        assert_eq!(s.doc.body, before);
        let bytes = wordcraft_docx::write(&s.doc).unwrap();
        let mut reopened = Session::new(wordcraft_docx::read(&bytes).unwrap());
        let all = notes(&reopened.doc);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].status, "kept");
        assert_eq!(all[1].status, "dismissed");
        assert!(all[1].information.contains("Mitgefühl"));
        assert!(anchor(&reopened.doc, &all[0]).is_some());
        reopened.run("study.keep", &json!({"id":all[1].id})).unwrap();
        assert_eq!(notes(&reopened.doc)[1].status, "kept");
    }
    #[test]
    fn inserting_and_undoing_information_does_not_erase_the_note() {
        let mut s = session();
        let before = paragraph_text(&s.doc, 0).unwrap();
        s.run("caret.docEnd", &json!({})).unwrap();
        let old_off = s.sel.focus.off;
        let a = add(&mut s, "Why?", "Because.");
        s.run("study.insert", &a).unwrap();
        assert_eq!(paragraph_text(&s.doc, 0).unwrap(), "Why? Because. What is Schadenfreude?");
        assert_eq!(s.sel.focus.off, old_off + " Because.".len());
        assert_eq!(notes(&s.doc)[0].status, "inserted");
        s.run("study.insert", &a).unwrap_err();
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(paragraph_text(&s.doc, 0).unwrap(), before);
        assert_eq!(notes(&s.doc)[0].status, "pending");
    }
    #[test]
    fn rejected_or_footnoted_inline_information_is_preserved() {
        for command in ["suggest.reject", "suggest.footnote"] {
            let mut s = session();
            s.run("caret.docEnd", &json!({})).unwrap();
            let a = s.run("suggest.add", &json!({"after":"Why?","insert":" Because of X.","provider":"codex"})).unwrap();
            s.run(command, &a).unwrap();
            assert_eq!(paragraph_text(&s.doc, 0).unwrap(), "Why? What is Schadenfreude?");
            let all = notes(&s.doc);
            assert_eq!(all.len(), 1);
            assert!(all[0].information.contains("Because of X"));
            assert_eq!(all[0].status, if command == "suggest.reject" { "dismissed" } else { "kept" });
            assert!(anchor(&s.doc, &all[0]).is_some());
            s.run("edit.undo", &json!({})).unwrap();
            assert_eq!(super::super::suggest::spans(&s.doc).len(), 1);
        }
    }
    #[test]
    fn missing_ambiguous_or_protected_anchors_never_mutate() {
        let mut s = session();
        for value in [
            json!({"anchor":"missing","information":"answer"}),
            json!({"anchor":"Why?","information":""}),
            json!({"anchor":"Why?","information":"answer","kind":"bad"}),
        ] {
            let before = s.doc.clone();
            s.run("study.add", &value).unwrap_err();
            assert_eq!(s.doc, before);
        }
        let a = add(&mut s, "Why?", "Because.");
        // If the source is removed, retain the information but stop attaching it elsewhere.
        s.run("document.setText", &json!({"text":"Other content."})).unwrap();
        assert!(anchor(&s.doc, &notes(&s.doc)[0]).is_none());
        s.run("study.insert", &a).unwrap_err();
        s.doc.settings.protection = Some("readOnly".into());
        s.run("study.keep", &a).unwrap_err();
        s.run("study.add", &json!({"anchor":"Other content.","information":"answer"})).unwrap_err();
    }
}

#[cfg(test)]
mod recheck_tests {
    use super::*;
    #[test]
    fn rechecking_a_dismissed_answer_finishes_without_repeating_forever() {
        for answer in ["First answer.", "A different answer."] {
            let mut s = Session::new(Document::from_text("Why?"));
            let a = s.run("study.add", &json!({"anchor":"Why?","information":"First answer.","kind":"answer"})).unwrap();
            s.run("study.dismiss", &a).unwrap();
            s.run("study.reask", &a).unwrap();
            let c = s.run("suggest.context", &json!({})).unwrap();
            assert_eq!(c["blocks"][0]["recheck"], true);
            assert!(c["blocks"][0]["answered"].as_array().unwrap().is_empty());
            s.run("study.add", &json!({"anchor":"Why?","information":answer,"kind":"answer"})).unwrap();
            assert!(notes(&s.doc).iter().all(|n| n.status != "queued"));
            let c = s.run("suggest.context", &json!({})).unwrap();
            assert_eq!(c["blocks"][0]["answered"][0], "Why?");
        }
    }
}

#[cfg(test)]
mod repeated_anchor_tests {
    use super::*;
    #[test]
    fn the_same_short_question_in_different_paragraphs_keeps_two_answers() {
        let mut s = Session::new(Document::from_text("Why?\nWhy?"));
        for block in [0, 1] {
            s.run("study.add", &json!({"anchor":"Why?","information":"An answer.","kind":"answer","block":block})).unwrap();
        }
        let all = notes(&s.doc);
        assert_eq!(all.len(), 2);
        assert_eq!(anchor(&s.doc, &all[0]).unwrap().0.path, Path::top(0));
        assert_eq!(anchor(&s.doc, &all[1]).unwrap().0.path, Path::top(1));
        let c = s.run("suggest.context", &json!({})).unwrap();
        assert_eq!(c["blocks"][0]["answered"][0], "Why?");
        assert_eq!(c["blocks"][1]["answered"][0], "Why?");
    }
}

#[cfg(test)]
mod tracked_anchor_tests {
    use super::*;
    #[test]
    fn information_for_a_question_with_pending_inline_markup_is_not_repeated() {
        let mut s = Session::new(Document::from_text("Why is it red?"));
        s.run("suggest.add", &json!({"find":"red","replace":"green"})).unwrap();
        s.run("study.add", &json!({"anchor":"Why is it green?","information":"An answer.","kind":"answer","block":0})).unwrap();
        let c = s.run("suggest.context", &json!({})).unwrap();
        assert_eq!(c["blocks"][0]["answered"][0], "Why is it green?");
        assert_eq!(notes(&s.doc).len(), 1);
    }
}
