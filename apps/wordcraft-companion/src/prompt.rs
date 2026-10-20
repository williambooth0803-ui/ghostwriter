//! What the writing assistant is asked, and reading its answer.

use serde_json::Value;

use crate::watch::Context;

/// The instructions; the brief, style guide and paragraphs follow.
const INSTRUCTIONS: &str = r#"You are a writing companion inside the writer's word processor. The writer is drafting a long-form essay and keeps typing while you work. Propose at most MAX small, high-value edits to the TARGET paragraph only. Each appears inline in the document, highlighted, and the writer accepts or rejects it with one click, so it must read naturally in place.

Good suggestions: a transition the paragraph needs, a tighter phrasing of a wordy sentence, a more precise word, a missing step in the argument, a grammar or punctuation slip. Keep the writer's voice and register; never make it sound generic or add hedging. If the paragraph is fine, return no suggestions: silence is better than noise.

Rules:
- "find" is copied exactly, character for character, from the TARGET paragraph: a phrase or one sentence, unique within it. "replace" is what it becomes.
- For a pure addition use "after" (exact text from the TARGET paragraph) and "insert" (the new text, with its leading space).
- No line breaks. Suggestions must not overlap.
- CARET_RULE
- "why" is a short reason (under 12 words).

Reply with JSON only, no prose and no code fence:
{"suggestions": [{"find": "...", "replace": "...", "why": "..."}, {"after": "...", "insert": "...", "why": "..."}]}"#;

pub struct Material<'a> {
    pub brief: Option<&'a str>,
    pub style: Option<&'a str>,
    pub max: usize,
}

/// The prompt for paragraph `target` of `ctx`.
pub fn build(ctx: &Context, target: usize, m: &Material) -> Option<String> {
    let t = ctx.block(target)?;
    let finished = t.text.trim_end().ends_with(['.', '!', '?', '"', '”', '’', ')', ':']);
    let caret_rule = if ctx.caret == Some(target) && !finished {
        "The writer's caret is in this paragraph and they may still be writing its last sentence: leave that sentence alone."
    } else {
        "The writer has moved on from this paragraph."
    };
    let mut p = INSTRUCTIONS.replace("MAX", &m.max.to_string()).replace("CARET_RULE", caret_rule);
    if let Some(b) = m.brief.filter(|b| !b.trim().is_empty()) {
        p.push_str("\n\n## The writer's brief for this piece\n");
        p.push_str(b.trim());
    }
    if let Some(s) = m.style.filter(|s| !s.trim().is_empty()) {
        p.push_str("\n\n## The writer's style\n");
        p.push_str(s.trim());
    }
    let outline: Vec<&str> = ctx
        .blocks
        .iter()
        .filter(|b| b.style.as_deref().is_some_and(|s| s.to_ascii_lowercase().starts_with("heading")))
        .map(|b| b.text.as_str())
        .collect();
    if !outline.is_empty() {
        p.push_str("\n\n## Outline so far\n");
        for h in outline {
            p.push_str("- ");
            p.push_str(h);
            p.push('\n');
        }
    }
    let pos = ctx.blocks.iter().position(|b| b.index == target)?;
    let before: Vec<&str> =
        ctx.blocks.get(pos.saturating_sub(3)..pos).unwrap_or_default().iter().map(|b| b.text.as_str()).filter(|t| !t.is_empty()).collect();
    if !before.is_empty() {
        p.push_str("\n\n## Paragraphs before (context only, don't edit)\n");
        p.push_str(&before.join("\n\n"));
    }
    p.push_str("\n\n## TARGET paragraph\n");
    p.push_str(&t.text);
    if let Some(next) = ctx.blocks.get(pos + 1).filter(|b| !b.text.is_empty()) {
        p.push_str("\n\n## Paragraph after (context only, don't edit)\n");
        p.push_str(&next.text);
    }
    Some(p)
}

/// One edit the assistant proposes: `suggest.add` parameters, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub params: Value,
    pub why: String,
}

