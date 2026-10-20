//! How an MCP client should use Ghostwriter's suggestions (fork addition, see
//! `docs/companion.md`): appended to the server's `instructions`.

/// Sent to every client in `initialize`.
pub const INSTRUCTIONS: &str = "\
Ghostwriter suggestions: while the writer has the document open, propose edits as suggestions \
they accept (✓) or reject (✗) instead of changing the text yourself, unless they ask you to edit \
directly. Codex clients: add provider:\"codex\" to suggest.add params for Codex revision attribution. Flow: (1) execute suggest.context → {blocks:[{index,text,style,suggestions}], \
caret:{block,off}}: the body paragraphs as they read with pending suggestions in, and where the \
writer is. (2) execute suggest.add {find:\"exact words\", replace:\"new words\"}, or \
{after|before:\"exact words\", insert:\" added words\"} (mind the spaces). Quote the writer's own \
words exactly as they stand, within one paragraph, never words that are already a pending \
suggestion; quote a whole phrase or sentence when that reads better: only the words that change \
are marked. No line breaks. (3) On an error: \"appears N times\" → quote more words or pass \
occurrence (1-based); \"isn't in the body\" → the writer changed it, run suggest.context again; \
\"the writer's caret is there\" → skip it, they're typing there. (4) suggest.list → \
[{id,block,start,end,text,replaces}]; suggest.withdraw {id} or {all:true} takes back yours that \
no longer fit. Leave suggest.accept and suggest.reject to the writer unless asked. Keep it to a \
few suggestions per paragraph, each one worth the writer's attention, and don't redo one the \
writer rejected.";

#[cfg(test)]
mod tests {
    #[test]
    fn names_only_real_commands() {
        let session = wordcraft_engine::Session::new(wordcraft_doc::Document::new());
        let ids: Vec<&str> = session.registry.all().iter().map(|c| c.id).collect();
        let named = super::INSTRUCTIONS.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.')).filter(|w| w.starts_with("suggest."));
        let mut n = 0;
        for id in named {
            assert!(ids.contains(&id), "{id} is not a command");
            n += 1;
        }
        assert!(n >= 6, "the instructions name the suggest commands");
    }
}
