//! The public facade: configure a run, then hand control to the loop.
//!
//! A [`Session`] holds the cast, the toolkit, the memory files and the access
//! policy; [`Session::run`] validates all of it before the first provider call
//! and then implements [`LoopHost`] for
//! [`OrchestratorLoop`](crate::orchestrator::OrchestratorLoop). Who speaks,
//! when the run ends and what it returns come from the gameplan's harness
//! contract, not from here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::access::{AccessManager, AccessPolicy};
use crate::agent::{Agent, Role};
use crate::agent_runtime::AgentRunner;
use crate::channel::{Channel, ConsoleChannel};
use crate::compaction::{compact, estimate_tokens, summary_request};
use crate::conversation::{ChatMessage, Conversation, Message, Turn};
use crate::error::{Error, Result};
use crate::exec;
use crate::gameplan::{load_gameplan, GameplanConfig};
use crate::harness::validate_harness;
use crate::logging;
use crate::memory::Memory;
use crate::orchestrator::{LoopHost, OrchestratorLoop};
use crate::persona::{load_persona, resolve_persona_path};
use crate::prompting::PromptAssembler;
use crate::provider::Provider;
use crate::pyfmt;
use crate::sessionfile::{
    check_identity, identity_for, load_snapshot, save_snapshot, SessionSnapshot,
};
use crate::skill::loader::{load_skill, SkillConfig};
use crate::skill::runtime::{
    apply_gate, format_skills_index, GrantPaths, SkillActivation, SkillRegistry, SkillsFor,
    SKILL_TOOL_NAME,
};
use crate::tooling::{Arguments, ToolHandler, ToolSpec};
use crate::toolkit::{resolve, ToolDispatcher, ToolsFor};
use crate::toolschema::{tool_schemas, ToolDialect};
use crate::utils::parse_memory_markers;

/// Default ceiling on one request, in estimated tokens.
///
/// This stands for what the model can hold, so it is not the framework's number
/// to pick well — a caller running a 128k model should say so. The default is
/// deliberately generous rather than clever: guessing per model would mean
/// shipping a table of context windows that goes stale every release and has no
/// entry at all for `CustomProvider`.
pub const DEFAULT_MAX_CONTEXT_TOKENS: usize = 256_000;

/// The default participant system prompt.
pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a participant in a structured debate. Be concise.";

/// Result of a completed session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionResult {
    pub topic: String,
    pub turns_completed: i64,
    pub consensus_reached: bool,
    pub history: Vec<Message>,
    pub final_summary: String,
    /// The gameplan's declared `result:` fields, read out of the closing turn.
    /// Empty when the gameplan declares no result shape.
    pub fields: Map<String, Value>,
    /// Rounds the loop actually completed. A round closes when every
    /// participant has spoken once since the last one did.
    pub rounds_run: i64,
    /// The phase the session was in when it stopped; `""` when the harness
    /// declared none.
    pub phase_reached: String,
    /// Why the loop stopped: `keyword`, `phases_complete`, `max_rounds`,
    /// `max_turns`, or `forced`.
    pub end_reason: String,
}

impl SessionResult {
    /// The final summary, under the name the Python API exposes.
    pub fn summary(&self) -> &str {
        &self.final_summary
    }
}

/// Everything a session is configured with, before any agent is added.
///
/// A struct rather than a builder because upstream's constructor is one call
/// with seventeen keywords and no ordering rules between them: several of these
/// decide what the session is built *out of* — `memory_write` picks the default
/// toolkit, `access_policy` the access manager — so they have to be settled
/// before construction rather than patched onto a half-built session.
#[derive(Clone)]
pub struct SessionConfig {
    /// Gameplan name or path.
    pub gameplan: String,
    /// The topic or question for the session.
    pub topic: String,
    /// Backend for agents that bring none of their own.
    pub provider: Option<Arc<dyn Provider>>,
    /// Where output goes while the run is in progress.
    pub channel: Option<Arc<dyn Channel>>,
    /// Path to the memory `.md` file. The file is yours: it is free-form
    /// prose, it is read as empty when absent, and nothing is created or
    /// templated on your behalf.
    pub memory: String,
    /// Whether the session may write to the memory file. Off by default, which
    /// makes a run read-only: the `write_memory` tool is not offered,
    /// `@MEMORY:` notes are dropped, and the session result is not recorded.
    pub memory_write: bool,
    /// Path to this run's state file, or `None` to persist nothing. This is
    /// not the memory file: it holds the conversation, the turn count, and the
    /// phase position for one run, in a schema the framework owns.
    pub session_file: Option<String>,
    /// Estimated ceiling on one request, standing in for what the model can
    /// hold. Everything the request carries counts against it.
    pub max_context_tokens: usize,
    /// Policy for running commands and reading files.
    pub access_policy: Option<AccessPolicy>,
    /// Enforced participant-round limit. `None` takes the gameplan's
    /// `loop.max_rounds`; zero skips directly to closing.
    pub max_rounds: Option<i64>,
    /// Hard limit on counted turns. `None` takes the gameplan's
    /// `loop.max_turns`.
    pub max_turns: Option<i64>,
    /// Max tool-call followup iterations per reply; `None` is unlimited.
    pub max_tool_iterations: Option<u32>,
    /// Delay between agent turns.
    pub turn_delay: Duration,
    /// Whether agents should show reasoning.
    pub show_reasoning: Option<bool>,
    /// Default system prompt for participant agents.
    pub system_prompt: String,
    /// Retries for unparseable orchestrator output. `None` takes the
    /// gameplan's `loop.orchestrator_retries`.
    pub orchestrator_retries: Option<i64>,
    /// Whether an agent's tool calls and results also enter the shared
    /// conversation. Off by default: tool exchanges are private to the turn
    /// that made them, so an agent sees what another agent *said*, not how it
    /// got there.
    pub tool_results_in_history: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            gameplan: "debate".to_string(),
            topic: String::new(),
            provider: None,
            channel: None,
            memory: "memory.md".to_string(),
            memory_write: false,
            session_file: None,
            max_context_tokens: DEFAULT_MAX_CONTEXT_TOKENS,
            access_policy: None,
            max_rounds: None,
            max_turns: None,
            max_tool_iterations: None,
            turn_delay: Duration::from_secs(1),
            show_reasoning: None,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            orchestrator_retries: None,
            tool_results_in_history: false,
        }
    }
}

/// A session's memory files, session-level and per agent.
struct Memories {
    session: Memory,
    per_agent: HashMap<String, Memory>,
}

impl Memories {
    /// The memory an agent reads and writes: its own when it has one.
    fn get(&self, name: &str) -> &Memory {
        self.per_agent.get(name).unwrap_or(&self.session)
    }

    fn get_mut(&mut self, name: &str) -> &mut Memory {
        if self.per_agent.contains_key(name) {
            return self.per_agent.get_mut(name).expect("checked above");
        }
        &mut self.session
    }
}

/// The parts of a session that outlive a borrow of it.
///
/// Tool handlers are `Arc<dyn ToolHandler>` and the dispatcher's tool source is
/// an `Arc<dyn Fn>`: both are `Send + Sync + 'static`, so neither can hold a
/// reference to the session that built them. Upstream's handlers are bound
/// methods on `self` and reach the whole object; here they close over this
/// instead, which is the same state by a different route.
struct Shared {
    channel: Arc<dyn Channel>,
    provider: Option<Arc<dyn Provider>>,
    show_reasoning: Option<bool>,
    memory_write: bool,
    access: Arc<Mutex<AccessManager>>,
    memories: Arc<Mutex<Memories>>,
    tools: Mutex<Vec<ToolSpec>>,
    /// Tool names the harness permits, set by `validate_harness` in `run()`.
    /// `None` means "not yet resolved"; an empty list is a real answer (a
    /// gameplan declaring `tools: []` grants none), so the two cannot share a
    /// representation.
    allowed_tools: Mutex<Option<Vec<String>>>,
    /// The skill activation for the turn in progress. Replaced at the start of
    /// every turn, which is what bounds a loaded body — and the tool gate it
    /// may carry — to that turn.
    activation: Mutex<Option<Arc<SkillActivation>>>,
    skills_cache: Arc<Mutex<HashMap<String, Vec<SkillConfig>>>>,
    skills_registry: SkillRegistry,
}

/// Take a session lock, treating poisoning as the panic it reports.
///
/// A poisoned lock means an earlier turn panicked while holding it. That is a
/// bug in the framework rather than a state the caller can act on, and the
/// alternative — propagating a lock error out of every accessor — would put an
/// impossible failure in every signature.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared {
    /// The resolved skills an agent may load.
    fn skills_for(&self, agent_name: &str) -> Vec<SkillConfig> {
        lock(&self.skills_cache)
            .get(agent_name)
            .cloned()
            .unwrap_or_default()
    }

    /// The memory content an agent reads.
    fn memory_text(&self, agent_name: &str) -> String {
        lock(&self.memories).get(agent_name).read().to_string()
    }

    /// Resolve the provider for an agent: agent-level over session-level.
    fn provider_for(&self, agent: &Agent) -> Option<Arc<dyn Provider>> {
        agent.provider.clone().or_else(|| self.provider.clone())
    }

    /// The tool dialect an agent's provider actually speaks.
    fn dialect_for(&self, agent: &Agent) -> ToolDialect {
        self.provider_for(agent)
            .map_or(ToolDialect::Text, |provider| provider.effective_dialect())
    }

    /// The tools callable right now.
    ///
    /// Three narrowings, applied in order, and the order matters:
    ///
    /// 1. The harness narrows the registered set. Before `run()` resolves it
    ///    this is every registered tool; after, it is the permitted set.
    /// 2. The turn's `Skill` tool is added, built from the agent's own skill
    ///    list so its `enum` names only skills that agent can load.
    /// 3. An active skill's `allowed-tools` narrows further, restrictively.
    ///
    /// Both the prompt and the dispatcher read this, so a tool the gameplan
    /// excluded is neither advertised to the model nor callable if it asks
    /// anyway — and the same holds for a tool an active skill gated out.
    fn active_tools(&self) -> Vec<ToolSpec> {
        let tools = resolve(&lock(&self.tools), lock(&self.allowed_tools).as_deref());
        let Some(activation) = lock(&self.activation).clone() else {
            return tools;
        };
        let mut tools = tools;
        if let Some(skill_tool) = self.skills_registry.build_tool(&activation) {
            tools.push(skill_tool);
        }
        apply_gate(&tools, activation.gate().as_ref())
    }

    /// Start a fresh activation for one agent's turn.
    fn start_activation(&self, agent_name: &str) {
        *lock(&self.activation) = Some(self.skills_registry.activation_for(agent_name));
    }

    /// An assembler bound to this session's current state.
    ///
    /// Built per call rather than cached because skills, memory, and the
    /// permitted tool set are all resolved during `run()` — an assembler
    /// captured at construction would close over the wrong ones.
    fn prompts(&self) -> PromptAssembler<'_> {
        PromptAssembler::new(
            |agent: &Agent| format_skills_index(&self.skills_for(&agent.name)),
            |agent: &Agent| self.memory_text(&agent.name),
            || self.active_tools(),
            self.show_reasoning,
        )
        .with_dialect(|agent: &Agent| self.dialect_for(agent))
        .with_memory_writable(self.memory_write)
    }
}

