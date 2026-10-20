//! When to ask for suggestions, and on which paragraph: after the writer pauses, on a paragraph
//! near the caret whose text hasn't been looked at yet.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

use serde_json::Value;

/// One top-level body paragraph, as `suggest.context` reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub index: usize,
    pub text: String,
    pub style: Option<String>,
    /// Suggestions already waiting in it.
    pub suggestions: usize,
    pub answered: Vec<String>,
    pub recheck: bool,
}

/// The document as the companion sees it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    pub document_id: Option<u64>,
    pub study_supported: bool,
    pub blocks: Vec<Block>,
    /// The body paragraph the caret is in.
    pub caret: Option<usize>,
}

impl Context {
    pub fn from_json(v: &Value) -> Context {
        let blocks = v
            .get("blocks")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|b| {
                        Some(Block {
                            index: usize::try_from(b.get("index")?.as_u64()?).ok()?,
                            text: b.get("text")?.as_str()?.to_string(),
                            style: b.get("style").and_then(Value::as_str).map(str::to_string),
                            recheck: b.get("recheck").and_then(Value::as_bool).unwrap_or(false),
                            answered: b
                                .get("answered")
                                .and_then(Value::as_array)
                                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                                .unwrap_or_default(),
                            suggestions: b.get("suggestions").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).unwrap_or(0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let caret = v.get("caret").and_then(|c| c.get("block")).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok());
        Context {
            blocks,
            caret,
            document_id: v.get("documentId").and_then(Value::as_u64),
            study_supported: v.get("study").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    pub fn block(&self, index: usize) -> Option<&Block> {
        self.blocks.iter().find(|b| b.index == index)
    }
}

pub fn text_hash(t: &str) -> u64 {
    let mut h = DefaultHasher::new();
    t.trim().hash(&mut h);
    h.finish()
}

/// Headings and titles get no suggestions.
fn is_heading(b: &Block) -> bool {
    b.style.as_deref().is_some_and(|s| {
        let s = s.to_ascii_lowercase();
        s.starts_with("heading") || s.starts_with("title") || s.starts_with("subtitle")
    })
}

pub struct Settings {
    pub study: bool,
    /// How long the document must stay unchanged before asking.
    pub idle: Duration,
    /// Paragraphs shorter than this (in words) are left alone.
    pub min_words: usize,
    /// How far from the caret (in paragraphs) to look; `None`: anywhere.
    pub reach: Option<usize>,
}

pub struct Watch {
    document_id: Option<u64>,
    settings: Settings,
    /// Paragraph texts already looked at (or written by the companion).
    reviewed: HashSet<u64>,
    initial: HashSet<u64>,
    /// The texts at the last look, by paragraph index.
    last: HashMap<usize, u64>,
    last_change: Option<Instant>,
}

impl Watch {
    pub fn new(settings: Settings) -> Watch {
        Watch { document_id: None, settings, reviewed: HashSet::new(), initial: HashSet::new(), last: HashMap::new(), last_change: None }
    }

    /// Count every paragraph as looked at (on start, unless existing text should be reviewed).
    pub fn mark_all(&mut self, ctx: &Context) {
        self.document_id = ctx.document_id;
        for b in &ctx.blocks {
            self.reviewed.insert(text_hash(&b.text));
            self.initial.insert(text_hash(&b.text));
        }
    }

    pub fn mark(&mut self, text: &str) {
        self.reviewed.insert(text_hash(text));
    }

    pub fn forget(&mut self, text: &str) {
        self.reviewed.remove(&text_hash(text));
    }

    /// Take a new look at the document. Returns the paragraph to ask about, if it's time.
    pub fn observe(&mut self, ctx: &Context, now: Instant) -> Option<usize> {
        if self.document_id.is_some() && self.document_id != ctx.document_id {
            self.reviewed.clear();
            self.initial.clear();
            self.last.clear();
            self.last_change = None;
        }
        self.document_id = ctx.document_id;
        let current: HashMap<usize, u64> = ctx.blocks.iter().map(|b| (b.index, text_hash(&b.text))).collect();
        if current != self.last {
            self.last = current;
            self.last_change = Some(now);
            return None;
        }
        let quiet = self.last_change.is_none_or(|t| now.duration_since(t) >= self.settings.idle);
        if !quiet {
            return None;
        }
        let caret = ctx.caret.unwrap_or(0);
        let mut candidates: Vec<&Block> = ctx
            .blocks
            .iter()
            .filter(|b| (self.settings.study || b.suggestions == 0) && (!is_heading(b) || !crate::prompt::questions(&b.text).is_empty()))
            .filter(|b| b.text.split_whitespace().count() >= self.settings.min_words || !crate::prompt::questions(&b.text).is_empty())
            .filter(|b| {
                !self.reviewed.contains(&text_hash(&b.text))
                    || (self.settings.study
                        && (!self.initial.contains(&text_hash(&b.text)) || b.recheck)
                        && crate::prompt::questions(&b.text).iter().any(|q| !b.answered.iter().any(|a| a == q)))
            })
            .filter(|b| self.settings.reach.is_none_or(|r| b.index.abs_diff(caret) <= r))
            .collect();
        // The caret's paragraph first, then those just above it (what was just written).
        candidates.sort_by_key(|b| (b.index > caret, b.index.abs_diff(caret)));
        let pick = candidates.first().map(|b| (b.index, b.text.clone()));
        let (index, text) = pick?;
        self.mark(&text);
        Some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(texts: &[&str], caret: usize) -> Context {
        Context {
            blocks: texts
                .iter()
                .enumerate()
                .map(|(i, t)| Block {
                    index: i,
                    text: (*t).to_string(),
                    style: (i == 0).then(|| "Heading1".to_string()),
                    suggestions: 0,
                    answered: vec![],
                    recheck: false,
                })
                .collect(),
            caret: Some(caret),
            ..Default::default()
        }
    }

    const LONG: &str = "Markets are not natural phenomena but things the state builds and keeps up.";

    #[test]
    fn asks_after_a_pause_about_the_caret_paragraph_once() {
        let mut w = Watch::new(Settings { study: false, idle: Duration::from_secs(3), min_words: 5, reach: Some(3) });
        let t0 = Instant::now();
        let c = ctx(&["Title words here and more", LONG, "Short one."], 1);
        assert_eq!(w.observe(&c, t0), None, "a change starts the wait");
        assert_eq!(w.observe(&c, t0 + Duration::from_secs(1)), None, "not quiet yet");
        assert_eq!(w.observe(&c, t0 + Duration::from_secs(4)), Some(1));
        assert_eq!(w.observe(&c, t0 + Duration::from_secs(8)), None, "already looked at; headings and short ones never");
    }

    #[test]
    fn typing_restarts_the_wait_and_existing_text_can_be_skipped() {
        let mut w = Watch::new(Settings { study: false, idle: Duration::from_secs(3), min_words: 5, reach: None });
        let t0 = Instant::now();
        let c = ctx(&["Heading", LONG], 1);
        w.mark_all(&c);
        assert_eq!(w.observe(&c, t0), None);
        assert_eq!(w.observe(&c, t0 + Duration::from_secs(5)), None, "existing text was marked");
        let longer = format!("{LONG} And it keeps them.");
        let c2 = ctx(&["Heading", &longer], 1);
        assert_eq!(w.observe(&c2, t0 + Duration::from_secs(6)), None);
        assert_eq!(w.observe(&c2, t0 + Duration::from_secs(10)), Some(1));
    }

    #[test]
    fn reads_suggest_context() {
        let v = serde_json::json!({"blocks": [{"index": 2, "text": "x", "style": null, "suggestions": 1}], "caret": {"block": 2, "off": 0}});
        let c = Context::from_json(&v);
        assert_eq!(c.caret, Some(2));
        assert_eq!(c.block(2).map(|b| b.suggestions), Some(1));
    }
}

#[cfg(test)]
mod rejection_tests {
    use super::*;
    #[test]
    fn dismissing_one_answer_does_not_stall_the_other_question() {
        let mut w = Watch::new(Settings { study: true, idle: Duration::from_secs(1), min_words: 8, reach: None });
        let now = Instant::now();
        let mut c = Context::from_json(
            &serde_json::json!({"blocks":[{"index":0,"text":"Why? What?","suggestions":1,"answered":["Why?"]}],"caret":{"block":0}}),
        );
        assert_eq!(w.observe(&c, now), None);
        w.mark("Why? What?"); // This text was already reviewed before rejection.
        assert_eq!(w.observe(&c, now + Duration::from_secs(2)), Some(0));
        c.blocks[0].answered.push("What?".into());
        assert_eq!(w.observe(&c, now + Duration::from_secs(3)), None, "do not keep repeating dismissed answers");
        c.blocks[0].answered.pop();
        assert_eq!(w.observe(&c, now + Duration::from_secs(4)), Some(0), "an explicit recheck is eligible");
    }
}

#[cfg(test)]
mod document_tests {
    use super::*;
    #[test]
    fn opening_another_document_does_not_inherit_reviewed_state() {
        let mut w = Watch::new(Settings { study: true, idle: Duration::from_secs(1), min_words: 8, reach: None });
        let mut c = Context::from_json(&serde_json::json!({"documentId":1,"blocks":[{"index":0,"text":"Why?"}],"caret":{"block":0}}));
        w.mark_all(&c);
        let now = Instant::now();
        w.observe(&c, now);
        assert_eq!(w.observe(&c, now + Duration::from_secs(2)), None);
        c.document_id = Some(2);
        assert_eq!(w.observe(&c, now + Duration::from_secs(3)), None);
        assert_eq!(w.observe(&c, now + Duration::from_secs(5)), Some(0));
    }
}
