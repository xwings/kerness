//! Keeping the conversation inside the model's context window.
//!
//! The conversation grows one turn at a time and is re-rendered in full for
//! every provider call, so something has to bound it: a provider's
//! `max_tokens` caps *output* only, and an unbounded conversation fails at the
//! API with a context-length error deep into a run the caller has already paid
//! for.
//!
//! The limit this module works against is not an allowance the framework
//! invents. It stands for what the model can physically hold, which is why the
//! session subtracts everything else in the request — system prompt, skills,
//! tools, memory — before saying how much of it the conversation may use. This
//! module receives the remainder and never sees the ceiling itself.
//!
//! [`compact`] trades the oldest turns for a summary of them. The topic
//! directive and the most recent turns survive verbatim; everything between is
//! handed to a summarizer and comes back as one directive. This module knows
//! nothing about who summarizes or where the result is stored.
//!
//! Counting is a character heuristic, not a tokenizer. The framework speaks to
//! three provider families with three different tokenizers, so an exact count
//! would mean either a new dependency or a number that is only right for one
//! provider. The limit is a compaction trigger, not a billing figure, and
//! every name here says `estimate`.

use crate::conversation::{ChatMessage, Turn};

/// Characters per token.
///
/// Roughly right for English prose across the major tokenizers, and wrong in
/// the same direction for everyone — it under-counts code and non-English
/// text, so a limit built on it is optimistic rather than dangerous.
pub const CHARS_PER_TOKEN: usize = 4;

/// Compaction targets this fraction of the limit rather than the limit itself.
///
/// Compacting to exactly the limit means the very next turn breaches it again,
/// and a session that compacts every turn pays for a summary every turn while
/// losing history each time.
pub const COMPACT_TO_FRACTION: f64 = 0.5;

/// Prefix on the directive that replaces the dropped turns.
///
/// Labelled so that neither a model nor a caller reading the transcript
/// mistakes a framework-written recap for something an agent actually said.
pub const SUMMARY_PREFIX: &str = "Summary of earlier discussion:";

/// Instruction handed to the summarizer along with the dropped turns.
pub const SUMMARY_PROMPT: &str = concat!(
    "The discussion below is being dropped from the working context to stay ",
    "within a token budget. Write a compact summary that preserves what later ",
    "turns will need: the positions each speaker took, the reasons they gave, ",
    "what was agreed, and what is still open. Attribute positions by name. ",
    "Write only the summary."
);

/// Estimate the tokens *text* costs a provider.
///
/// An estimate, deliberately — see the module documentation. Counted in
/// characters rather than bytes, so a non-ASCII transcript is not charged
/// twice for the same prose.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / CHARS_PER_TOKEN
}

/// Estimate the tokens a rendered conversation costs.
///
/// Counts the *rendered* form, because that is what reaches the provider — an
/// assistant turn regains its `[Speaker]` prefix on the way out.
pub fn estimate_turns(turns: &[Turn]) -> usize {
    turns
        .iter()
        .map(|turn| estimate_tokens(&turn.render().content))
        .sum()
}

