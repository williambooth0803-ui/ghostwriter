//! Companion suggestions (fork addition, see `docs/companion.md`): an edit an assistant proposes
//! is kept in the text as a pair of tracked changes by [`AUTHOR`], the words it replaces as a
//! deletion and its own words as an insertion right after them. The layout leaves the replaced
//! words out, so the paragraph reads as it would once accepted; the app draws the new words in
//! orange until the writer accepts or rejects them.

use crate::Document;
use crate::props::Rgb;

/// Default author, retained for existing Claude clients and saved documents.
pub const AUTHOR: &str = "Claude (suggestion)";
/// Codex suggestions use the same layout and accept/reject handling as Claude suggestions.
pub const CODEX_AUTHOR: &str = "Codex (suggestion)";

/// The orange behind a suggestion's words.
pub const FILL: Rgb = Rgb(0xFF, 0xC8, 0x8A);

/// Whether revision `rev` (`None`: no revision) is part of a suggestion.
pub fn is_suggestion(doc: &Document, rev: Option<u32>) -> bool {
    rev.and_then(|r| doc.revisions.get(r as usize)).is_some_and(|r| r.author == AUTHOR || r.author == CODEX_AUTHOR)
}