/// Orchestrates a multi-agent collaboration session.
///
/// The orchestrator LLM routes participant turns and may request early phase
/// changes or termination; the loop also enforces and auto-advances gameplan
/// phase, round, and turn limits. The gameplan `.md` is injected into the
/// orchestrator's system prompt as its instruction manual.
pub struct Session {
    gameplan: GameplanConfig,
    topic: String,
    max_rounds: i64,
    max_turns: Option<i64>,
    max_tool_iterations: Option<u32>,
    orchestrator_retries: Option<i64>,
    turn_delay: Duration,
    system_prompt: String,
    tool_results_in_history: bool,
    max_context_tokens: usize,
    session_file: Option<String>,
    compactions: i64,

    agents: Vec<Agent>,
    skills: Vec<SkillConfig>,
    conversation: Conversation,
    dispatcher: ToolDispatcher,
    shared: Arc<Shared>,

    /// `LoopHost` state, populated by `run()` before the loop is constructed.
    orch_prompt: String,
    participants: Vec<String>,
    identity: Map<String, Value>,
    /// The loop's last published position, for `save()`.
    ///
    /// Upstream reads `self._loop.snapshot()` back out of the loop it owns;
    /// here the loop pushes its position through
    /// [`LoopHost::record_position`] instead.
    loop_position: Map<String, Value>,
}

impl Session {
    /// Load the gameplan and build the session it describes.
    pub fn new(config: SessionConfig) -> Result<Self> {
        let gameplan = load_gameplan(&config.gameplan)?;
        let max_rounds = config.max_rounds.unwrap_or_else(|| gameplan.max_rounds());

        let channel: Arc<dyn Channel> = config
            .channel
            .unwrap_or_else(|| Arc::new(ConsoleChannel::default()));
        // `AccessPolicy::new()`, not `default()`: the two differ on
        // `trust_skill_bundles`, and a session that silently stopped granting
        // bundle paths would break skills with no diagnostic.
        let policy = match config.access_policy {
            Some(policy) => policy,
            None => AccessPolicy::new(),
        };
        let access = Arc::new(Mutex::new(AccessManager::new(policy)));
        let memories = Arc::new(Mutex::new(Memories {
            session: Memory::new(&config.memory),
            per_agent: HashMap::new(),
        }));
        let skills_cache: Arc<Mutex<HashMap<String, Vec<SkillConfig>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let skills_for: SkillsFor = {
            let cache = Arc::clone(&skills_cache);
            Arc::new(move |name: &str| lock(&cache).get(name).cloned().unwrap_or_default())
        };
        let grant_paths: GrantPaths = {
            let access = Arc::clone(&access);
            Arc::new(move |paths: &[PathBuf]| {
                let mut manager = lock(&access);
                if !manager.policy().trust_skill_bundles {
                    return;
                }
                manager.allow_dirs(paths.iter().map(|path| path.display().to_string()));
            })
        };

        let shared = Arc::new(Shared {
            tools: Mutex::new(default_tools(&access, &memories, &channel, config.memory_write)),
            channel,
            provider: config.provider,
            show_reasoning: config.show_reasoning,
            memory_write: config.memory_write,
            access,
            memories,
            allowed_tools: Mutex::new(None),
            activation: Mutex::new(None),
            skills_cache,
            skills_registry: SkillRegistry::new(skills_for, Some(grant_paths)),
        });
        let dispatcher = ToolDispatcher::new({
            let shared = Arc::clone(&shared);
            Arc::new(move || shared.active_tools()) as ToolsFor
        });

        Ok(Session {
            gameplan,
            topic: config.topic,
            max_rounds,
            max_turns: config.max_turns,
            max_tool_iterations: config.max_tool_iterations,
            orchestrator_retries: config.orchestrator_retries,
            turn_delay: config.turn_delay,
            system_prompt: config.system_prompt,
            tool_results_in_history: config.tool_results_in_history,
            max_context_tokens: config.max_context_tokens,
            session_file: config.session_file,
            compactions: 0,
            agents: Vec::new(),
            skills: Vec::new(),
            conversation: Conversation::new(),
            dispatcher,
            shared,
            orch_prompt: String::new(),
            participants: Vec::new(),
            identity: Map::new(),
            loop_position: Map::new(),
        })
    }

    /// The allowed command regex patterns.
    pub fn exec(&self) -> Vec<String> {
        lock(&self.shared.access)
            .policy()
            .allowed_command_patterns
            .clone()
    }

    /// Replace the allowed command regex patterns.
    pub fn set_exec(&mut self, patterns: Vec<String>) {
        let mut manager = lock(&self.shared.access);
        let mut policy = manager.policy().clone();
        policy.allowed_command_patterns = patterns;
        *manager = AccessManager::new(policy);
    }

    /// The session-level memory file's content, as last read or written.
    pub fn memory(&self) -> String {
        lock(&self.shared.memories).session.read().to_string()
    }

    /// The rounds limit in force, after the gameplan's default is applied.
    pub fn max_rounds(&self) -> i64 {
        self.max_rounds
    }

    /// The agents registered so far.
    pub fn agents(&self) -> &[Agent] {
        &self.agents
    }

    /// Add a participant agent to the session.
    pub fn add_participant(&mut self, agent: Agent) -> &mut Self {
        let mut agent = agent;
        agent.role = Role::Participant;
        self.agents.push(agent);
        self
    }

    /// Add the session's one orchestrator.
    ///
    /// # Errors
    ///
    /// [`Error::Session`] when the session already has one.
    pub fn add_orchestrator(&mut self, agent: Agent) -> Result<&mut Self> {
        if let Some(existing) = self.agents.iter().find(|a| a.is_orchestrator()) {
            return Err(Error::session(format!(
                "Session already has an orchestrator: '{}'. Only one \
                 orchestrator is allowed.",
                existing.name
            )));
        }
        let mut agent = agent;
        agent.role = Role::Orchestrator;
        self.agents.push(agent);
        Ok(self)
    }

    /// Attach a skill to the session.
    ///
    /// Inheriting agents see its index entry and load its body through the
    /// `Skill` tool.
    pub fn add_skill(&mut self, name: &str) -> Result<&mut Self> {
        let skill = load_skill(name)?;
        self.skills.push(skill);
        Ok(self)
    }

    /// Register a callable tool for agents to invoke.
    ///
    /// # Errors
    ///
    /// [`Error::Session`] when *name* is the reserved `Skill` tool, which the
    /// runtime builds per agent — shadowing it would disable skill loading with
    /// no diagnostic — or when a tool of that name is already registered.
    pub fn add_tool(
        &mut self,
        name: &str,
        description: &str,
        parameters: Value,
        handler: Arc<dyn ToolHandler>,
    ) -> Result<&mut Self> {
        if name == SKILL_TOOL_NAME {
            return Err(Error::session(format!(
                "'{SKILL_TOOL_NAME}' is a reserved tool name — the runtime \
                 builds it per agent for skill loading. Choose another name."
            )));
        }
        let mut tools = lock(&self.shared.tools);
        if tools.iter().any(|tool| tool.name == name) {
            return Err(Error::session(format!(
                "A tool named '{name}' is already registered. Tool names must \
                 be unique."
            )));
        }
        tools.push(ToolSpec::new(name, description, parameters, handler));
        drop(tools);
        Ok(self)
    }

    /// Run an external command with access control.
    ///
    /// A named *actor* also reports the attempt through the session's channel.
    pub fn run_command(
        &self,
        command: &str,
        cwd: Option<&Path>,
        timeout: Option<Duration>,
        actor: &str,
    ) -> Result<String> {
        run_and_log(
            &self.shared.access,
            self.shared.channel.as_ref(),
            command,
            cwd,
            timeout,
            actor,
        )
    }

    /// Read a file with access control.
    pub fn read_file(&self, path: &str, actor: &str) -> Result<String> {
        exec::read_file(&lock(&self.shared.access), path, actor)
    }

    /// List a directory with access control.
    pub fn list_dir(&self, path: &str, actor: &str) -> Result<Vec<String>> {
        exec::list_dir(&lock(&self.shared.access), path, actor)
    }