/// Read Claude's reply: the output of `claude -p --output-format json` (or its bare text).
pub fn parse(stdout: &str) -> Result<Vec<Proposal>, String> {
    let outer: Option<Value> = serde_json::from_str(stdout.trim()).ok();
    if let Some(o) = &outer
        && o.get("is_error").and_then(Value::as_bool) == Some(true)
    {
        return Err(format!("claude: {}", o.get("result").and_then(Value::as_str).unwrap_or("error")));
    }
    let text = outer.as_ref().and_then(|o| o.get("result")).and_then(Value::as_str).unwrap_or(stdout);
    let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) else { return Err(format!("no JSON in the reply: {}", short(text))) };
    let inner: Value =
        serde_json::from_str(text.get(a..=b).unwrap_or_default()).map_err(|e| format!("bad JSON in the reply ({e}): {}", short(text)))?;
    let list = inner.get("suggestions").and_then(Value::as_array).cloned().unwrap_or_default();
    Ok(list
        .into_iter()
        .filter_map(|s| {
            let str_of = |k: &str| s.get(k).and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string);
            let why = str_of("why").unwrap_or_default();
            let params = match (str_of("find"), str_of("replace"), str_of("after"), str_of("insert")) {
                (Some(f), Some(r), _, _) if f != r => serde_json::json!({"find": f, "replace": r}),
                (_, _, Some(a), Some(i)) => serde_json::json!({"after": a, "insert": i}),
                _ => return None,
            };
            Some(Proposal { params, why })
        })
        .collect())
}

fn short(t: &str) -> String {
    t.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::Block;

    #[test]
    fn reads_the_reply_inside_claude_json() {
        let reply = serde_json::json!({"type": "result", "is_error": false, "result":
            "```json\n{\"suggestions\": [{\"find\": \"builds\", \"replace\": \"constructs\", \"why\": \"precise\"}, {\"after\": \"them.\", \"insert\": \" Always.\"}, {\"find\": \"same\", \"replace\": \"same\"}, {\"bogus\": 1}]}\n```"});
        let got = parse(&reply.to_string()).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].params, serde_json::json!({"find": "builds", "replace": "constructs"}));
        assert_eq!(got[0].why, "precise");
        assert_eq!(got[1].params, serde_json::json!({"after": "them.", "insert": " Always."}));
        assert!(parse(r#"{"is_error": true, "result": "limit reached"}"#).is_err());
        assert!(parse("nothing here").is_err());
        assert_eq!(parse(r#"{"suggestions": []}"#).unwrap(), vec![]);
    }

    #[test]
    fn the_prompt_holds_the_target_brief_and_context() {
        let b = |i: usize, t: &str, s: Option<&str>| Block { index: i, text: t.into(), style: s.map(str::to_string), suggestions: 0 };
        let ctx = Context {
            blocks: vec![b(0, "Rents", Some("Heading1")), b(1, "First para.", None), b(2, "Target para.", None), b(3, "Next.", None)],
            caret: Some(2),
        };
        let p = build(&ctx, 2, &Material { brief: Some("Argue that rent is a tax."), style: None, max: 3 }).unwrap();
        assert!(p.contains("## TARGET paragraph\nTarget para."));
        assert!(p.contains("Argue that rent is a tax."));
        assert!(p.contains("- Rents"));
        assert!(p.contains("First para."));
        assert!(!p.contains("leave that sentence alone"), "the paragraph ends in a full stop");
        let mut unfinished = ctx.clone();
        if let Some(b) = unfinished.blocks.get_mut(2) {
            b.text = "Target para and".into();
        }
        assert!(build(&unfinished, 2, &Material { brief: None, style: None, max: 3 }).unwrap().contains("leave that sentence alone"));
        assert!(p.contains("at most 3"));
        assert!(build(&ctx, 9, &Material { brief: None, style: None, max: 3 }).is_none());
    }
}
