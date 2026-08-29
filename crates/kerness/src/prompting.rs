//! System prompt assembly.
//!
//! One place that decides what goes into an agent's system prompt and in what
//! order. Orchestrators and participants start from different base prompts,
//! then both receive their agent decoration, turn-local skills, available
//! tools, and memory here, which is what keeps the two prompt paths from
//! drifting apart.

use serde_json::{json, Value};

use crate::agent::Agent;
use crate::error::Result;
use crate::tooling::{format_tools_prompt, ToolSpec};
use crate::toolschema::ToolDialect;

pub const MEMORY_HEADER: &str = "\n\n## Memory\nThe following is shared memory for this session.";

/// Appended to the header only when the session actually writes memory. A
/// read-only session that still invited `@MEMORY:` lines would be asking for
/// notes it then discards.
pub const MEMORY_WRITE_HINT: &str = " You can add notes with the `write_memory` tool, \
or by including lines starting with `@MEMORY:` in your response.";

/// Render an agent's memory, or an empty string when it has none.
///
/// Takes the content rather than the [`Memory`](crate::memory::Memory) it came
/// from: every caller already holds the text, and a session that keeps its
/// memories behind a lock cannot hand out a borrow of one.
pub fn memory_block(content: &str, writable: bool) -> String {
    let content = content.trim();
    if content.is_empty() {
        return String::new();
    }
    let hint = if writable { MEMORY_WRITE_HINT } else { "" };
    format!("{MEMORY_HEADER}{hint}\n\n{content}")
}

/// Something the session looks up per agent: its skills block, its memory.
type AgentText<'a> = Box<dyn Fn(&Agent) -> String + 'a>;

/// The tool dialect an agent's provider speaks.
type AgentDialect<'a> = Box<dyn Fn(&Agent) -> ToolDialect + 'a>;

/// Builds system prompts and message lists for a session's agents.
///
/// The assembler holds no state of its own; it reads through callables the
/// session supplies so that per-agent skills, memory, and the harness-narrowed
/// tool set stay owned by the session. It borrows them, so a session builds one
/// per call rather than caching it — skills, memory, and the permitted tool set
/// are all resolved during the run, and an assembler built up front would close
/// over the wrong ones.
pub struct PromptAssembler<'a> {
    skills_for: AgentText<'a>,
    memory_for: AgentText<'a>,
    tools_for: Box<dyn Fn() -> Vec<ToolSpec> + 'a>,
    dialect_for: Option<AgentDialect<'a>>,
    show_reasoning: Option<bool>,
    memory_writable: bool,
}

impl<'a> PromptAssembler<'a> {
    /// Bind an assembler to the session state it reads through.
    ///
    /// * `skills_for` — the formatted skills block for an agent.
    /// * `memory_for` — the memory content an agent reads.
    /// * `tools_for` — the tools currently permitted.
    /// * `show_reasoning` — reasoning preference passed to participants.
    ///
    /// Every agent is assumed to speak [`ToolDialect::Text`] and no memory is
    /// written back until [`PromptAssembler::with_dialect`] and
    /// [`PromptAssembler::with_memory_writable`] say otherwise, which is what a
    /// caller with no providers in hand should see.
    pub fn new(
        skills_for: impl Fn(&Agent) -> String + 'a,
        memory_for: impl Fn(&Agent) -> String + 'a,
        tools_for: impl Fn() -> Vec<ToolSpec> + 'a,
        show_reasoning: Option<bool>,
    ) -> Self {
        PromptAssembler {
            skills_for: Box::new(skills_for),
            memory_for: Box::new(memory_for),
            tools_for: Box::new(tools_for),
            dialect_for: None,
            show_reasoning,
            memory_writable: false,
        }
    }

    /// Resolve the tool dialect an agent's provider speaks.
    pub fn with_dialect(mut self, dialect_for: impl Fn(&Agent) -> ToolDialect + 'a) -> Self {
        self.dialect_for = Some(Box::new(dialect_for));
        self
    }