    /// Execute the session, blocking until the loop terminates.
    ///
    /// Validates, assembles, and hands control to [`OrchestratorLoop`]. The
    /// flow itself — who speaks, when it ends, what the result contains — comes
    /// from the gameplan's harness contract, not from this method.
    pub fn run(&mut self) -> Result<SessionResult> {
        let missing: Vec<&str> = self
            .agents
            .iter()
            .filter(|agent| agent.provider.is_none())
            .map(|agent| agent.name.as_str())
            .collect();
        if self.shared.provider.is_none() && !missing.is_empty() {
            return Err(Error::session(format!(
                "No provider configured. Either pass a provider to Session() \
                 or set a provider on each agent. Missing: {}",
                missing.join(", ")
            )));
        }
        if self.topic.is_empty() {
            return Err(Error::session("No topic set."));
        }
        self.check_tool_history_dialect()?;

        let participants: Vec<String> = self
            .agents
            .iter()
            .filter(|agent| agent.is_participant())
            .map(|agent| agent.name.clone())
            .collect();
        if participants.is_empty() {
            return Err(Error::session(
                "No participant agents added. Add at least one.",
            ));
        }
        let orchestrator = match self.agents.iter().find(|agent| agent.is_orchestrator()) {
            Some(agent) => agent.name.clone(),
            // Only one loop shape exists and it is orchestrator-driven, so a
            // harness may make an orchestrator optional but a *run* cannot do
            // without one.
            None => {
                return Err(Error::session(
                    "No orchestrator agent added. The session loop is \
                     orchestrator-driven, so one is required even when the \
                     gameplan declares 'agents.orchestrator: false'. Call \
                     add_orchestrator(...).",
                ))
            }
        };

        // The harness decides whether this configuration is legal — role
        // requirements, participant count, and tool resolution all at once.
        let harness = self.gameplan.harness.clone();
        let registered: Vec<String> = lock(&self.shared.tools)
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        let allowed = validate_harness(
            &harness,
            &participants,
            Some(orchestrator.as_str()),
            &registered,
        )?;
        *lock(&self.shared.allowed_tools) = Some(allowed);

        // Personas resolve here, before a single provider call, for the same
        // reason tools and skills do: a configuration error should cost
        // nothing.
        self.resolve_personas()?;

        // Resolve which skills each agent may load. Only the index reaches the
        // prompt; bodies travel through the `Skill` tool.
        let mut cache = HashMap::new();
        for agent in &self.agents {
            cache.insert(agent.name.clone(), self.resolve_agent_skills(agent)?);
        }
        *lock(&self.shared.skills_cache) = cache;

        let orch_prompt = self.build_orchestrator_prompt(&participants)?;

        // Load memory files: session-level, then every agent keeping its own.
        {
            let mut memories = lock(&self.shared.memories);
            memories.session.load()?;
            memories.per_agent.clear();
            for agent in &self.agents {
                let Some(path) = &agent.memory else { continue };
                let mut memory = Memory::new(path);
                memory.load()?;
                memories.per_agent.insert(agent.name.clone(), memory);
            }
        }

        self.identity = identity_for(&self.gameplan.name, &self.topic, &participants, &orchestrator);

        // Continue a previous run, or seed the conversation with the topic.
        self.conversation = Conversation::new();
        let resume_state = self.resume()?;
        if resume_state.is_none() {
            self.conversation.directive(self.topic.clone());
        }

        self.orch_prompt = orch_prompt;
        self.participants = participants.clone();

        let mut loop_spec = harness.loop_spec.clone();
        loop_spec.max_rounds = self.max_rounds;
        let mut orchestrator_loop = OrchestratorLoop::new(loop_spec, &orchestrator, participants)
            .with_result_fields(harness.result.clone());
        if let Some(max_turns) = self.max_turns {
            orchestrator_loop = orchestrator_loop.with_max_turns(max_turns);
        }
        if let Some(retries) = self.orchestrator_retries {
            orchestrator_loop = orchestrator_loop.with_retries(retries);
        }
        if let Some(state) = resume_state {
            orchestrator_loop = orchestrator_loop.with_resume_state(state);
        }

        let state = orchestrator_loop.run(self)?;
        self.loop_position = orchestrator_loop.snapshot();
        self.save()?;

        if self.shared.memory_write {
            let mut block = format!(
                "## Session Result\n- Consensus: {}",
                pyfmt::repr(&Value::Bool(state.consensus_reached))
            );
            if !state.final_summary.is_empty() {
                block.push_str(&format!("\n- Summary: {}", state.final_summary));
            }
            lock(&self.shared.memories).session.append_entry(&block)?;
        }

        Ok(SessionResult {
            topic: self.topic.clone(),
            turns_completed: state.turn_count,
            consensus_reached: state.consensus_reached,
            history: self.conversation.transcript().to_vec(),
            final_summary: state.final_summary,
            fields: state.fields,
            rounds_run: state.rounds_run,
            phase_reached: state.phase_reached,
            end_reason: state.end_reason.as_str().to_string(),
        })
    }

