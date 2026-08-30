//! An LLM-backed participant in a session.

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::persona::{format_persona_for_prompt, load_persona};
use crate::provider::{Provider, ReasoningEffort};
use crate::pyfmt;
use crate::role::{load_role, Position, DEFAULT_ROLE_FILE};

/// One participant in a session, and everything that shapes its prompt.
///
/// Every option an agent shares with the session is written `None` for "the
/// session decides". [`Session::run`](crate::session::Session::run) fills those
/// in once, before the first turn, so a `None` seen during a run means the
/// session declared nothing either — never "not resolved yet".
#[derive(Clone)]
pub struct Agent {
    pub name: String,
    /// The model to call. `None` takes the session's.
    pub model: Option<String>,
    /// How hard this agent's model should think. Beside the model rather than
    /// on the provider, so two agents sharing one backend can differ. `None`
    /// takes the session's.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Persona prose, or a path to a `.md` persona file. `None` takes the
    /// session's, which is itself `None` when no persona is pinned.
    pub persona: Option<String>,
    /// What this agent is here to do: a built-in role name, a path to a `.md`
    /// role file, or the description written out as prose. `None` is a plain
    /// participant, which is what an agent that named nothing must be — the
    /// orchestrator is a privileged singleton, and defaulting into it would
    /// hand the session's conductor's seat to anyone who said nothing.
    ///
    /// Unlike every other option here this one does *not* take the session's:
    /// a session-wide role would make every agent the orchestrator at once.
    /// [`Session::add_agent`](crate::session::Session::add_agent) reads it into
    /// [`Agent::position`] and pins a file spec to an absolute path.
    pub role: Option<String>,
    /// Where this agent sits in the loop, read out of [`Agent::role`] when the
    /// session admits the agent. Written by the session, not by the caller.
    pub position: Position,
    /// Language to answer in. `None` takes the session's, which is itself
    /// `None` when the session pins no language.
    pub language: Option<String>,
    /// Replaces the session's default prompt when set.
    pub system_prompt: Option<String>,
    /// The backend this agent calls. The session supplies one when the agent
    /// has none of its own.
    ///
    /// Set this and you must set [`Agent::model`] too: the session's model
    /// names a model on the *session's* backend, so inheriting it across a
    /// provider boundary would send one vendor's model name to another.
    pub provider: Option<Arc<dyn Provider>>,
    /// Skills this agent may load. `None` means every skill the session has;
    /// an empty list means none.
    pub skills: Option<Vec<String>>,
    /// Tools this agent may call. `None` means every tool the session permits;
    /// an empty list means none.
    ///
    /// This narrows and never widens: a name the gameplan did not permit is
    /// refused when the session resolves its agents, so an agent cannot hand
    /// itself a tool the harness withheld. Narrowing here is what keeps a
    /// large catalogue off the prompt of an agent that calls two of it —
    /// under [`ToolDialect::Text`](crate::toolschema::ToolDialect::Text) every
    /// permitted schema is written out in the system prompt, so what an agent
    /// may call is also what it pays for on every turn.
    pub tools: Option<Vec<String>>,
    /// Memory scope name, when this agent keeps its own.
    pub memory: Option<String>,
    /// A directory this agent alone is confined to. It must sit inside the
    /// session's workspace, which it narrows rather than replaces — see
    /// [`AccessPolicy::agent_workspaces`](crate::access::AccessPolicy::agent_workspaces).
    /// `None` leaves the agent held to the session's.
    pub workspace: Option<String>,
}

impl Agent {
    /// A participant that takes every option from the session.
    pub fn new(name: impl Into<String>) -> Self {
        Agent {
            name: name.into(),
            model: None,
            reasoning_effort: None,
            persona: None,
            role: None,
            position: Position::Participant,
            language: None,
            system_prompt: None,
            provider: None,
            skills: None,
            tools: None,
            memory: None,
            workspace: None,
        }
    }

    /// Call *model* rather than the session's.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Take *role*: a built-in name, a path to a `.md` role file, or prose.
    ///
    /// Only a role file can seat an orchestrator, and only by declaring
    /// `position: orchestrator` in its frontmatter. Prose always seats a
    /// participant, so `"orchestrator, but sceptical"` describes a
    /// participant's job rather than quietly taking over the session.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    /// Call *provider* rather than the session's, with *model* on it.
    ///
    /// The two are set together because setting a provider alone is the one
    /// combination [`Session::run`](crate::session::Session::run) rejects.
    pub fn with_provider(mut self, provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        self.provider = Some(provider);
        self.model = Some(model.into());
        self
    }

    /// The model to call, or `""` before the session has resolved one.
    pub fn model_name(&self) -> &str {
        self.model.as_deref().unwrap_or_default()
    }

    /// The effort to reason at, or the framework default before the session
    /// has resolved one.
    pub fn effort(&self) -> ReasoningEffort {
        self.reasoning_effort.unwrap_or_default()
    }

