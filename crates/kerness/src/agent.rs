//! An LLM-backed participant in a session.

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::persona::{format_persona_for_prompt, load_persona};
use crate::provider::{Provider, ReasoningEffort};
use crate::pyfmt;

/// What an agent is in the session.
///
/// A closed set rather than a string: an unrecognised role satisfies neither
/// [`Agent::is_orchestrator`] nor the orchestrator lookup, so accepting one
/// would turn the session's conductor into an extra debater with no error
/// anywhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Role {
    #[default]
    Participant,
    Orchestrator,
}

impl Role {
    /// The name this role is written with in a gameplan.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Participant => "participant",
            Role::Orchestrator => "orchestrator",
        }
    }

    /// Read a role from its written name.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "participant" => Ok(Role::Participant),
            "orchestrator" => Ok(Role::Orchestrator),
            other => Err(Error::Value(format!(
                "Unknown agent role {}. Expected 'participant' or 'orchestrator'.",
                pyfmt::repr_str(other)
            ))),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One participant in a session, and everything that shapes its prompt.
#[derive(Clone)]
pub struct Agent {
    pub name: String,
    pub model: String,
    /// How hard this agent's model should think. Beside the model rather than
    /// on the provider, so two agents sharing one backend can differ.
    pub reasoning_effort: ReasoningEffort,
    /// Persona prose, or a path to a `.md` persona file.
    pub persona: String,
    pub role: Role,
    /// Language to answer in, if the session pins one.
    pub language: String,
    /// Replaces the session's default prompt when set.
    pub system_prompt: String,
    /// The backend this agent calls. The session supplies one when the agent
    /// has none of its own.
    pub provider: Option<Arc<dyn Provider>>,
    /// Skills this agent may load. `None` means every skill the session has;
    /// an empty list means none.
    pub skills: Option<Vec<String>>,
    /// Memory scope name, when this agent keeps its own.
    pub memory: Option<String>,
}

impl Agent {
    /// A participant with no persona, language, or provider of its own.
    pub fn new(name: impl Into<String>, model: impl Into<String>) -> Self {
        Agent {
            name: name.into(),
            model: model.into(),
            reasoning_effort: ReasoningEffort::default(),
            persona: String::new(),
            role: Role::Participant,
            language: String::new(),
            system_prompt: String::new(),
            provider: None,
            skills: None,
            memory: None,
        }
    }

    /// Build the full system prompt, falling back to *default_prompt*.
    pub fn build_system_prompt(
        &self,
        default_prompt: &str,
        show_reasoning: Option<bool>,
        skills_prompt: &str,
    ) -> Result<String> {
        let base = if self.system_prompt.is_empty() {
            default_prompt
        } else {
            &self.system_prompt
        };
        self.decorate_system_prompt(base, show_reasoning, skills_prompt)
    }

    /// Apply this agent's persona and response preferences to *prompt*.
    ///
    /// Prompt assembly uses this for orchestrators after the session has
    /// already resolved their gameplan or custom base prompt. Keeping the
    /// decoration here prevents participant and orchestrator behavior from
    /// drifting apart.
    pub fn decorate_system_prompt(
        &self,
        prompt: &str,
        show_reasoning: Option<bool>,
        skills_prompt: &str,
    ) -> Result<String> {
        let mut prompt = Cow::Borrowed(prompt);
        if !self.persona.is_empty() {
            prompt = Cow::Owned(format!("{prompt}\n{}", self.resolve_persona()?));
        }
        match show_reasoning {
            Some(true) => prompt = Cow::Owned(format!("{prompt}\nProvide brief reasoning.")),
            Some(false) => {
                prompt = Cow::Owned(format!(
                    "{prompt}\nDo not include your reasoning, only the answer."
                ))
            }
            None => {}
        }
        if !self.language.is_empty() {
            prompt = Cow::Owned(format!("{prompt}\nRespond in {}.", self.language));
        }
        if !skills_prompt.is_empty() {
            prompt = Cow::Owned(format!("{prompt}\n{skills_prompt}"));
        }
        // Checked before replacing: a prompt with no placeholders is the
        // common case, and three unconditional rewrites would copy it three
        // times to produce the string it already was.
        for (placeholder, replacement) in [
            ("{bot_id}", &self.name),
            ("{bot_name}", &self.name),
            ("{model}", &self.model),
        ] {
            if prompt.contains(placeholder) {
                prompt = Cow::Owned(prompt.replace(placeholder, replacement));
            }
        }
        Ok(prompt.into_owned())
    }

    /// Resolve this agent's persona, loading from file when it names one.
    ///
    /// A path that does not resolve **fails**, exactly as a missing gameplan or
    /// skill does. Passing the unresolved path through as persona text would
    /// put the literal line `Persona: ./personas/typo.md` into the system
    /// prompt and let the session run to completion looking healthy, which is
    /// worse than not loading at all: the run costs real provider calls to
    /// produce agents with none of the character they were configured with.
    ///
    /// The session pins every `.md` persona to a resolved absolute path before
    /// the run, so this stays a plain lookup with no search path of its own.
    fn resolve_persona(&self) -> Result<String> {
        if self.persona.ends_with(".md") {
            return Ok(format_persona_for_prompt(&load_persona(
                &self.persona,
                &[],
            )?));
        }
        Ok(format!("Persona: {}", self.persona))
    }

    /// Build the message list for one provider call: system prompt, then
    /// *history* in order.
    pub fn build_messages(
        &self,
        history: &[Value],
        default_prompt: &str,
        show_reasoning: Option<bool>,
        skills_prompt: &str,
    ) -> Result<Vec<Value>> {
        let system_prompt =
            self.build_system_prompt(default_prompt, show_reasoning, skills_prompt)?;
        let mut messages = Vec::with_capacity(history.len() + 1);
        messages.push(json!({"role": "system", "content": system_prompt}));
        messages.extend(history.iter().cloned());
        Ok(messages)
    }