    /// Run one agent's turn, tool loop included.
    ///
    /// The activation is started before the history is measured: it decides the
    /// turn's tool set, and the prompt overhead the history has to fit inside
    /// is measured against exactly that set.
    fn turn(
        &mut self,
        agent: &Agent,
        base_prompt: &str,
        purpose: &str,
        instruction: Option<&str>,
    ) -> Result<String> {
        self.shared.start_activation(&agent.name);
        let history = self.history_for_turn(agent, base_prompt)?;

        let provider = self.shared.provider_for(agent).ok_or_else(|| {
            Error::session(format!("No provider configured for agent '{}'.", agent.name))
        })?;
        let assembling = Arc::clone(&self.shared);
        let advertising = Arc::clone(&self.shared);
        let max_tool_iterations = self.max_tool_iterations;
        let mirror_exchanges = self.tool_results_in_history;

        let Session {
            dispatcher,
            conversation,
            ..
        } = self;
        let mut runner = AgentRunner::new(
            agent,
            provider.as_ref(),
            move |agent: &Agent, history: &[Value], base: &str| {
                assembling.prompts().messages_for(agent, history, base)
            },
            dispatcher,
            base_prompt,
        )
        .with_tools(move || advertising.active_tools());
        if let Some(limit) = max_tool_iterations {
            runner = runner.with_max_tool_iterations(limit);
        }
        if mirror_exchanges {
            runner = runner.with_record(move |message: &Value| {
                conversation.raw(
                    message.get("role").and_then(Value::as_str).unwrap_or_default(),
                    message
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
            });
        }
        runner.run(&history, purpose, instruction)
    }

    fn orchestrator_agent(&self) -> Result<Agent> {
        self.agents
            .iter()
            .find(|agent| agent.is_orchestrator())
            .cloned()
            .ok_or_else(|| Error::session("No orchestrator agent added."))
    }

    /// Construct the orchestrator's system prompt from the gameplan.
    ///
    /// The harness frontmatter is the machine contract and is *not* shown to
    /// the model — the orchestrator reads the Markdown body, plus the phase
    /// list rendered from `loop.phases`.
    ///
    /// An explicit orchestrator `system_prompt` replaces the whole thing,
    /// mirroring what [`Agent::build_system_prompt`] already does for
    /// participants.
    fn build_orchestrator_prompt(&self, participants: &[String]) -> Result<String> {
        let orchestrator = self.agents.iter().find(|agent| agent.is_orchestrator());
        if let Some(agent) = orchestrator {
            if !agent.system_prompt.is_empty() {
                return Ok(agent.system_prompt.replace("{topic}", &self.topic));
            }
        }

        let mut roster = String::new();
        for name in participants {
            let agent = self
                .agents
                .iter()
                .find(|agent| agent.is_participant() && &agent.name == name)
                .ok_or_else(|| Error::session(format!("Unknown participant: {name}")))?;
            if !roster.is_empty() {
                roster.push('\n');
            }
            roster.push_str(&format!("- {}", agent.name));
            if !agent.persona.is_empty() {
                roster.push_str(&format!(" ({})", persona_label(agent)?));
            }
        }

        let harness = &self.gameplan.harness;
        let spec = &harness.loop_spec;

        let mut phase_block = String::new();
        if !spec.phases.is_empty() {
            let mut lines = Vec::new();
            for (index, phase) in spec.phases.iter().enumerate() {
                let marker = if phase.rethink { " [rethink]" } else { "" };
                let plural = if phase.rounds == 1 { "" } else { "s" };
                lines.push(format!(
                    "{}. {} ({} round{plural}){marker}",
                    index + 1,
                    phase.name,
                    phase.rounds
                ));
                if !phase.instruction.is_empty() {
                    lines.push(format!("   Tell participants: {}", phase.instruction));
                }
            }
            phase_block = format!(
                "Phases, in order:\n{}\n\nThe session runs these phases in order \
                 and advances on its own: a round ends when every participant \
                 has spoken once, and each participant is given the active \
                 phase's instruction automatically. You will be told at each \
                 boundary which phase is active and who has yet to speak — call \
                 on those participants. A phase marked [rethink] asks \
                 participants to re-examine a position they already stated; \
                 press anyone who merely repeats themselves.\nWrite {} to end \
                 the current phase early, before its rounds are up.\n\n",
                lines.join("\n"),
                spec.advance_on
            );
        }

        let consensus = spec.consensus_keyword();
        let end_rules: String = spec
            .terminate_on
            .iter()
            .map(|keyword| {
                if consensus == Some(keyword.as_str()) {
                    format!("- If consensus is reached, include {keyword} in your response\n")
                } else {
                    format!("- When the session should end, include {keyword} in your response\n")
                }
            })
            .collect();

        // Only a phase-less harness gets round guidance. `max_rounds` is a
        // ceiling on any single phase, so telling a phased orchestrator to "aim
        // for around 3 rounds" would contradict the phase block directly above
        // it — `debate` declares max_rounds: 3 and phases summing to 5, and the
        // loop will run all five. Where there are no phases the number is the
        // real bound on the session, so it is stated.
        let rounds_rule = if spec.phases.is_empty() {
            format!(
                "- The session ends after {} rounds (every participant \
                 speaking once is one round)\n",
                self.max_rounds
            )
        } else {
            String::new()
        };

        // The harness's own words about itself, when it has any.
        let summary = if harness.description.is_empty() {
            String::new()
        } else {
            format!(" {}", harness.description)
        };
        // Same for agents.orchestrator.instruction, whose whole documented
        // purpose is to append to these rules.
        let extra = if harness.agents.orchestrator.instruction.is_empty() {
            String::new()
        } else {
            format!("\n{}\n", harness.agents.orchestrator.instruction)
        };

        let prompt = format!(
            "You are the orchestrator of a {} session.{summary}\n\n\
             Topic: {{topic}}\n\n\
             Participants:\n{roster}\n\n\
             {phase_block}\
             Your gameplan:\n{}\n\n\
             Rules:\n\
             - To have a participant speak, mention them with @ (e.g. @{}, \
             present your opening argument)\n\
             - You can give them specific instructions after the @ mention\n\
             - Only call on ONE participant at a time\n\
             {end_rules}\
             - You control the flow: decide who speaks, when to move phases, \
             when to summarize, when to check consensus\n\
             - You can summarize, comment, redirect, or challenge participants \
             at any point\n\
             {rounds_rule}\
             {extra}",
            self.gameplan.name, self.gameplan.body, participants[0],
        );
        Ok(prompt.replace("{topic}", &self.topic))
    }

    /// The skills one agent may load.
    ///
    /// `Agent::skills` is a tri-state: `None` inherits, a list selects exactly,
    /// an empty list opts out. Inheriting also picks up skills the harness
    /// declares — skills widen, see [`crate::harness`].
    fn resolve_agent_skills(&self, agent: &Agent) -> Result<Vec<SkillConfig>> {
        let Some(declared) = &agent.skills else {
            let session_names: Vec<String> =
                self.skills.iter().map(|skill| skill.name.clone()).collect();
            let names = self.gameplan.harness.resolve_skills(&session_names);
            return names
                .iter()
                .map(|name| {
                    match self.skills.iter().find(|skill| &skill.name == name) {
                        Some(skill) => Ok(skill.clone()),
                        None => load_skill(name),
                    }
                })
                .collect();
        };
        let mut resolved: Vec<SkillConfig> = Vec::new();
        for name in declared {
            let skill = load_skill(name)?;
            if !resolved.iter().any(|seen| seen.name == skill.name) {
                resolved.push(skill);
            }
        }
        Ok(resolved)
    }

    /// Pin every agent's persona to a file that exists, before the run.
    ///
    /// The search path is the session's to own, not the agent's: an [`Agent`]
    /// has no idea which gameplan it was registered against, and the gameplan's
    /// own directory is the one place a third-party project can put personas and
    /// expect the paths inside its gameplan to work from any working directory.
    ///
    /// Each resolved path is written back onto the agent as an absolute path, so
    /// persona lookup stays a plain read and the same file is used no matter
    /// what the working directory does mid-session.
    fn resolve_personas(&mut self) -> Result<()> {
        let search: Vec<PathBuf> = self.gameplan.directory().into_iter().collect();
        for agent in &mut self.agents {
            if !agent.persona.ends_with(".md") {
                continue;
            }
            // Naming the agent matters — the loader knows the path but not who
            // asked for it, and "which agent?" is the first question a caller
            // has.
            let resolved = resolve_persona_path(&agent.persona, &search).map_err(|err| {
                Error::session(format!(
                    "Agent '{}' has persona '{}' which could not be loaded. {err}",
                    agent.name, agent.persona
                ))
            })?;
            agent.persona = absolute(&resolved).display().to_string();
        }
        Ok(())
    }

    /// Refuse `tool_results_in_history` against a native provider.
    ///
    /// Mirroring a native tool exchange into the shared conversation puts one
    /// API's message shapes in front of every other agent, which is the exact
    /// corruption private scratch buffers exist to prevent. Failing here is
    /// better than failing on turn three with a 400 from the other provider.
    fn check_tool_history_dialect(&self) -> Result<()> {
        if !self.tool_results_in_history {
            return Ok(());
        }
        let mut native: Vec<&str> = self
            .agents
            .iter()
            .map(|agent| self.shared.dialect_for(agent))
            .filter(|dialect| *dialect != ToolDialect::Text)
            .map(ToolDialect::as_str)
            .collect();
        native.sort_unstable();
        native.dedup();
        if native.is_empty() {
            return Ok(());
        }
        Err(Error::session(format!(
            "tool_results_in_history=True is supported for the TEXT tool \
             dialect only, but this session has {} provider(s). Native tool \
             exchanges are not portable between APIs, so they stay private to \
             the agent that made them.",
            native.join(", ")
        )))
    }

    /// Strip `@MEMORY:` markers and append their notes to the agent's memory.
    ///
    /// Marker lines are stripped whether or not the session writes memory: a
    /// `@MEMORY:` line is an instruction to the framework, and it does not
    /// belong in the transcript even when there is nowhere to store it.
    fn process_memory_markers(&self, text: &str, agent_name: &str) -> Result<String> {
        let (cleaned, notes) = parse_memory_markers(text);
        if notes.is_empty() || !self.shared.memory_write {
            return Ok(cleaned);
        }
        let mut memories = lock(&self.shared.memories);
        let memory = memories.get_mut(agent_name);
        for note in &notes {
            memory.append_entry(note)?;
        }
        Ok(cleaned)
    }

    /// Record a message in the conversation and emit it to the channel.
    fn record_and_emit(
        &mut self,
        sender: &str,
        content: &str,
        turn_idx: i64,
        msg_type: &str,
    ) -> Result<()> {
        self.conversation.say(sender, content, turn_idx, msg_type);
        self.shared.channel.send(sender, content)?;
        // Every routed turn passes through here, which makes it the one place a
        // save covers the whole loop — including the retry path, which is where
        // the session is most likely to be interrupted.
        self.save()?;
        if !self.turn_delay.is_zero() {
            std::thread::sleep(self.turn_delay);
        }
        Ok(())
    }

    fn emit_system(&mut self, message: &str) -> Result<()> {
        self.conversation.note(message);
        self.shared.channel.send_system(message)
    }

    // ---- Session file and budget ----------------------------------------

    /// Load a previous run into the conversation, if there is one.
    ///
    /// `None` covers both "no session file configured" and "configured but
    /// nothing there yet".
    fn resume(&mut self) -> Result<Option<Map<String, Value>>> {
        let Some(path) = self.session_file.clone() else {
            return Ok(None);
        };
        let Some(snapshot) = load_snapshot(Path::new(&path))? else {
            return Ok(None);
        };

        // Resume is automatic, so this check is what stands between a stale
        // file and a run that silently inherits an unrelated conversation.
        check_identity(&snapshot.identity, &self.identity)?;

        let recorded = snapshot.transcript.len();
        let turn_count = snapshot
            .loop_state
            .get("turn_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        self.conversation
            .restore(snapshot.turns, snapshot.transcript);
        self.compactions = snapshot.compactions;
        // Said out loud, because nothing else about re-running a script signals
        // that it picked up where the last one stopped.
        self.emit_system(&format!(
            "Resumed from {path}: {turn_count} turns, {recorded} recorded messages."
        ))?;
        Ok(Some(snapshot.loop_state))
    }

    /// Write the run's state, if the caller asked for one.
    fn save(&self) -> Result<()> {
        let Some(path) = &self.session_file else {
            return Ok(());
        };
        save_snapshot(
            Path::new(path),
            &SessionSnapshot {
                identity: self.identity.clone(),
                turns: self.conversation.turns().to_vec(),
                transcript: self.conversation.transcript().to_vec(),
                loop_state: self.loop_position.clone(),
                compactions: self.compactions,
            },
        )
    }

    /// Estimate everything the request carries except the conversation.
    ///
    /// Rendered with an empty history because message assembly appends history
    /// after the system message and changes nothing else, so this is the real
    /// system message rather than a model of one.
    ///
    /// Native tool schemas travel in the request body instead of the system
    /// prompt, so they are counted separately. `tool_schemas` returns `None`
    /// under TEXT, where the tool definitions are already in the system message
    /// — counting them here as well would charge the caller for them twice.
    fn prompt_overhead(&self, agent: &Agent, base_prompt: &str) -> Result<usize> {
        let messages = self.shared.prompts().messages_for(agent, &[], base_prompt)?;
        let mut total: usize = messages
            .iter()
            .map(|message| {
                estimate_tokens(
                    message
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )
            })
            .sum();
        let schemas = tool_schemas(self.shared.dialect_for(agent), &self.shared.active_tools());
        if let Some(schemas) = schemas {
            total += estimate_tokens(&pyfmt::json_dumps(&Value::Array(schemas)));
        }
        Ok(total)
    }

    /// Render the conversation for a provider, compacting it if it grew.
    ///
    /// Checked before the call rather than after the previous one so that the
    /// turn about to be sent is the one that fits, not the one before it.
    ///
    /// `max_context_tokens` bounds the whole request, but the conversation is
    /// the only part of it compaction can shrink, so the fixed part is measured
    /// first and the remainder is what the conversation may use. Measured per
    /// turn rather than once: personas, skill indexes and permitted tool sets
    /// differ by agent, and the memory file grows during the run.
    fn history_for_turn(&mut self, agent: &Agent, base_prompt: &str) -> Result<Vec<Value>> {
        let overhead = self.prompt_overhead(agent, base_prompt)?;
        if overhead >= self.max_context_tokens {
            // Not a provider's problem to discover. Compaction cannot touch the
            // system prompt, so no amount of summarizing makes this fit;
            // continuing would buy a summary call per turn and still 400.
            return Err(Error::session(format!(
                "The prompt for {} needs about {overhead} tokens before any \
                 conversation, which already meets or exceeds \
                 max_context_tokens={}. Compaction shrinks the conversation \
                 only, so nothing it can do would make this request fit. Raise \
                 max_context_tokens, or cut the persona, skill index, tool set, \
                 or memory file.",
                agent.name, self.max_context_tokens
            )));
        }
        let available = self.max_context_tokens - overhead;

        let mut failure: Option<Error> = None;
        let compacted = compact(self.conversation.turns(), available, |turns| {
            match self.summarize(turns) {
                Ok(text) => text,
                Err(error) => {
                    failure = Some(error);
                    String::new()
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }

        if let Some(turns) = compacted {
            self.conversation.replace_turns(turns);
            self.compactions += 1;
            let message = format!(
                "Compacted conversation: {overhead} of {} estimated tokens are \
                 prompt overhead, leaving {available} for the conversation.",
                self.max_context_tokens
            );
            self.emit_system(&message)?;
            self.save()?;
        }
        Ok(as_values(&self.conversation.render()))
    }

    /// Summarize turns being dropped, using the orchestrator's model.
    ///
    /// A plain provider call, not an [`AgentRunner`] turn: a summarizer has no
    /// persona to hold, no skills to load, and nothing to gain from a tool
    /// loop. An empty string on failure is a real answer — `compact` reads it
    /// as "leave the conversation alone", which is the right outcome when the
    /// alternative is trading real turns for nothing.
    fn summarize(&self, turns: &[Turn]) -> Result<String> {
        let Ok(agent) = self.orchestrator_agent() else {
            return Ok(String::new());
        };
        let Some(provider) = self.shared.provider_for(&agent) else {
            return Ok(String::new());
        };
        let messages = as_values(&summary_request(turns));
        match provider.chat_with_retries(&agent.model, &messages, "compaction", None) {
            Ok(response) => Ok(response.content),
            Err(error) if error.is_provider() => {
                logging::warning("Compaction summary failed; leaving history intact");
                Ok(String::new())
            }
            Err(error) => Err(error),
        }
    }
}

// ---- LoopHost -----------------------------------------------------------
//
// The loop owns control flow; these methods are everything it is allowed to
// reach back for. Keeping the surface this narrow is what lets the loop be
// tested without a provider.

impl LoopHost for Session {
    fn orchestrator_turn(&mut self, purpose: &str) -> Result<String> {
        let agent = self.orchestrator_agent()?;
        let prompt = self.orch_prompt.clone();
        self.turn(&agent, &prompt, purpose, None)
    }

    fn participant_turn(&mut self, name: &str, instruction: &str) -> Result<String> {
        let agent = self
            .agents
            .iter()
            .find(|agent| agent.is_participant() && agent.name == name)
            .cloned()
            .ok_or_else(|| Error::session(format!("Unknown participant: {name}")))?;
        let prompt = self.system_prompt.clone();
        self.turn(
            &agent,
            &prompt,
            &format!("turn from {name}"),
            Some(instruction),
        )
    }

    /// The marker pass lives here rather than in the loop so that every routed
    /// turn gets it, including retries.
    fn deliver(&mut self, sender: &str, text: &str, turn: i64, msg_type: &str) -> Result<()> {
        let cleaned = self.process_memory_markers(text, sender)?;
        self.record_and_emit(sender, &cleaned, turn, msg_type)
    }

    fn note(&mut self, message: &str) -> Result<()> {
        self.emit_system(message)
    }

    fn directive(&mut self, text: &str) -> Result<()> {
        self.conversation.directive(text);
        Ok(())
    }

    fn closing_turn(&mut self, prompt: &str) -> Result<String> {
        self.conversation.directive(prompt);
        self.orchestrator_turn("final summary")
    }

    fn record_summary(&mut self, text: &str, turn: i64) -> Result<()> {
        let name = self.orchestrator_agent()?.name;
        self.conversation.say(&name, text, turn, "final_summary");
        self.shared.channel.send(&name, text)
    }

    fn record_position(&mut self, snapshot: Map<String, Value>) {
        self.loop_position = snapshot;
    }
}

/// How a participant's persona is named in the orchestrator's roster.
///
/// A file path is not a character description. Listing `- Alice
/// (pragmatic_engineer.md)` was already cosmetic noise; once personas resolve
/// to absolute paths it becomes a private filesystem path handed to a model as
/// if it said something about who Alice is. The persona file's own `# Persona:`
/// header is what it says. Inline personas are already prose and pass through
/// untouched.
fn persona_label(agent: &Agent) -> Result<String> {
    if !agent.persona.ends_with(".md") {
        return Ok(agent.persona.clone());
    }
    Ok(load_persona(&agent.persona, &[])?.name)
}

/// Run a command and report the attempt through the channel.
///
/// The channel owns where output goes and how it is presented. Printing
/// straight to stdout would mean a session configured with a `FileChannel` or a
/// caller's own remote channel recorded every turn *except* the commands agents
/// actually ran. An unnamed actor is a direct call by the host program, which
/// nobody needs told about.
fn run_and_log(
    access: &Mutex<AccessManager>,
    channel: &dyn Channel,
    command: &str,
    cwd: Option<&Path>,
    timeout: Option<Duration>,
    actor: &str,
) -> Result<String> {
    let outcome = exec::run_command(&lock(access), command, cwd, timeout, actor);
    if actor.is_empty() {
        return outcome;
    }
    let status = match &outcome {
        Ok(_) => "approved",
        Err(Error::AccessDenied(_)) => "denied",
        Err(_) => "failed",
    };
    channel.send_system(&format!("[Command:{status}] {actor}: {command}"))?;
    outcome
}

/// Register the built-in tools: `cmd`, `read_file`, `list_dir`, and memory.
///
/// These take the calling agent's name because the access prompt, the command
/// log, and per-agent memory all identify who asked; `add_tool` handlers do
/// not, so the specs are built here rather than through the public method.
///
/// `write_memory` is registered only for a writing session. Advertising a tool
/// whose every call would be discarded is worse than not offering it. Both
/// memory tools are ordinary registered tools rather than reserved
/// runtime-owned names, which is what lets a gameplan narrow them away through
/// its `tools:` list — `tools: [cmd, read_memory]` is read-only memory with no
/// extra machinery.
fn default_tools(
    access: &Arc<Mutex<AccessManager>>,
    memories: &Arc<Mutex<Memories>>,
    channel: &Arc<dyn Channel>,
    memory_write: bool,
) -> Vec<ToolSpec> {
    let mut tools = Vec::new();

    tools.push(
        ToolSpec::new(
            "cmd",
            "Run an allowed command without shell interpretation.",
            json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Command and arguments to execute.",
                    }
                },
                "required": ["command"],
            }),
            {
                let access = Arc::clone(access);
                let channel = Arc::clone(channel);
                Arc::new(move |arguments: &Arguments, actor: &str| {
                    run_and_log(
                        &access,
                        channel.as_ref(),
                        &argument(arguments, "command"),
                        None,
                        Some(exec::DEFAULT_TIMEOUT),
                        actor,
                    )
                })
            },
        )
        .with_actor(),
    );

    tools.push(
        ToolSpec::new(
            "read_file",
            "Read a file and return its contents.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file to read."}
                },
                "required": ["path"],
            }),
            {
                let access = Arc::clone(access);
                Arc::new(move |arguments: &Arguments, actor: &str| {
                    exec::read_file(&lock(&access), &argument(arguments, "path"), actor)
                })
            },
        )
        .with_actor(),
    );

    tools.push(
        ToolSpec::new(
            "list_dir",
            "List entries in a directory.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the directory."}
                },
                "required": ["path"],
            }),
            {
                let access = Arc::clone(access);
                Arc::new(move |arguments: &Arguments, actor: &str| {
                    let entries =
                        exec::list_dir(&lock(&access), &argument(arguments, "path"), actor)?;
                    Ok(entries.join(", "))
                })
            },
        )
        .with_actor(),
    );

    tools.push(
        ToolSpec::new(
            "read_memory",
            "Read this session's memory file and return it as written.",
            json!({"type": "object", "properties": {}}),
            {
                let memories = Arc::clone(memories);
                Arc::new(move |_arguments: &Arguments, actor: &str| {
                    let memories = lock(&memories);
                    let content = memories.get(actor).read().trim().to_string();
                    Ok(if content.is_empty() {
                        "(memory is empty)".to_string()
                    } else {
                        content
                    })
                })
            },
        )
        .with_actor(),
    );

    if memory_write {
        tools.push(
            ToolSpec::new(
                "write_memory",
                "Append a note to this session's memory file. The note is \
                 stored word for word: include whoever and whenever matters, \
                 because nothing is added around it.",
                json!({
                    "type": "object",
                    "properties": {
                        "note": {"type": "string", "description": "The note to store, as written."}
                    },
                    "required": ["note"],
                }),
                {
                    let memories = Arc::clone(memories);
                    Arc::new(move |arguments: &Arguments, actor: &str| {
                        lock(&memories)
                            .get_mut(actor)
                            .append_entry(&argument(arguments, "note"))?;
                        Ok("Saved to memory.".to_string())
                    })
                },
            )
            .with_actor(),
        );
    }

    tools
}