    /// Build the full system prompt, falling back to *default_prompt*.
    pub fn build_system_prompt(
        &self,
        default_prompt: &str,
        show_reasoning: Option<bool>,
        skills_prompt: &str,
    ) -> Result<String> {
        let base = self.system_prompt.as_deref().unwrap_or(default_prompt);
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
        if self.persona.is_some() {
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
        if let Some(language) = &self.language {
            prompt = Cow::Owned(format!("{prompt}\nRespond in {language}."));
        }
        if !skills_prompt.is_empty() {
            prompt = Cow::Owned(format!("{prompt}\n{skills_prompt}"));
        }
        // Checked before replacing: a prompt with no placeholders is the
        // common case, and three unconditional rewrites would copy it three
        // times to produce the string it already was.
        for (placeholder, replacement) in [
            ("{bot_id}", self.name.as_str()),
            ("{bot_name}", self.name.as_str()),
            ("{model}", self.model_name()),
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
        let persona = self.persona.as_deref().unwrap_or_default();
        if persona.ends_with(".md") {
            return Ok(format_persona_for_prompt(&load_persona(persona, &[])?));
        }
        Ok(format!("Persona: {persona}"))
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

    /// This agent's base system prompt: the body of the role it named.
    ///
    /// The session pins every `.md` role to a resolved absolute path when it
    /// admits the agent, so this stays a plain lookup with no search path of
    /// its own — the same arrangement persona resolution works under. Prose is
    /// its own prompt, and an agent that named no role at all reads the
    /// built-in `participant` role.
    pub fn resolve_role(&self) -> Result<String> {
        let Some(role) = self.role.as_deref() else {
            return Ok(load_role(DEFAULT_ROLE_FILE, &[])?.content);
        };
        if role.ends_with(".md") {
            return Ok(load_role(role, &[])?.content);
        }
        Ok(role.to_string())
    }

    pub fn is_orchestrator(&self) -> bool {
        self.position == Position::Orchestrator
    }

    pub fn is_participant(&self) -> bool {
        self.position != Position::Orchestrator
    }

    /// Fill every unset option from *defaults*.
    ///
    /// Called once per agent before the first turn, which is what lets the rest
    /// of the framework read these fields without repeating the fallback.
    ///
    /// Provider and model are the one pair that does not fall back
    /// independently. A model name means something only on the backend it was
    /// written for, so an agent that brings its own provider must bring its own
    /// model: inheriting the session's would send one vendor's model name to
    /// another and fail on the first turn, or worse, silently answer from the
    /// wrong model.
    ///
    /// # Errors
    ///
    /// [`Error::Session`] when this agent sets a provider but no model, or when
    /// neither it nor the session names a model at all.
    pub fn inherit(&mut self, defaults: &AgentDefaults) -> Result<()> {
        if self.provider.is_some() && self.model.is_none() {
            return Err(Error::session(format!(
                "Agent {} sets its own provider but no model. A model name \
                 belongs to the backend it was written for, so the session's \
                 model is not inherited across providers: set a model on this \
                 agent.",
                pyfmt::repr_str(&self.name)
            )));
        }
        if self.model.is_none() {
            self.model.clone_from(&defaults.model);
        }
        if self.model.is_none() {
            return Err(Error::session(format!(
                "Agent {} has no model and the session sets none. Give the \
                 agent a model, or set one on SessionConfig.",
                pyfmt::repr_str(&self.name)
            )));
        }
        if self.reasoning_effort.is_none() {
            self.reasoning_effort = Some(defaults.reasoning_effort);
        }
        if self.persona.is_none() {
            self.persona.clone_from(&defaults.persona);
        }
        if self.language.is_none() {
            self.language.clone_from(&defaults.language);
        }
        Ok(())
    }
}

/// The session's answer for each option an agent may leave unset.
///
/// [`Agent::system_prompt`] is absent on purpose: it already falls back through
/// the `default_prompt` argument to [`Agent::build_system_prompt`], so
/// resolving it into the agent would give it two fallbacks that could disagree.
#[derive(Clone, Debug, Default)]
pub struct AgentDefaults {
    pub model: Option<String>,
    pub reasoning_effort: ReasoningEffort,
    pub persona: Option<String>,
    pub language: Option<String>,
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
            .field("position", &self.position)
            .field("language", &self.language)
            .field("system_prompt", &self.system_prompt)
            .finish()
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::testing::TempDir;

    fn alice() -> Agent {
        Agent::new("Alice").with_model("test/model")
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
            system_prompt: Some("Custom prompt.".to_string()),
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
            persona: Some("Pragmatic engineer".to_string()),
            language: Some("French".to_string()),
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
            persona: Some(path.display().to_string()),
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
            persona: Some("typo.md".to_string()),
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
            persona: Some("A sceptic who asks for evidence.".to_string()),
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
    fn the_two_positions_are_exclusive_and_participant_is_the_default() {
        let orchestrator = Agent {
            position: Position::Orchestrator,
            ..Agent::new("Orch").with_model("test/model")
        };
        assert!(orchestrator.is_orchestrator());
        assert!(!orchestrator.is_participant());
        assert!(alice().is_participant());
        assert!(!alice().is_orchestrator());
    }

    /// A backend that answers nothing; only its presence on an agent matters.
    struct StubProvider(crate::provider::ProviderBase);

    impl crate::provider::Provider for StubProvider {
        fn name(&self) -> &str {
            "StubProvider"
        }

        fn base(&self) -> &crate::provider::ProviderBase {
            &self.0
        }

        fn chat(
            &self,
            _model: &str,
            _messages: &[Value],
            _tools: Option<&[crate::tooling::ToolSpec]>,
            _effort: ReasoningEffort,
        ) -> Result<crate::provider::ProviderResponse> {
            unreachable!("inheritance never calls the backend")
        }
    }

    fn stub_provider() -> Arc<dyn Provider> {
        Arc::new(StubProvider(crate::provider::ProviderBase::new(
            0, 0.0, None,
        )))
    }

    fn defaults() -> AgentDefaults {
        AgentDefaults {
            model: Some("session/model".to_string()),
            reasoning_effort: ReasoningEffort::Low,
            persona: Some("A session persona".to_string()),
            language: Some("German".to_string()),
        }
    }

    #[test]
    fn an_unset_option_takes_the_sessions_and_a_set_one_keeps_its_own() {
        let mut inherits = Agent::new("Alice");
        inherits.inherit(&defaults()).expect("session supplies all");
        assert_eq!(inherits.model_name(), "session/model");
        assert_eq!(inherits.effort(), ReasoningEffort::Low);
        assert_eq!(inherits.persona.as_deref(), Some("A session persona"));
        assert_eq!(inherits.language.as_deref(), Some("German"));

        let mut overrides = Agent {
            reasoning_effort: Some(ReasoningEffort::High),
            persona: Some("Its own persona".to_string()),
            language: Some("French".to_string()),
            ..Agent::new("Bob").with_model("own/model")
        };
        overrides.inherit(&defaults()).expect("agent supplies all");
        assert_eq!(overrides.model_name(), "own/model");
        assert_eq!(overrides.effort(), ReasoningEffort::High);
        assert_eq!(overrides.persona.as_deref(), Some("Its own persona"));
        assert_eq!(overrides.language.as_deref(), Some("French"));
    }

    /// The session may pin nothing at all, and that has to survive inheritance
    /// as "nothing" rather than becoming an empty persona in the prompt.
    #[test]
    fn a_session_that_pins_nothing_leaves_the_agent_pinning_nothing() {
        let mut agent = Agent::new("Alice").with_model("own/model");
        agent
            .inherit(&AgentDefaults::default())
            .expect("a model of its own is enough");

        assert_eq!(agent.persona, None);
        assert_eq!(agent.language, None);
        assert_eq!(agent.effort(), ReasoningEffort::default());
        assert_eq!(
            agent.build_system_prompt("Base.", None, "").unwrap(),
            "Base."
        );
    }

    /// The one pair that does not fall back independently: a model name is
    /// only meaningful on the backend it was written for.
    #[test]
    fn an_agent_with_its_own_provider_must_bring_its_own_model() {
        let mut agent = Agent::new("Alice");
        agent.provider = Some(stub_provider());

        let error = agent
            .inherit(&defaults())
            .expect_err("the session's model names a model on another backend");

        assert!(matches!(error, Error::Session(_)), "{error:?}");
        let message = error.to_string();
        assert!(message.contains("'Alice'"), "{message}");
        assert!(
            message.contains("not inherited across providers"),
            "{message}"
        );

        let mut paired = Agent::new("Bob").with_provider(stub_provider(), "own/model");
        paired
            .inherit(&defaults())
            .expect("provider and model set together");
        assert_eq!(paired.model_name(), "own/model");
    }

    #[test]
    fn a_model_named_nowhere_is_an_error_that_says_both_places_to_set_it() {
        let error = Agent::new("Alice")
            .inherit(&AgentDefaults::default())
            .expect_err("no model anywhere is not a runnable agent");

        assert!(matches!(error, Error::Session(_)), "{error:?}");
        let message = error.to_string();
        assert!(message.contains("'Alice'"), "{message}");
        assert!(message.contains("SessionConfig"), "{message}");
    }

    #[test]
    fn a_named_role_is_kept_verbatim_until_the_session_reads_it() {
        let agent = Agent::new("Alice")
            .with_model("m")
            .with_role("orchestrator, but sceptical");
        assert_eq!(agent.role.as_deref(), Some("orchestrator, but sceptical"));
        assert_eq!(agent.position, Position::Participant);
    }
}