    pub fn is_orchestrator(&self) -> bool {
        self.role == Role::Orchestrator
    }

    pub fn is_participant(&self) -> bool {
        self.role != Role::Orchestrator
    }
}

/// The configuration, without the provider, skills, or memory.
///
/// A provider is a live connection and the other two are session wiring; none
/// of them says anything about *this* agent that a reader debugging a prompt
/// wants to see.
impl fmt::Debug for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Agent")
            .field("name", &self.name)
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("persona", &self.persona)
            .field("role", &self.role)
            .field("language", &self.language)
            .field("system_prompt", &self.system_prompt)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("kerness-agent-{tag}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        fn write(&self, name: &str, text: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, text).expect("write persona");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn alice() -> Agent {
        Agent::new("Alice", "test/model")
    }

    #[test]
    fn an_undecorated_agent_gets_the_prompt_it_was_given() {
        assert_eq!(
            alice()
                .build_system_prompt("You are helpful.", None, "")
                .unwrap(),
            "You are helpful."
        );

        let custom = Agent {
            system_prompt: "Custom prompt.".to_string(),
            ..alice()
        };
        assert_eq!(
            custom
                .build_system_prompt("You are helpful.", None, "")
                .unwrap(),
            "Custom prompt."
        );
    }

    #[test]
    fn every_decoration_is_appended_to_the_base() {
        let agent = Agent {
            persona: "Pragmatic engineer".to_string(),
            language: "French".to_string(),
            ..alice()
        };
        let result = agent
            .build_system_prompt("Base.", None, "Use summarize skill.")
            .unwrap();

        assert_eq!(
            result,
            "Base.\nPersona: Pragmatic engineer\nRespond in French.\nUse summarize skill."
        );
    }

    #[test]
    fn show_reasoning_says_yes_no_or_nothing_at_all() {
        let agent = alice();
        assert!(agent
            .build_system_prompt("Base.", Some(true), "")
            .unwrap()
            .contains("Provide brief reasoning."));
        assert!(agent
            .build_system_prompt("Base.", Some(false), "")
            .unwrap()
            .contains("Do not include your reasoning"));
        assert!(!agent
            .build_system_prompt("Base.", None, "")
            .unwrap()
            .contains("reasoning"));
    }

    #[test]
    fn placeholders_carry_the_agents_own_identity() {
        let result = alice()
            .build_system_prompt("Hello {bot_name} ({bot_id}), model={model}", None, "")
            .unwrap();
        assert_eq!(result, "Hello Alice (Alice), model=test/model");
    }

    #[test]
    fn a_persona_file_is_loaded_and_formatted() {
        let dir = TempDir::new("persona-file");
        let path = dir.write(
            "testbot.md",
            "# Persona: TestBot\n\n\
             ## Persona\nA test persona.\n\n\
             ## Background\nTest background.\n\n\
             ## Communication Style\nDirect.\n",
        );

        let agent = Agent {
            persona: path.display().to_string(),
            ..alice()
        };
        let result = agent.build_system_prompt("Base.", None, "").unwrap();

        assert!(result.contains("Persona: A test persona."), "{result}");
        assert!(result.contains("Background: Test background."), "{result}");
        assert!(result.contains("Communication style: Direct."), "{result}");
    }

    #[test]
    fn a_missing_persona_file_fails_and_names_what_it_tried() {
        let agent = Agent {
            persona: "typo.md".to_string(),
            ..alice()
        };
        let error = agent
            .build_system_prompt("Base.", None, "")
            .expect_err("a persona path that resolves to nothing is not prose");
        assert!(matches!(error, Error::NotFound(_)), "{error:?}");
        assert!(error.to_string().contains("Tried:"), "{error}");
    }

    #[test]
    fn a_plain_prose_persona_is_untouched() {
        let agent = Agent {
            persona: "A sceptic who asks for evidence.".to_string(),
            ..alice()
        };
        assert!(agent
            .build_system_prompt("Base.", None, "")
            .unwrap()
            .contains("Persona: A sceptic who asks for evidence."));
    }

    #[test]
    fn the_system_prompt_leads_and_history_follows_in_order() {
        let history = [
            json!({"role": "user", "content": "first"}),
            json!({"role": "assistant", "content": "second"}),
        ];
        let messages = alice()
            .build_messages(&history, "Default prompt.", None, "")
            .unwrap();

        assert_eq!(
            messages,
            vec![
                json!({"role": "system", "content": "Default prompt."}),
                history[0].clone(),
                history[1].clone(),
            ]
        );
    }

    #[test]
    fn the_two_roles_are_exclusive_and_participant_is_the_default() {
        let orchestrator = Agent {
            role: Role::Orchestrator,
            ..Agent::new("Orch", "test/model")
        };
        assert!(orchestrator.is_orchestrator());
        assert!(!orchestrator.is_participant());
        assert!(alice().is_participant());
        assert!(!alice().is_orchestrator());
    }

    #[test]
    fn an_unknown_role_is_rejected() {
        assert_eq!(Role::parse("participant").unwrap(), Role::Participant);
        assert_eq!(Role::parse("orchestrator").unwrap(), Role::Orchestrator);
        for role in ["orchestrater", "moderator"] {
            let error = Role::parse(role).expect_err("a typo is not a role");
            assert_eq!(
                error.to_string(),
                format!("Unknown agent role '{role}'. Expected 'participant' or 'orchestrator'.")
            );
        }
    }
}