    /// Say that the session saves what agents write to memory.
    pub fn with_memory_writable(mut self, writable: bool) -> Self {
        self.memory_writable = writable;
        self
    }

    /// Render an agent's memory with the session's write hint applied.
    fn memory_block(&self, agent: &Agent) -> String {
        memory_block(&(self.memory_for)(agent), self.memory_writable)
    }

    /// Render the text tool protocol, or nothing under a native dialect.
    ///
    /// A natively-equipped model already has the schemas in its request.
    /// Emitting the prose block too would declare every tool twice and, worse,
    /// instruct the model to answer in a fenced block instead of calling
    /// properly.
    fn tools_block(&self, agent: &Agent) -> String {
        if let Some(dialect_for) = &self.dialect_for {
            if dialect_for(agent) != ToolDialect::Text {
                return String::new();
            }
        }
        format_tools_prompt(&(self.tools_for)())
    }

    /// Assemble the orchestrator's system prompt from *base_prompt*, the
    /// prompt built from the gameplan.
    ///
    /// Order is base, skills, tools, memory.
    pub fn orchestrator_system(&self, agent: &Agent, base_prompt: &str) -> Result<String> {
        let skills = (self.skills_for)(agent);
        let mut prompt = agent.decorate_system_prompt(base_prompt, self.show_reasoning, &skills)?;
        let tools = self.tools_block(agent);
        if !tools.is_empty() {
            prompt = format!("{prompt}\n\n{tools}");
        }
        prompt.push_str(&self.memory_block(agent));
        Ok(prompt)
    }

    /// Assemble a participant's full message list, system message first.
    ///
    /// A participant's memory rides along with its skills block into
    /// [`Agent::build_messages`], which is what places both after the persona
    /// and language lines. Tools are appended afterwards.
    pub fn participant_messages(
        &self,
        agent: &Agent,
        history: &[Value],
        base_prompt: &str,
    ) -> Result<Vec<Value>> {
        let skills = (self.skills_for)(agent) + &self.memory_block(agent);
        let mut messages =
            agent.build_messages(history, base_prompt, self.show_reasoning, &skills)?;
        let tools = self.tools_block(agent);
        if !tools.is_empty() {
            let system = format!(
                "{}\n\n{tools}",
                messages[0]["content"].as_str().unwrap_or_default()
            );
            messages[0]["content"] = Value::String(system);
        }
        Ok(messages)
    }

    /// Assemble the message list for any agent, by position.
    ///
    /// *base_prompt* is the orchestrator's gameplan prompt, or the default
    /// participant prompt.
    pub fn messages_for(
        &self,
        agent: &Agent,
        history: &[Value],
        base_prompt: &str,
    ) -> Result<Vec<Value>> {
        if !agent.is_orchestrator() {
            return self.participant_messages(agent, history, base_prompt);
        }
        let system = self.orchestrator_system(agent, base_prompt)?;
        let mut messages = Vec::with_capacity(history.len() + 1);
        messages.push(json!({"role": "system", "content": system}));
        messages.extend(history.iter().cloned());
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Arc;

    use super::*;

    const FILLED: &str = "# Memory\n- a prior note\n";

    fn ping_tool() -> ToolSpec {
        ToolSpec::new(
            "ping",
            "Ping tool",
            json!({"type": "object", "properties": {}}),
            Arc::new(|_: &crate::tooling::Arguments, _: &str| Ok("pong".to_string())),
        )
    }

    fn assembler<'a>() -> PromptAssembler<'a> {
        PromptAssembler::new(|_| String::new(), |_| String::new(), Vec::new, None)
    }

    fn orchestrator() -> Agent {
        Agent::new("Mod").with_model("m").with_role("orchestrator")
    }

    fn index_of(haystack: &str, needle: &str) -> usize {
        haystack
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} is missing from:\n{haystack}"))
    }

    #[test]
    fn memory_with_nothing_in_it_renders_nothing() {
        assert_eq!(memory_block("", false), "");
        assert_eq!(memory_block("   \n\n  \n", false), "");
    }

