//! Text protocol parsing and the retry helper.
//!
//! Everything here reads model output looking for a token the framework
//! defined: an `@Name` address, a terminator, an `@MEMORY:` marker. The
//! matching is deliberately boundary-aware rather than substring-based, so
//! `END_SESSION` in prose does not fire on `END_SESSIONS`.

use std::thread::sleep;
use std::time::Duration;

/// Terminators assumed when a caller supplies none. Order is priority order.
pub const DEFAULT_TERMINATORS: [&str; 2] = ["CONSENSUS_REACHED", "END_SESSION"];

/// Whether an ASCII byte belongs to the `[A-Za-z0-9_]` class the protocol
/// tokens are delimited by.
///
/// Bytes outside ASCII are never in the class, which makes byte-wise boundary
/// checks correct on UTF-8 without decoding: a continuation byte is `>= 0x80`.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether a character is a regex `\w`, Unicode-aware — a keyword bounded by
/// accented letters is still bounded.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Return whether *keyword* appears in *text* as a standalone protocol token.
///
/// Case-insensitive, and delimited by `[A-Za-z0-9_]` on both sides. An empty
/// keyword never matches — a harness that declared one would otherwise end on
/// every reply.
pub fn keyword_in_text(text: &str, keyword: &str) -> bool {
    if keyword.is_empty() {
        return false;
    }
    let hay = text.as_bytes();
    let needle = keyword.as_bytes();
    if needle.len() > hay.len() {
        return false;
    }
    for start in 0..=(hay.len() - needle.len()) {
        let end = start + needle.len();
        if !hay[start..end].eq_ignore_ascii_case(needle) {
            continue;
        }
        let before_ok = start == 0 || !is_token_byte(hay[start - 1]);
        let after_ok = end == hay.len() || !is_token_byte(hay[end]);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// Check for session-ending keywords in orchestrator output.
///
/// *keywords* comes from the harness (`loop.terminate_on`). Its order is the
/// priority order: when a reply contains more than one terminator, the one
/// declared first wins. An orchestrator that writes "consensus reached,
/// END_SESSION" means both, and which one the session records is the gameplan
/// author's call rather than an accident of word order.
pub fn parse_session_end<S: AsRef<str>>(text: &str, keywords: &[S]) -> Option<String> {
    keywords
        .iter()
        .find(|k| keyword_in_text(text, k.as_ref()))
        .map(|k| k.as_ref().to_string())
}

/// Parse an orchestrator response for an `@AgentName` mention.
///
/// Names are tried in the order given and the first one *present anywhere* in
/// the text wins, which is what makes routing depend on the roster rather than
/// on which mention landed leftmost.
///
/// Returns `(agent_name, instruction_text)`, where the instruction is
/// everything after the mention with leading `,`, `:` and spaces removed.
pub fn parse_orchestrator_call(text: &str, agent_names: &[String]) -> Option<(String, String)> {
    for name in agent_names {
        if let Some(end) = find_mention(text, name) {
            let instruction = text[end..].trim().trim_start_matches([',', ':', ' ']);
            return Some((name.clone(), instruction.to_string()));
        }
    }
    None
}

/// Byte offset just past the first `@name` in *text* that ends on a word
/// boundary, mirroring the `@{name}\b` pattern.
fn find_mention(text: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let mention = format!("@{name}");
    let last = name.chars().next_back()?;
    let mut from = 0usize;
    while let Some(offset) = text[from..].find(&mention) {
        let end = from + offset + mention.len();
        let next = text[end..].chars().next();
        // `\b` is a transition, so it holds when the character classes of the
        // last matched character and the next one differ.
        let boundary = match next {
            None => is_word_char(last),
            Some(c) => is_word_char(last) != is_word_char(c),
        };
        if boundary {
            return Some(end);
        }
        from = from + offset + 1;
    }
    None
}

/// Extract `@MEMORY:` lines from agent output.
///
/// Lines whose stripped form starts with `@MEMORY:` (case-insensitive) are
/// removed from the text and returned as notes. Markers are an instruction to
/// the framework, not transcript content, so they never reach a channel.
///
/// Returns `(cleaned_text, notes)`.
pub fn parse_memory_markers(text: &str) -> (String, Vec<String>) {
    const MARKER: &str = "@MEMORY:";
    let mut cleaned = String::with_capacity(text.len());
    let mut notes = Vec::new();
    for line in split_lines_keepends(text) {
        let stripped = line.trim();
        if stripped.len() >= MARKER.len()
            && stripped.as_bytes()[..MARKER.len()].eq_ignore_ascii_case(MARKER.as_bytes())
        {
            let note = stripped[MARKER.len()..].trim();
            if !note.is_empty() {
                notes.push(note.to_string());
            }
        } else {
            cleaned.push_str(line);
        }
    }
    (cleaned.trim().to_string(), notes)
}

/// Split *text* into lines, keeping the terminator on each line.
fn split_lines_keepends(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(&text[start..=i]);
                i += 1;
                start = i;
            }
            b'\r' => {
                let end = if bytes.get(i + 1) == Some(&b'\n') { i + 2 } else { i + 1 };
                lines.push(&text[start..end]);
                i = end;
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Retry a fallible call, sleeping between attempts.
///
/// Total attempts are `retries + 1`. Sleeps grow as `backoff_sec * attempt`
/// unless *interval_sec* pins them to a fixed wait. The last error is returned
/// when every attempt fails.
pub fn retry<T, E, F>(
    mut call: F,
    retries: u32,
    backoff_sec: f64,
    interval_sec: Option<f64>,
) -> std::result::Result<T, E>
where
    F: FnMut() -> std::result::Result<T, E>,
{
    let mut last: Option<E> = None;
    for attempt in 0..=retries {
        match call() {
            Ok(value) => return Ok(value),
            Err(err) => {
                last = Some(err);
                if attempt >= retries {
                    break;
                }
                let wait = match interval_sec {
                    Some(fixed) if fixed > 0.0 => fixed,
                    _ => backoff_sec * f64::from(attempt + 1),
                };
                if wait > 0.0 {
                    sleep(Duration::from_secs_f64(wait));
                }
            }
        }
    }
    Err(last.expect("retry always runs at least one attempt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_matches_only_as_a_standalone_token() {
        assert!(keyword_in_text("we are done. END_SESSION", "END_SESSION"));
        assert!(keyword_in_text("end_session now", "END_SESSION"));
        assert!(!keyword_in_text("END_SESSIONS", "END_SESSION"));
        assert!(!keyword_in_text("XEND_SESSION", "END_SESSION"));
        assert!(keyword_in_text("(END_SESSION)", "END_SESSION"));
        assert!(!keyword_in_text("anything", ""));
    }

    #[test]
    fn terminator_priority_follows_declaration_order() {
        let keywords = ["CONSENSUS_REACHED".to_string(), "END_SESSION".to_string()];
        let text = "END_SESSION and CONSENSUS_REACHED";
        assert_eq!(
            parse_session_end(text, &keywords).as_deref(),
            Some("CONSENSUS_REACHED")
        );
    }

    #[test]
    fn mention_parsing_strips_leading_punctuation() {
        let names = vec!["Alice".to_string(), "Bob".to_string()];
        assert_eq!(
            parse_orchestrator_call("@Alice, make your case", &names),
            Some(("Alice".to_string(), "make your case".to_string()))
        );
        assert_eq!(parse_orchestrator_call("@Alicia speaks", &names), None);
        assert_eq!(parse_orchestrator_call("nobody named", &names), None);
    }

    #[test]
    fn memory_markers_are_stripped_and_collected() {
        let (cleaned, notes) = parse_memory_markers("hello\n@MEMORY: remember this\nworld\n");
        assert_eq!(cleaned, "hello\nworld");
        assert_eq!(notes, vec!["remember this".to_string()]);

        let (cleaned, notes) = parse_memory_markers("  @memory:   lower case  ");
        assert_eq!(cleaned, "");
        assert_eq!(notes, vec!["lower case".to_string()]);

        let (_, notes) = parse_memory_markers("@MEMORY:\n");
        assert!(notes.is_empty(), "an empty marker records nothing");
    }

    #[test]
    fn retry_returns_the_last_error_after_exhausting_attempts() {
        let mut attempts = 0;
        let result: std::result::Result<(), &str> = retry(
            || {
                attempts += 1;
                Err("nope")
            },
            2,
            0.0,
            None,
        );
        assert_eq!(result, Err("nope"));
        assert_eq!(attempts, 3, "retries + 1 total attempts");
    }
}