/// One string argument as the model supplied it.
fn argument(arguments: &Arguments, name: &str) -> String {
    arguments.get(name).map(pyfmt::str).unwrap_or_default()
}

/// Render chat messages the way a provider request carries them.
fn as_values(messages: &[ChatMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| json!({"role": message.role, "content": message.content}))
        .collect()
}

/// A path with the working directory prepended when it is relative.
///
/// Personas are pinned before the run so the same file is read no matter what
/// the working directory does mid-session. `canonicalize` would do more than
/// that — it also resolves symlinks, which would rewrite a path the caller
/// deliberately pointed through one.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::provider::{ProviderBase, ProviderResponse};

    /// A scratch directory that removes itself.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("kerness-session-{tag}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        fn join(&self, name: &str) -> String {
            self.0.join(name).display().to_string()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Replies in order, repeating the last one, recording every request.
    struct SequenceProvider {
        base: ProviderBase,
        dialect: ToolDialect,
        replies: Vec<String>,
        index: AtomicUsize,
        calls: Mutex<Vec<Vec<Value>>>,
    }

    impl SequenceProvider {
        fn new(replies: &[&str]) -> Self {
            SequenceProvider {
                base: ProviderBase::new(0, 0.0, None),
                dialect: ToolDialect::Text,
                replies: replies.iter().map(|reply| (*reply).to_string()).collect(),
                index: AtomicUsize::new(0),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn with_dialect(mut self, dialect: ToolDialect) -> Self {
            self.dialect = dialect;
            self
        }

        fn calls(&self) -> Vec<Vec<Value>> {
            lock(&self.calls).clone()
        }

        /// Every system prompt this provider was sent.
        fn system_prompts(&self) -> Vec<String> {
            self.calls()
                .iter()
                .filter_map(|messages| messages.first().cloned())
                .map(|message| {
                    message
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        }
    }

    impl Provider for SequenceProvider {
        fn name(&self) -> &str {
            "SequenceProvider"
        }

        fn base(&self) -> &ProviderBase {
            &self.base
        }

        fn tool_dialect(&self) -> ToolDialect {
            self.dialect
        }

        fn chat(
            &self,
            _model: &str,
            messages: &[Value],
            _tools: Option<&[ToolSpec]>,
        ) -> Result<ProviderResponse> {
            lock(&self.calls).push(messages.to_vec());
            if self.replies.is_empty() {
                return Ok(ProviderResponse::text("(no responses configured)"));
            }
            let index = self
                .index
                .fetch_add(1, Ordering::SeqCst)
                .min(self.replies.len() - 1);
            Ok(ProviderResponse::text(self.replies[index].clone()))
        }
    }

    /// A channel that records instead of printing.
    #[derive(Default)]
    struct CaptureChannel {
        messages: Mutex<Vec<(String, String)>>,
    }

    impl CaptureChannel {
        fn messages(&self) -> Vec<(String, String)> {
            lock(&self.messages).clone()
        }

        fn contains(&self, needle: &str) -> bool {
            self.messages()
                .iter()
                .any(|(_, text)| text.contains(needle))
        }
    }

    impl Channel for CaptureChannel {
        fn send(&self, sender: &str, message: &str) -> Result<()> {
            lock(&self.messages).push((sender.to_string(), message.to_string()));
            Ok(())
        }

        fn send_system(&self, message: &str) -> Result<()> {
            lock(&self.messages).push(("system".to_string(), message.to_string()));
            Ok(())
        }

        fn type_name(&self) -> String {
            "CaptureChannel".to_string()
        }
    }

    /// The two-participant debate every flow test runs.
    fn debate(
        temp: &TempDir,
        provider: Arc<dyn Provider>,
        channel: Arc<dyn Channel>,
        topic: &str,
    ) -> Session {
        let config = SessionConfig {
            topic: topic.to_string(),
            provider: Some(provider),
            channel: Some(channel),
            memory: temp.join("memory.md"),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("the debate gameplan loads");
        session.add_participant(Agent::new("Alice", "m"));
        session.add_participant(Agent::new("Bob", "m"));
        session
            .add_orchestrator(Agent::new("Mod", "m"))
            .expect("the first orchestrator is accepted");
        session
    }

    fn error_text(error: &Error) -> String {
        format!("{error}")
    }

    // ---- flow ----------------------------------------------------------

    #[test]
    fn the_closing_keyword_decides_how_the_session_ends() {
        // END_SESSION and CONSENSUS_REACHED are one mechanism reporting
        // opposite verdicts, so a flag pinned to either value passes half of
        // this and fails the other half.
        let temp = TempDir::new("keyword");
        let ended = {
            let provider = Arc::new(SequenceProvider::new(&[
                "Let's begin. @Alice, present your opening argument.",
                "I think pineapple is great on pizza.",
                "Interesting. @Bob, what is your response?",
                "I disagree, pineapple doesn't belong on pizza.",
                "Good discussion. END_SESSION",
                "Both sides presented. Alice favors pineapple, Bob opposes.",
            ]));
            let channel = Arc::new(CaptureChannel::default());
            debate(&temp, provider, channel, "Is pineapple acceptable on pizza?")
                .run()
                .expect("a run")
        };

        assert!(!ended.consensus_reached);
        assert!(ended.turns_completed >= 4);
        assert_eq!(ended.topic, "Is pineapple acceptable on pizza?");
        assert!(!ended.final_summary.is_empty());
        assert!(ended.history.iter().any(|message| message.msg_type == "turn"));
        assert_eq!(ended.end_reason, "keyword");

        let agreed = {
            let provider = Arc::new(SequenceProvider::new(&[
                "@Alice, share your view.",
                "Pineapple is fine.",
                "@Bob, your thoughts?",
                "I agree with Alice.",
                "Both participants agree. CONSENSUS_REACHED",
                "Consensus: pineapple is acceptable on pizza.",
            ]));
            let channel = Arc::new(CaptureChannel::default());
            debate(&temp, provider, channel, "Is pineapple acceptable on pizza?")
                .run()
                .expect("a run")
        };

        assert!(agreed.consensus_reached);
        assert!(!agreed.final_summary.is_empty());
    }

    #[test]
    fn unparseable_output_is_retried_and_then_forced_to_an_end() {
        let temp = TempDir::new("unparseable");
        let provider = Arc::new(SequenceProvider::new(&[
            "Hmm, let me think about this topic.",
            "I need to consider all angles here.",
            "This is a complex topic indeed.",
            "The session ended without resolution.",
        ]));
        let channel = Arc::new(CaptureChannel::default());
        let mut session = debate(&temp, provider, Arc::clone(&channel) as Arc<dyn Channel>, "T");
        session.orchestrator_retries = Some(2);

        let result = session.run().expect("a run");

        assert!(!result.consensus_reached);
        assert!(result
            .history
            .iter()
            .any(|message| message.msg_type == "system"
                && message.content.contains("Forcing END_SESSION")));
    }

    #[test]
    fn max_turns_stops_the_session() {
        let temp = TempDir::new("max-turns");
        let provider = Arc::new(SequenceProvider::new(&[
            "@Alice, speak.",
            "Response from Alice.",
        ]));
        let channel = Arc::new(CaptureChannel::default());
        let mut session = debate(&temp, provider, channel, "T");
        session.max_turns = Some(6);
        session.orchestrator_retries = Some(0);

        let result = session.run().expect("a run");

        assert!(result.turns_completed <= 6);
        assert!(!result.final_summary.is_empty());
    }

    #[test]
    fn the_counters_carry_what_their_names_say() {
        let temp = TempDir::new("counters");
        let provider = Arc::new(SequenceProvider::new(&[
            "@Alice, go.",
            "Mine.",
            "@Bob, go.",
            "Mine too.",
            "END_SESSION",
            "Done.",
        ]));
        let channel = Arc::new(CaptureChannel::default());
        let result = debate(&temp, provider, channel, "T").run().expect("a run");

        // Rounds and turns count different things: a round closes only once
        // every participant has spoken.
        assert!(result.rounds_run <= result.turns_completed);
        // The phase the run stopped in, which is the gameplan's, not a guess.
        assert!(["think", "argue", "cross_question", "rethink"]
            .contains(&result.phase_reached.as_str()));
        // Every field the gameplan declares is present, whether or not the
        // orchestrator cooperated.
        assert_eq!(
            result.fields.keys().collect::<Vec<_>>(),
            vec!["consensus", "summary"]
        );
        assert_eq!(result.summary(), result.final_summary);
    }

    // ---- roster and errors ----------------------------------------------

    #[test]
    fn a_second_orchestrator_is_refused_by_name() {
        let mut session = Session::new(SessionConfig {
            topic: "T".to_string(),
            ..SessionConfig::default()
        })
        .expect("the debate gameplan loads");
        session
            .add_orchestrator(Agent::new("Mod1", "m"))
            .expect("the first is accepted");

        let error = session
            .add_orchestrator(Agent::new("Mod2", "m"))
            .map(|_| ())
            .expect_err("the second is refused");

        assert!(error_text(&error).contains("already has an orchestrator: 'Mod1'"));
    }

    #[test]
    fn each_missing_piece_of_a_run_is_named_by_the_error() {
        let temp = TempDir::new("preflight");
        let base = || SessionConfig {
            topic: "T".to_string(),
            channel: Some(Arc::new(CaptureChannel::default())),
            memory: temp.join("memory.md"),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };

        // No provider anywhere, and the error names who is missing one.
        let mut session = Session::new(SessionConfig {
            provider: None,
            ..base()
        })
        .expect("loads");
        session.add_participant(Agent::new("Alice", "m"));
        let error = error_text(&session.run().expect_err("no provider"));
        assert!(error.contains("No provider configured"));
        assert!(error.contains("Missing: Alice"));

        // No topic.
        let mut session = Session::new(SessionConfig {
            topic: String::new(),
            provider: Some(Arc::new(SequenceProvider::new(&["x"]))),
            ..base()
        })
        .expect("loads");
        session.add_participant(Agent::new("Alice", "m"));
        assert!(error_text(&session.run().expect_err("no topic")).contains("No topic set."));

        // No participants.
        let mut session = Session::new(SessionConfig {
            provider: Some(Arc::new(SequenceProvider::new(&["x"]))),
            ..base()
        })
        .expect("loads");
        session
            .add_orchestrator(Agent::new("Mod", "m"))
            .expect("accepted");
        assert!(error_text(&session.run().expect_err("no participants"))
            .contains("No participant agents added."));

        // No orchestrator, which no harness can waive.
        let mut session = Session::new(SessionConfig {
            provider: Some(Arc::new(SequenceProvider::new(&["x"]))),
            ..base()
        })
        .expect("loads");
        session.add_participant(Agent::new("Alice", "m"));
        session.add_participant(Agent::new("Bob", "m"));
        let error = error_text(&session.run().expect_err("no orchestrator"));
        assert!(error.contains("No orchestrator agent added."));
        assert!(error.contains("add_orchestrator"));
    }

    #[test]
    fn add_tool_refuses_a_name_it_could_not_honour() {
        let temp = TempDir::new("add-tool");
        let mut session = Session::new(SessionConfig {
            topic: "T".to_string(),
            memory: temp.join("memory.md"),
            ..SessionConfig::default()
        })
        .expect("loads");
        let handler: Arc<dyn ToolHandler> =
            Arc::new(|_: &Arguments, _: &str| Ok("ok".to_string()));
        let schema = json!({"type": "object", "properties": {}});

        // The runtime builds `Skill` per agent; shadowing it would disable
        // skill loading with no diagnostic.
        let error = session
            .add_tool(SKILL_TOOL_NAME, "d", schema.clone(), Arc::clone(&handler))
            .map(|_| ())
            .expect_err("the reserved name is refused");
        assert!(error_text(&error).contains("reserved tool name"));

        session
            .add_tool("mine", "d", schema.clone(), Arc::clone(&handler))
            .expect("a fresh name is accepted");
        let error = session
            .add_tool("mine", "d", schema.clone(), Arc::clone(&handler))
            .map(|_| ())
            .expect_err("a duplicate is refused");
        assert!(error_text(&error).contains("already registered"));

        // A built-in name collides on the same rule.
        let error = session
            .add_tool("cmd", "d", schema, handler)
            .map(|_| ())
            .expect_err("a built-in name is refused");
        assert!(error_text(&error).contains("already registered"));
    }

    #[test]
    fn zero_is_a_max_rounds_the_caller_can_set_and_omitting_it_is_not() {
        // `None` takes the gameplan's number; `Some(0)` is a real answer.
        let inherited = Session::new(SessionConfig {
            topic: "T".to_string(),
            max_rounds: None,
            ..SessionConfig::default()
        })
        .expect("loads");
        assert_eq!(inherited.max_rounds(), 3);

        let zero = Session::new(SessionConfig {
            topic: "T".to_string(),
            max_rounds: Some(0),
            ..SessionConfig::default()
        })
        .expect("loads");
        assert_eq!(zero.max_rounds(), 0);
    }

    // ---- the orchestrator prompt ----------------------------------------

    #[test]
    fn the_orchestrator_prompt_is_built_from_the_gameplan() {
        let temp = TempDir::new("prompt");
        let provider = Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]));
        let channel = Arc::new(CaptureChannel::default());
        debate(
            &temp,
            Arc::clone(&provider) as Arc<dyn Provider>,
            channel,
            "Pineapple?",
        )
        .run()
        .expect("a run");

        let prompt = provider.system_prompts().remove(0);
        assert!(prompt.starts_with("You are the orchestrator of a debate session."));
        assert!(prompt.contains("Topic: Pineapple?"));
        assert!(prompt.contains("Participants:\n- Alice\n- Bob"));
        // The Markdown body is the manual; the YAML contract is not shown.
        assert!(prompt.contains("Your gameplan:"));
        assert!(!prompt.contains("terminate_on"));
        // Both declared terminators are named, and consensus is named as such.
        assert!(prompt.contains("include END_SESSION in your response"));
        assert!(prompt.contains("If consensus is reached, include CONSENSUS_REACHED"));
        // Phases replace the round target, and name the advance keyword.
        assert!(prompt.contains("Phases, in order:\n1. think (1 round)"));
        assert!(prompt.contains("4. rethink (1 round) [rethink]"));
        assert!(prompt.contains("Write NEXT_PHASE to end the current phase early"));
        assert!(!prompt.contains("The session ends after"));
    }

    #[test]
    fn an_explicit_orchestrator_prompt_replaces_the_gameplan_one() {
        let temp = TempDir::new("prompt-override");
        let provider = Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]));
        let channel = Arc::new(CaptureChannel::default());
        let config = SessionConfig {
            topic: "Rust or Go?".to_string(),
            provider: Some(Arc::clone(&provider) as Arc<dyn Provider>),
            channel: Some(channel),
            memory: temp.join("memory.md"),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("loads");
        session.add_participant(Agent::new("Alice", "m"));
        session.add_participant(Agent::new("Bob", "m"));
        let mut orchestrator = Agent::new("Mod", "m");
        orchestrator.system_prompt = "Judge this: {topic}".to_string();
        session.add_orchestrator(orchestrator).expect("accepted");

        session.run().expect("a run");

        // The override replaces the gameplan prompt entirely; the toolkit is
        // appended after it as it is for any system prompt.
        let prompt = provider.system_prompts().remove(0);
        assert!(prompt.starts_with("Judge this: Rust or Go?"));
        assert!(!prompt.contains("You are the orchestrator"));
    }

    #[test]
    fn the_roster_names_the_character_never_the_path() {
        // A filesystem path says nothing about who Alice is, and once personas
        // resolve to absolute paths it is a private path handed to a model.
        let temp = TempDir::new("roster");
        let provider = Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]));
        let channel = Arc::new(CaptureChannel::default());
        let config = SessionConfig {
            topic: "T".to_string(),
            provider: Some(Arc::clone(&provider) as Arc<dyn Provider>),
            channel: Some(channel),
            memory: temp.join("memory.md"),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("loads");
        let mut alice = Agent::new("Alice", "m");
        alice.persona = "pragmatic_engineer.md".to_string();
        let mut bob = Agent::new("Bob", "m");
        bob.persona = "A cautious lawyer".to_string();
        session.add_participant(alice);
        session.add_participant(bob);
        session
            .add_orchestrator(Agent::new("Mod", "m"))
            .expect("accepted");

        session.run().expect("a run");

        let prompt = provider.system_prompts().remove(0);
        assert!(prompt.contains("- Alice (Pragmatic Engineer)"));
        assert!(prompt.contains("- Bob (A cautious lawyer)"));
        assert!(!prompt.contains(".md"));
    }

    #[test]
    fn a_missing_persona_stops_the_run_before_it_costs_anything() {
        let temp = TempDir::new("persona-missing");
        let provider = Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]));
        let config = SessionConfig {
            topic: "T".to_string(),
            provider: Some(Arc::clone(&provider) as Arc<dyn Provider>),
            channel: Some(Arc::new(CaptureChannel::default())),
            memory: temp.join("memory.md"),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("loads");
        let mut alice = Agent::new("Alice", "m");
        alice.persona = "nope_not_here.md".to_string();
        session.add_participant(alice);
        session.add_participant(Agent::new("Bob", "m"));
        session
            .add_orchestrator(Agent::new("Mod", "m"))
            .expect("accepted");

        let error = error_text(&session.run().expect_err("the persona is missing"));

        assert!(error.contains("Agent 'Alice' has persona 'nope_not_here.md'"));
        assert!(provider.calls().is_empty());
    }

    // ---- access and the command log --------------------------------------

    #[test]
    fn the_exec_setter_replaces_the_command_patterns() {
        let mut session = Session::new(SessionConfig {
            topic: "T".to_string(),
            ..SessionConfig::default()
        })
        .expect("loads");
        assert!(session.exec().is_empty());

        session.set_exec(vec![r"^echo\s".to_string()]);

        assert_eq!(session.exec(), vec![r"^echo\s".to_string()]);
        session
            .run_command("echo hi", None, None, "")
            .expect("the pattern allows it");
    }

    #[test]
    fn a_named_actor_puts_both_verdicts_on_the_channel() {
        let channel = Arc::new(CaptureChannel::default());
        let mut policy = AccessPolicy::new();
        policy.allowed_commands = vec!["echo hi".to_string()];
        let session = Session::new(SessionConfig {
            topic: "T".to_string(),
            channel: Some(Arc::clone(&channel) as Arc<dyn Channel>),
            access_policy: Some(policy),
            ..SessionConfig::default()
        })
        .expect("loads");

        session
            .run_command("echo hi", None, None, "Alice")
            .expect("allowed");
        session
            .run_command("rm -rf /", None, None, "Alice")
            .expect_err("denied");

        assert!(channel.contains("[Command:approved] Alice: echo hi"));
        assert!(channel.contains("[Command:denied] Alice: rm -rf /"));

        // An unnamed actor is the host program calling directly, which nobody
        // needs told about.
        let before = channel.messages().len();
        let _ = session.run_command("echo hi", None, None, "");
        assert_eq!(channel.messages().len(), before);
    }

    // ---- tool exchange privacy -------------------------------------------

    #[test]
    fn tool_exchanges_are_private_unless_the_switch_reverses_it() {
        let temp = TempDir::new("privacy");
        let call = "```tool_calls\n\
                    {\"tool_calls\":[{\"id\":\"c1\",\"type\":\"function\",\
                    \"function\":{\"name\":\"read_memory\",\"arguments\":\"{}\"}}]}\n\
                    ```";
        let replies: Vec<String> = vec![
            "@Alice, go.".to_string(),
            call.to_string(),
            "My view.".to_string(),
            "END_SESSION".to_string(),
            "Done.".to_string(),
        ];
        let borrowed: Vec<&str> = replies.iter().map(String::as_str).collect();

        let private = {
            let provider = Arc::new(SequenceProvider::new(&borrowed));
            let channel = Arc::new(CaptureChannel::default());
            debate(&temp, provider, channel, "T").run().expect("a run")
        };
        assert!(private
            .history
            .iter()
            .all(|message| message.msg_type != "raw"));

        let shared = {
            let provider = Arc::new(SequenceProvider::new(&borrowed));
            let channel = Arc::new(CaptureChannel::default());
            let mut session = debate(&temp, provider, channel, "T");
            session.tool_results_in_history = true;
            session.run().expect("a run")
        };

        // The transcript is the public record and never carries tool traffic;
        // the switch only changes what the *conversation* replays.
        assert!(shared
            .history
            .iter()
            .all(|message| message.msg_type != "raw"));
        assert!(shared.turns_completed >= private.turns_completed);
    }

    #[test]
    fn tool_history_is_refused_against_a_native_provider() {
        let temp = TempDir::new("native-guard");
        let provider =
            Arc::new(SequenceProvider::new(&["END_SESSION"]).with_dialect(ToolDialect::Openai));
        let channel = Arc::new(CaptureChannel::default());
        let mut session = debate(&temp, provider, channel, "T");
        session.tool_results_in_history = true;

        let error = error_text(&session.run().expect_err("the combination is refused"));

        assert!(error.contains("tool_results_in_history=True is supported for the TEXT"));
        assert!(error.contains("openai"));
    }

    #[test]
    fn either_half_alone_is_fine() {
        // Only the combination is refused. Text plus the switch runs, and a
        // native provider without the switch runs.
        let temp = TempDir::new("native-halves");
        let provider = Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]));
        let channel = Arc::new(CaptureChannel::default());
        let mut session = debate(&temp, provider, channel, "T");
        session.tool_results_in_history = true;
        session.run().expect("text plus the switch runs");

        let provider =
            Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]).with_dialect(ToolDialect::Openai));
        let channel = Arc::new(CaptureChannel::default());
        debate(&temp, provider, channel, "T")
            .run()
            .expect("a native provider without the switch runs");
    }

    // ---- memory ----------------------------------------------------------

    #[test]
    fn a_marker_is_always_stripped_and_only_sometimes_saved() {
        // A `@MEMORY:` line is an instruction to the framework. It leaves the
        // transcript whether or not there is anywhere to store it.
        let temp = TempDir::new("markers");
        let replies = [
            "@Alice, go.",
            "My view.\n@MEMORY: Alice prefers Rust.",
            "END_SESSION",
            "Done.",
        ];

        let read_only = {
            let provider = Arc::new(SequenceProvider::new(&replies));
            let channel = Arc::new(CaptureChannel::default());
            let mut session = debate(&temp, provider, channel, "T");
            let result = session.run().expect("a run");
            (result, session.memory())
        };
        let spoken = read_only
            .0
            .history
            .iter()
            .find(|message| message.sender == "Alice")
            .expect("Alice spoke")
            .content
            .clone();
        assert_eq!(spoken, "My view.");
        assert!(!read_only.1.contains("Alice prefers Rust"));

        let memory_path = temp.join("written.md");
        let writing = {
            let provider = Arc::new(SequenceProvider::new(&replies));
            let config = SessionConfig {
                topic: "T".to_string(),
                provider: Some(provider),
                channel: Some(Arc::new(CaptureChannel::default())),
                memory: memory_path.clone(),
                memory_write: true,
                turn_delay: Duration::ZERO,
                ..SessionConfig::default()
            };
            let mut session = Session::new(config).expect("loads");
            session.add_participant(Agent::new("Alice", "m"));
            session.add_participant(Agent::new("Bob", "m"));
            session
                .add_orchestrator(Agent::new("Mod", "m"))
                .expect("accepted");
            session.run().expect("a run")
        };
        let written = std::fs::read_to_string(&memory_path).expect("the memory file exists");

        // Stored word for word, with nothing wrapped around it.
        assert!(written.contains("Alice prefers Rust."));
        // And the result block, which only a writing session records.
        assert!(written.contains("## Session Result"));
        assert!(written.contains("- Consensus: False"));
        assert!(!writing.final_summary.is_empty());
        assert!(written.contains("- Summary: "));
    }

    #[test]
    fn write_memory_is_offered_only_to_a_writing_session() {
        // Advertising a tool whose every call would be discarded is worse than
        // not offering it.
        let temp = TempDir::new("memory-tools");
        let read_only = Session::new(SessionConfig {
            topic: "T".to_string(),
            memory: temp.join("memory.md"),
            ..SessionConfig::default()
        })
        .expect("loads");
        let names: Vec<String> = read_only
            .shared
            .active_tools()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        assert!(names.contains(&"read_memory".to_string()));
        assert!(!names.contains(&"write_memory".to_string()));

        let writing = Session::new(SessionConfig {
            topic: "T".to_string(),
            memory: temp.join("memory.md"),
            memory_write: true,
            ..SessionConfig::default()
        })
        .expect("loads");
        let names: Vec<String> = writing
            .shared
            .active_tools()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        assert!(names.contains(&"write_memory".to_string()));
    }

    #[test]
    fn read_memory_returns_the_file_or_says_there_is_none() {
        let temp = TempDir::new("read-memory");
        let path = temp.join("memory.md");
        let mut session = Session::new(SessionConfig {
            topic: "T".to_string(),
            memory: path.clone(),
            memory_write: true,
            ..SessionConfig::default()
        })
        .expect("loads");
        session.add_participant(Agent::new("Alice", "m"));

        let tools = session.shared.active_tools();
        let read = tools
            .iter()
            .find(|tool| tool.name == "read_memory")
            .expect("read_memory is registered");
        let write = tools
            .iter()
            .find(|tool| tool.name == "write_memory")
            .expect("write_memory is registered");

        assert_eq!(
            read.handler.call(&Map::new(), "Alice").expect("a read"),
            "(memory is empty)"
        );

        let mut note = Map::new();
        note.insert("note".to_string(), Value::String("Ship it.".to_string()));
        assert_eq!(
            write.handler.call(&note, "Alice").expect("a write"),
            "Saved to memory."
        );

        assert_eq!(
            read.handler.call(&Map::new(), "Alice").expect("a read"),
            "Ship it."
        );
        assert!(std::fs::read_to_string(&path)
            .expect("the file exists")
            .contains("Ship it."));
    }

    // ---- harness narrowing ------------------------------------------------

    #[test]
    fn the_tools_key_decides_what_the_prompt_advertises() {
        let temp = TempDir::new("narrowing");
        let gameplan = temp.join("narrow.md");
        std::fs::write(
            &gameplan,
            "---\nname: narrow\nagents:\n  orchestrator:\n    required: true\n  \
             participants:\n    min: 1\nloop:\n  max_rounds: 1\n  terminate_on: \
             [END_SESSION]\ntools: [read_memory]\n---\n\n# Narrow\n\nGo.\n",
        )
        .expect("write gameplan");

        let provider = Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]));
        let config = SessionConfig {
            gameplan: gameplan.clone(),
            topic: "T".to_string(),
            provider: Some(Arc::clone(&provider) as Arc<dyn Provider>),
            channel: Some(Arc::new(CaptureChannel::default())),
            memory: temp.join("memory.md"),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("loads");
        session.add_participant(Agent::new("Alice", "m"));
        session
            .add_orchestrator(Agent::new("Mod", "m"))
            .expect("accepted");
        session.run().expect("a run");

        let prompt = provider.system_prompts().remove(0);
        assert!(prompt.contains("read_memory"));
        assert!(!prompt.contains("read_file"));
    }

    #[test]
    fn a_gameplan_naming_an_unregistered_tool_fails_the_session() {
        let temp = TempDir::new("unregistered");
        let gameplan = temp.join("ghost.md");
        std::fs::write(
            &gameplan,
            "---\nname: ghost\nagents:\n  orchestrator:\n    required: true\n  \
             participants:\n    min: 1\nloop:\n  max_rounds: 1\n  terminate_on: \
             [END_SESSION]\ntools: [nonexistent]\n---\n\n# Ghost\n\nGo.\n",
        )
        .expect("write gameplan");

        let provider = Arc::new(SequenceProvider::new(&["END_SESSION"]));
        let config = SessionConfig {
            gameplan,
            topic: "T".to_string(),
            provider: Some(Arc::clone(&provider) as Arc<dyn Provider>),
            channel: Some(Arc::new(CaptureChannel::default())),
            memory: temp.join("memory.md"),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("loads");
        session.add_participant(Agent::new("Alice", "m"));
        session
            .add_orchestrator(Agent::new("Mod", "m"))
            .expect("accepted");

        assert!(session.run().is_err());
        assert!(provider.calls().is_empty());
    }

    // ---- the session file --------------------------------------------------

    #[test]
    fn no_session_file_writes_nothing() {
        let temp = TempDir::new("no-file");
        let provider = Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]));
        let channel = Arc::new(CaptureChannel::default());
        debate(&temp, provider, channel, "T").run().expect("a run");

        // Nothing at all: a read-only session does not create its memory file
        // either, and no session file was asked for.
        let files: Vec<String> = std::fs::read_dir(&temp.0)
            .expect("the temp dir exists")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(files.is_empty(), "unexpected files: {files:?}");
    }

    #[test]
    fn a_second_run_picks_up_where_the_first_stopped() {
        let temp = TempDir::new("resume");
        let path = temp.join("run.json");

        let run = |replies: &[&str]| -> SessionResult {
            let provider = Arc::new(SequenceProvider::new(replies));
            let config = SessionConfig {
                topic: "T".to_string(),
                provider: Some(provider),
                channel: Some(Arc::new(CaptureChannel::default())),
                memory: temp.join("memory.md"),
                session_file: Some(path.clone()),
                turn_delay: Duration::ZERO,
                ..SessionConfig::default()
            };
            let mut session = Session::new(config).expect("loads");
            session.add_participant(Agent::new("Alice", "m"));
            session.add_participant(Agent::new("Bob", "m"));
            session
                .add_orchestrator(Agent::new("Mod", "m"))
                .expect("accepted");
            session.run().expect("a run")
        };

        let first = run(&["@Alice, go.", "Mine.", "END_SESSION", "Done."]);
        assert!(Path::new(&path).exists());

        let second = run(&["@Bob, go.", "Mine too.", "END_SESSION", "Done."]);

        // Resumption is the caller re-running with the same file, so the second
        // run continues the first rather than starting over.
        assert!(second.turns_completed > first.turns_completed);
        assert!(second.history.len() > first.history.len());
        assert!(second
            .history
            .iter()
            .any(|message| message.content.contains("Resumed from")));
    }

    #[test]
    fn a_file_written_for_another_topic_refuses_to_resume() {
        // Resume is automatic, so this check is what stands between a stale
        // file and a run that silently inherits an unrelated conversation.
        let temp = TempDir::new("identity");
        let path = temp.join("run.json");
        let run = |topic: &str| -> Result<SessionResult> {
            let provider = Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]));
            let config = SessionConfig {
                topic: topic.to_string(),
                provider: Some(provider),
                channel: Some(Arc::new(CaptureChannel::default())),
                memory: temp.join("memory.md"),
                session_file: Some(path.clone()),
                turn_delay: Duration::ZERO,
                ..SessionConfig::default()
            };
            let mut session = Session::new(config).expect("loads");
            session.add_participant(Agent::new("Alice", "m"));
            session.add_participant(Agent::new("Bob", "m"));
            session
                .add_orchestrator(Agent::new("Mod", "m"))
                .expect("accepted");
            session.run()
        };

        run("First topic").expect("the first run");
        let error = error_text(&run("A different topic").expect_err("the identity differs"));

        assert!(error.contains("written for a different run: topic was"));
    }

    // ---- the context ceiling ------------------------------------------------

    #[test]
    fn a_prompt_over_the_limit_before_any_turns_fails_loudly() {
        // Compaction cannot touch the system prompt, so no amount of
        // summarizing makes this fit.
        let temp = TempDir::new("overhead");
        let provider = Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]));
        let channel = Arc::new(CaptureChannel::default());
        let mut session = debate(
            &temp,
            Arc::clone(&provider) as Arc<dyn Provider>,
            channel,
            "T",
        );
        session.max_context_tokens = 10;

        let error = error_text(&session.run().expect_err("the prompt cannot fit"));

        assert!(error.contains("tokens before any conversation"));
        assert!(error.contains("max_context_tokens=10"));
        assert!(provider.calls().is_empty());
    }

    #[test]
    fn a_generous_limit_leaves_the_conversation_alone() {
        let temp = TempDir::new("generous");
        let provider = Arc::new(SequenceProvider::new(&[
            "@Alice, go.",
            "Mine.",
            "END_SESSION",
            "Done.",
        ]));
        let channel = Arc::new(CaptureChannel::default());
        debate(&temp, provider, Arc::clone(&channel) as Arc<dyn Channel>, "T")
            .run()
            .expect("a run");

        assert!(!channel.contains("Compacted conversation:"));
    }
}