    #[test]
    fn only_a_writable_session_invites_notes() {
        // Asking for notes a read-only session discards is a false promise.
        let block = memory_block(FILLED, false);
        assert!(block.starts_with(MEMORY_HEADER), "{block}");
        assert!(block.contains("a prior note"), "{block}");
        assert!(!block.contains("@MEMORY:"), "{block}");
        assert!(!block.contains("write_memory"), "{block}");

        let writable = memory_block(FILLED, true);
        assert!(writable.contains("@MEMORY:"), "{writable}");
        assert!(writable.contains("write_memory"), "{writable}");
    }

    #[test]
    fn the_orchestrator_order_is_base_skills_tools_memory() {
        let assembler = PromptAssembler::new(
            |_| "SKILLS_BLOCK".to_string(),
            |_| FILLED.to_string(),
            || vec![ping_tool()],
            None,
        );
        let prompt = assembler
            .orchestrator_system(&orchestrator(), "BASE")
            .unwrap();

        assert!(index_of(&prompt, "BASE") < index_of(&prompt, "SKILLS_BLOCK"));
        assert!(index_of(&prompt, "SKILLS_BLOCK") < index_of(&prompt, "Tool definitions:"));
        assert!(index_of(&prompt, "Tool definitions:") < index_of(&prompt, "## Memory"));
    }

    #[test]
    fn no_skills_no_tools_and_no_memory_is_just_the_base() {
        assert_eq!(
            assembler()
                .orchestrator_system(&orchestrator(), "BASE")
                .unwrap(),
            "BASE"
        );
    }

    #[test]
    fn history_follows_the_system_message_and_is_not_mutated() {
        let history = [json!({"role": "user", "content": "topic"})];
        let messages = assembler()
            .messages_for(&orchestrator(), &history, "BASE")
            .unwrap();

        assert_eq!(messages[0], json!({"role": "system", "content": "BASE"}));
        assert_eq!(messages[1..], history);
    }

    #[test]
    fn a_participants_persona_and_language_survive() {
        let agent = Agent {
            persona: Some("Engineer".to_string()),
            language: Some("French".to_string()),
            ..Agent::new("Bob").with_model("m")
        };
        let messages = assembler()
            .participant_messages(&agent, &[], "BASE")
            .unwrap();
        let system = messages[0]["content"].as_str().unwrap();

        assert!(system.contains("Persona: Engineer"), "{system}");
        assert!(system.contains("Respond in French."), "{system}");
    }

    #[test]
    fn a_participants_memory_rides_with_its_skills_before_the_tools() {
        let assembler = PromptAssembler::new(
            |_| "SKILLS_BLOCK".to_string(),
            |_| FILLED.to_string(),
            || vec![ping_tool()],
            None,
        );
        let messages = assembler
            .participant_messages(&Agent::new("Bob").with_model("m"), &[], "BASE")
            .unwrap();
        let system = messages[0]["content"].as_str().unwrap();

        assert!(index_of(system, "SKILLS_BLOCK") < index_of(system, "## Memory"));
        assert!(index_of(system, "## Memory") < index_of(system, "Tool definitions:"));
    }

    #[test]
    fn an_agents_own_system_prompt_overrides_the_default() {
        let agent = Agent {
            system_prompt: Some("CUSTOM".to_string()),
            ..Agent::new("Bob").with_model("m")
        };
        let messages = assembler()
            .participant_messages(&agent, &[], "DEFAULT")
            .unwrap();
        let system = messages[0]["content"].as_str().unwrap();

        assert!(system.contains("CUSTOM"), "{system}");
        assert!(!system.contains("DEFAULT"), "{system}");
    }

