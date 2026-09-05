//! The record of what was said, and how to render it for a provider.
//!
//! One structured record, rendered on demand. Holding the conversation
//! provider-shaped — speaker attribution baked into a string — would leave
//! nothing to render a *different* way, which is exactly what a mixed-provider
//! session needs.
//!
//! [`Turn`] therefore keeps the speaker as a field rather than a string
//! prefix, and [`Conversation::render`] puts it back. The public transcript is
//! kept alongside it here too, so the two cannot fall out of step.

use serde::{Deserialize, Serialize};

/// A message shaped the way a provider's chat API expects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage {
            role: role.into(),
            content: content.into(),
        }
    }
}

/// A single message in the public session transcript.
///
/// Part of the public API — returned in `SessionResult::history`. It lives
/// here rather than in the session because [`Conversation`] builds it and the
/// session depends on the conversation, not the other way around.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub sender: String,
    pub content: String,
    pub round_idx: i64,
    /// Transcript category: `turn`, `orchestrator`, `summary`, `system`, or
    /// `final_summary`.
    pub msg_type: String,
}

impl Message {
    pub fn new(sender: impl Into<String>, content: impl Into<String>) -> Self {
        Message {
            sender: sender.into(),
            content: content.into(),
            round_idx: 0,
            msg_type: "turn".into(),
        }
    }
}

/// One entry in the conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    /// `"user"` for a directive addressed to the next speaker, `"assistant"`
    /// for something an agent said.
    pub role: String,
    /// The agent who said it; empty for a directive.
    pub speaker: String,
    /// The text, without any speaker prefix.
    pub content: String,
    /// Turn number at the time it was recorded.
    pub round_idx: i64,
    /// Transcript category; see [`Message`].
    pub msg_type: String,
}

impl Turn {
    pub fn new(
        role: impl Into<String>,
        speaker: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Turn {
            role: role.into(),
            speaker: speaker.into(),
            content: content.into(),
            round_idx: 0,
            msg_type: "turn".into(),
        }
    }

    /// Render this turn for a provider.
    ///
    /// An assistant turn regains its `[Speaker]` prefix. A directive is passed
    /// through unchanged.
    pub fn render(&self) -> ChatMessage {
        if self.speaker.is_empty() {
            ChatMessage::new(self.role.clone(), self.content.clone())
        } else {
            ChatMessage::new(
                self.role.clone(),
                format!("[{}] {}", self.speaker, self.content),
            )
        }
    }
}

/// The turns of one session, plus the transcript returned to the caller.
///
/// Two records are kept because they are not the same thing. Directives the
/// session injects (the topic, a retry hint, the closing summary request) are
/// part of what the model reads but are not something an agent *said*, so they
/// never reach the transcript. System notices are the mirror image: reported
/// to the caller, never shown to a model.
#[derive(Clone, Debug, Default)]
pub struct Conversation {
    turns: Vec<Turn>,
    transcript: Vec<Message>,
}

impl Conversation {
    pub fn new() -> Self {
        Conversation::default()
    }

    /// Record a user-role instruction that no agent authored.
    pub fn directive(&mut self, content: impl Into<String>) {
        self.raw("user", content);
    }

    /// Record an already-rendered message verbatim.
    ///
    /// Used for tool exchanges when the session is configured to keep them in
    /// the shared conversation — their `[Tool:name]` prefix is part of the
    /// content, not a speaker.
    pub fn raw(&mut self, role: impl Into<String>, content: impl Into<String>) {
        self.turns.push(Turn::new(role, "", content));
    }

    /// Record what an agent said, in both the conversation and transcript.
    pub fn say(&mut self, speaker: &str, content: &str, round_idx: i64, msg_type: &str) {
        self.turns.push(Turn {
            role: "assistant".into(),
            speaker: speaker.into(),
            content: content.into(),
            round_idx,
            msg_type: msg_type.into(),
        });
        self.transcript.push(Message {
            sender: speaker.into(),
            content: content.into(),
            round_idx,
            msg_type: msg_type.into(),
        });
    }

    /// Record a system notice for the caller only; models never see it.
    pub fn note(&mut self, content: impl Into<String>) {
        self.transcript.push(Message {
            sender: "system".into(),
            content: content.into(),
            round_idx: 0,
            msg_type: "system".into(),
        });
    }

    /// Render every turn as provider-shaped messages.
    pub fn render(&self) -> Vec<ChatMessage> {
        self.turns.iter().map(Turn::render).collect()
    }

    /// The turns themselves, oldest first.
    ///
    /// [`Conversation::render`] folds speaker into content and drops `msg_type`
    /// and `round_idx`, so persistence and compaction need the structured form.
    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    /// Swap the conversation for *turns*, leaving the transcript alone.
    ///
    /// This is compaction's write-back. The transcript is deliberately
    /// untouched: it is never sent to a model, so it is not what overflows a
    /// context window, and shrinking it would mean a caller who asked for a
    /// smaller prompt silently got a shorter report.
    pub fn replace_turns(&mut self, turns: Vec<Turn>) {
        self.turns = turns;
    }

    /// Seed both records from a saved run.
    pub fn restore(&mut self, turns: Vec<Turn>, transcript: Vec<Message>) {
        self.turns = turns;
        self.transcript = transcript;
    }

    /// The public transcript.
    pub fn transcript(&self) -> &[Message] {
        &self.transcript
    }

    pub fn len(&self) -> usize {
        self.turns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_turn_regains_its_speaker_prefix() {
        let turn = Turn::new("assistant", "Alice", "hello");
        assert_eq!(
            turn.render(),
            ChatMessage::new("assistant", "[Alice] hello")
        );
    }

    #[test]
    fn a_directive_is_rendered_unchanged() {
        let turn = Turn::new("user", "", "the topic");
        assert_eq!(turn.render(), ChatMessage::new("user", "the topic"));
    }

    #[test]
    fn directives_never_reach_the_transcript_and_notes_never_reach_the_model() {
        let mut conversation = Conversation::new();
        conversation.directive("discuss this");
        conversation.say("Alice", "my view", 1, "turn");
        conversation.note("Resumed from ./run.json");

        assert_eq!(conversation.len(), 2, "the note is not a turn");
        assert_eq!(
            conversation.transcript().len(),
            2,
            "the directive is not said"
        );
        assert_eq!(conversation.transcript()[0].sender, "Alice");
        assert_eq!(conversation.transcript()[1].msg_type, "system");
    }

    #[test]
    fn replacing_turns_leaves_the_transcript_whole() {
        let mut conversation = Conversation::new();
        conversation.say("Alice", "one", 1, "turn");
        conversation.say("Bob", "two", 2, "turn");
        conversation.replace_turns(vec![Turn::new("user", "", "summary")]);

        assert_eq!(conversation.len(), 1);
        assert_eq!(conversation.transcript().len(), 2);
    }
}