/// Return *turns* shrunk under *limit*, or `None` to leave it alone.
///
/// `None` covers every case where compaction is not the answer: the
/// conversation already fits, there is nothing to drop, or the summarizer came
/// back empty. That last one matters — a failed provider call must leave the
/// conversation intact rather than trading real turns for nothing.
///
/// *limit* is what the conversation may use, not the context window: the
/// session has already subtracted the rest of the request from it.
pub fn compact<F>(turns: &[Turn], limit: usize, summarize: F) -> Option<Vec<Turn>>
where
    F: FnOnce(&[Turn]) -> String,
{
    if estimate_turns(turns) <= limit {
        return None;
    }
    if turns.len() < 2 {
        // One turn over the limit on its own. There is nothing to summarize
        // that would not be the whole conversation.
        return None;
    }

    // The first turn is the topic. Every later turn is a reply to it, so
    // dropping it would leave the summary and the recent turns discussing
    // something the model can no longer see.
    let (anchor, rest) = turns.split_first().expect("length checked above");
    let target = ((limit as f64) * COMPACT_TO_FRACTION) as isize
        - estimate_turns(std::slice::from_ref(anchor)) as isize;

    let mut keep_from = rest.len();
    let mut used: isize = 0;
    for (index, turn) in rest.iter().enumerate().rev() {
        let cost = estimate_turns(std::slice::from_ref(turn)) as isize;
        // The `keep_from < rest.len()` guard keeps the most recent turn
        // unconditionally: a single turn larger than the whole target would
        // otherwise compact to nothing but a summary, leaving the next speaker
        // with no immediate context.
        if keep_from < rest.len() && used + cost > target {
            break;
        }
        keep_from = index;
        used += cost;
    }

    let dropped = &rest[..keep_from];
    if dropped.is_empty() {
        return None;
    }

    let summary = summarize(dropped);
    let summary = summary.trim();
    if summary.is_empty() {
        return None;
    }

    let mut compacted = Vec::with_capacity(2 + rest.len() - keep_from);
    compacted.push(anchor.clone());
    compacted.push(Turn::new(
        "user",
        "",
        format!("{SUMMARY_PREFIX}\n{summary}"),
    ));
    compacted.extend_from_slice(&rest[keep_from..]);
    Some(compacted)
}

/// Build the messages that ask a provider to summarize *turns*.
///
/// A plain message list rather than an agent turn: a summarizer has no persona
/// to keep, no skills to load, and no business making tool calls.
pub fn summary_request(turns: &[Turn]) -> Vec<ChatMessage> {
    let rendered = turns
        .iter()
        .map(|turn| turn.render().content)
        .collect::<Vec<_>>()
        .join("\n");
    vec![
        ChatMessage::new("system", SUMMARY_PROMPT),
        ChatMessage::new("user", rendered),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(speaker: &str, content: &str) -> Turn {
        Turn::new("assistant", speaker, content)
    }

    #[test]
    fn a_conversation_that_fits_is_left_alone() {
        let turns = vec![Turn::new("user", "", "topic"), said("Alice", "short")];
        assert!(compact(&turns, 1_000, |_| "unused".into()).is_none());
    }

    #[test]
    fn a_single_oversized_turn_is_left_alone() {
        let turns = vec![Turn::new("user", "", "x".repeat(4_000))];
        assert!(compact(&turns, 10, |_| "summary".into()).is_none());
    }

    #[test]
    fn the_topic_and_the_latest_turn_always_survive() {
        let turns = vec![
            Turn::new("user", "", "the topic"),
            said("Alice", &"a".repeat(4_000)),
            said("Bob", &"b".repeat(4_000)),
            said("Carol", &"c".repeat(4_000)),
        ];
        let compacted = compact(&turns, 100, |_| "they disagreed".into()).expect("compacts");

        assert_eq!(compacted[0], turns[0], "the topic anchors the conversation");
        assert_eq!(
            compacted.last().expect("non-empty"),
            turns.last().expect("non-empty"),
            "the most recent turn is never dropped"
        );
        assert_eq!(compacted[1].role, "user");
        assert!(compacted[1].content.starts_with(SUMMARY_PREFIX));
    }

    #[test]
    fn an_empty_summary_leaves_the_conversation_intact() {
        let turns = vec![
            Turn::new("user", "", "the topic"),
            said("Alice", &"a".repeat(4_000)),
            said("Bob", &"b".repeat(4_000)),
        ];
        assert!(
            compact(&turns, 100, |_| "   ".into()).is_none(),
            "a failed summary must not trade real turns for nothing"
        );
    }

    #[test]
    fn estimates_count_the_rendered_form() {
        // "[Alice] " is eight characters the provider is charged for.
        assert_eq!(estimate_turns(&[said("Alice", "12345678")]), 4);
    }

    #[test]
    fn the_summary_request_carries_the_prompt_and_the_dropped_prose() {
        let request = summary_request(&[said("Alice", "one"), said("Bob", "two")]);
        assert_eq!(request[0].role, "system");
        assert_eq!(request[0].content, SUMMARY_PROMPT);
        assert_eq!(request[1].content, "[Alice] one\n[Bob] two");
    }
}