    #[test]
    fn show_reasoning_reaches_the_participant_prompt() {
        // `None` says nothing at all, which is not the same as saying no.
        let agent = Agent::new("Bob").with_model("m");
        let system = |flag| {
            let assembler =
                PromptAssembler::new(|_| String::new(), |_| String::new(), Vec::new, flag);
            let messages = assembler.participant_messages(&agent, &[], "BASE").unwrap();
            messages[0]["content"].as_str().unwrap().to_string()
        };

        assert!(system(Some(true)).contains("Provide brief reasoning."));
        assert!(system(Some(false)).contains("Do not include your reasoning, only the answer."));
        assert!(!system(None).to_lowercase().contains("reasoning"));
    }

    #[test]
    fn messages_for_routes_by_position() {
        // The orchestrator's prompt is used verbatim; a participant's is composed.
        let participant = Agent {
            persona: Some("Engineer".to_string()),
            ..Agent::new("Bob").with_model("m")
        };
        let assembler = assembler();

        assert_eq!(
            assembler
                .messages_for(&orchestrator(), &[], "BASE")
                .unwrap()[0]["content"],
            json!("BASE")
        );
        assert!(
            assembler.messages_for(&participant, &[], "BASE").unwrap()[0]["content"]
                .as_str()
                .unwrap()
                .contains("Persona: Engineer")
        );
    }

    #[test]
    fn the_tools_block_reflects_the_currently_permitted_set() {
        // tools_for is read per call, so harness narrowing is picked up.
        let permitted: RefCell<Vec<ToolSpec>> = RefCell::new(Vec::new());
        let assembler = PromptAssembler::new(
            |_| String::new(),
            |_| String::new(),
            || permitted.borrow().clone(),
            None,
        );
        let agent = orchestrator();

        assert!(!assembler
            .orchestrator_system(&agent, "BASE")
            .unwrap()
            .contains("Tool definitions:"));
        permitted.borrow_mut().push(ping_tool());
        assert!(assembler
            .orchestrator_system(&agent, "BASE")
            .unwrap()
            .contains("Tool definitions:"));
    }

    #[test]
    fn a_native_dialect_drops_the_prose_tools_block() {
        // Under a native dialect the schemas ride in the request, not the prompt.
        let agent = orchestrator();
        let system = |dialect| {
            PromptAssembler::new(
                |_| String::new(),
                |_| String::new(),
                || vec![ping_tool()],
                None,
            )
            .with_dialect(move |_| dialect)
            .orchestrator_system(&agent, "BASE")
            .unwrap()
        };

        let native = system(ToolDialect::Openai);
        assert!(!native.contains("Tool definitions:"), "{native}");
        // The fence instruction is the harmful half: a native model told to
        // answer in a fence will do that instead of calling properly.
        assert!(!native.contains("tool_calls"), "{native}");

        assert!(system(ToolDialect::Text).contains("Tool definitions:"));
    }

    #[test]
    fn no_resolver_means_text() {
        // A caller with no providers in hand — the suite, mostly — sees text.
        let assembler = PromptAssembler::new(
            |_| String::new(),
            |_| String::new(),
            || vec![ping_tool()],
            None,
        );
        let messages = assembler
            .participant_messages(&Agent::new("Bob").with_model("m"), &[], "BASE")
            .unwrap();
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("Tool definitions:"));
    }

    #[test]
    fn the_dialect_is_resolved_per_agent() {
        // Mixed-provider sessions are supported, so this cannot be session-wide.
        // Participants are gated by it exactly as the orchestrator is.
        let assembler = PromptAssembler::new(
            |_| String::new(),
            |_| String::new(),
            || vec![ping_tool()],
            None,
        )
        .with_dialect(|agent| match agent.name.as_str() {
            "Native" => ToolDialect::Anthropic,
            _ => ToolDialect::Text,
        });

        let native = assembler
            .participant_messages(&Agent::new("Native").with_model("m"), &[], "B")
            .unwrap();
        let fenced = assembler
            .participant_messages(&Agent::new("Fenced").with_model("m"), &[], "B")
            .unwrap();

        assert!(!native[0]["content"]
            .as_str()
            .unwrap()
            .contains("Tool definitions:"));
        assert!(fenced[0]["content"]
            .as_str()
            .unwrap()
            .contains("Tool definitions:"));
    }
}
