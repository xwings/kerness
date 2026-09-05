//! Session configuration, registration, and preparation for [`SessionRun`].
//! [`Session::run`] drives the owned engine with compatibility defaults;
//! [`Session::start`] transfers control to the host. [`LoopHost`] remains an
//! adapter for callers driving an [`OrchestratorLoop`] directly.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::access::{AccessManager, AccessPolicy};
use crate::agent::{Agent, AgentDefaults};
use crate::agent_runtime::AgentRunner;
use crate::channel::{Channel, ConsoleChannel};
use crate::compaction::{compact, estimate_tokens, summary_request};
use crate::context::ContextSource;
use crate::conversation::{ChatMessage, Conversation, Message, Turn};
use crate::error::{Error, Result};
use crate::exec;
use crate::gameplan::{load_gameplan, GameplanConfig};
use crate::harness::{validate_harness, Permitted};
use crate::logging;
use crate::memory::{FileMemory, MemoryFilter, MemoryStore};
use crate::orchestrator::{LoopHost, OrchestratorLoop};
use crate::persona::{load_persona, resolve_persona_path};
use crate::prompting::PromptAssembler;
use crate::provider::{Provider, ReasoningEffort};
use crate::pyfmt;
use crate::role::{load_role, role_file};
use crate::sessionfile::{
    check_identity, identity_for, load_snapshot, save_snapshot, SessionSnapshot,
};
use crate::skill::loader::{load_skill, SkillConfig};
use crate::skill::runtime::{
    admit_required, apply_gate, format_skills_index, GrantPaths, SkillActivation, SkillRegistry,
    SkillsFor, SKILL_TOOL_NAME,
};
use crate::tooling::{Arguments, ToolHandler, ToolSpec};
use crate::toolkit::{resolve, ToolDispatcher, ToolsFor};
use crate::toolschema::{tool_schemas, ToolDialect};
use crate::utils::parse_memory_markers;

mod capabilities;
mod outcome;
mod run;

pub use capabilities::{
    ContextToolHandler, ContextToolSpec, PreflightAction, ToolContext, ToolIdentity,
};
pub use outcome::{ResultDiagnostics, ResultIssue, ResultValidation};
pub use run::{
    ApprovalMode, ApprovalRequest, EventSink, RunControl, RunEvent, RunEventKind, RunInput,
    RunMode, RunOptions, RunOutcome, RunReason, SessionRun, StepOutcome, WaitReason,
};

/// Default ceiling on one request, in estimated tokens.
///
/// This stands for what the model can hold, so it is not the framework's number
/// to pick well — a caller running a 128k model should say so, either here or
/// through [`Provider::context_window`], whichever is smaller binding. The
/// default is deliberately generous rather than clever: guessing per model would
/// mean shipping a table of context windows that goes stale every release and
/// has no entry at all for `CustomProvider`.
pub const DEFAULT_MAX_CONTEXT_TOKENS: usize = 256_000;

/// How much of the estimated allowance the conversation is compacted to after a
/// provider refuses a request for being too long.
///
/// The estimate said the request fit and the provider says it did not, so the
/// retry cannot be measured against the same figure — it would find nothing to
/// do. Half is the same step [`COMPACT_TO_FRACTION`](crate::compaction::COMPACT_TO_FRACTION)
/// takes for the same reason: big enough that one retry is likely to be the only
/// one, small enough not to throw the conversation away over a heuristic that
/// was slightly off.
pub const OVERFLOW_RETRY_FRACTION: f64 = 0.5;

/// Result of a completed session.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
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
    /// The final summary, under the shorter name the Python bindings expose.
    pub fn summary(&self) -> &str {
        &self.final_summary
    }
}

/// Session configuration. `memory_write` and `access_policy` determine the
/// toolkit and access manager at construction; agent defaults resolve at start.
#[derive(Clone)]
pub struct SessionConfig {
    /// Gameplan name or path.
    pub gameplan: String,
    /// The topic or question for the session.
    pub topic: String,
    /// Backend for agents that bring none of their own.
    pub provider: Option<Arc<dyn Provider>>,
    /// Model for agents that name none of their own.
    ///
    /// Read as a model on [`SessionConfig::provider`], so an agent that brings
    /// its own provider does not inherit it — that agent must name its own
    /// model. See [`Agent::inherit`].
    pub model: Option<String>,
    /// How hard agents that ask for no level of their own should think.
    pub reasoning_effort: ReasoningEffort,
    /// Persona for agents that bring none of their own. `None` pins none.
    pub persona: Option<String>,
    /// Language for agents that name none of their own. `None` pins none.
    pub language: Option<String>,
    /// Where output goes while the run is in progress.
    pub channel: Option<Arc<dyn Channel>>,
    /// The session-level memory scope, which the store interprets.
    ///
    /// Under the default store this is the path to a `.md` file, and the file
    /// is yours: it is free-form prose, it is read as empty when absent, and
    /// nothing is created or templated on your behalf. Under another store it
    /// is whatever that store names a collection by.
    pub memory: String,
    /// Where memory is kept, or `None` for [`FileMemory`] — a Markdown file per
    /// scope, which is what a session does when the caller says nothing.
    pub memory_store: Option<Arc<dyn MemoryStore>>,
    /// Whether the session may write to memory. Off by default, which makes a
    /// run read-only: the `write_memory` tool is not offered, `@MEMORY:` notes
    /// are dropped, and the session result is not recorded.
    pub memory_write: bool,
    /// Filter over what agents write to memory, or `None` to store notes as
    /// written.
    ///
    /// Only consulted when [`SessionConfig::memory_write`] is on, because a
    /// read-only session stores nothing to filter. Applied before the store
    /// sees a note, so installing a store cannot route around it. See
    /// [`MemoryFilter`] for why the framework ships no default implementation.
    pub memory_filter: Option<Arc<dyn MemoryFilter>>,
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
    /// Base system prompt for participants that name no role of their own.
    ///
    /// `None` leaves each agent reading the role it named, or the built-in
    /// `participant` role when it named none. An agent's own `role` is more
    /// specific than a session-wide prompt and wins over this; an agent's own
    /// `system_prompt` wins over both.
    pub system_prompt: Option<String>,
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
            model: None,
            reasoning_effort: ReasoningEffort::default(),
            persona: None,
            language: None,
            channel: None,
            memory: "memory.md".to_string(),
            memory_store: None,
            memory_write: false,
            memory_filter: None,
            session_file: None,
            max_context_tokens: DEFAULT_MAX_CONTEXT_TOKENS,
            access_policy: None,
            max_rounds: None,
            max_turns: None,
            max_tool_iterations: None,
            turn_delay: Duration::from_secs(1),
            show_reasoning: None,
            system_prompt: None,
            orchestrator_retries: None,
            tool_results_in_history: false,
        }
    }
}

/// A session's memory: one store, and the scope each agent addresses it by.
///
/// Public so a caller can hold the session's memory rather than a copy of what
/// it said last: it is written during the run, and a snapshot taken before it
/// is stale by the time the run ends.
///
/// Preparation fills the scope map from agents with their own memory. An agent
/// absent from it shares the session scope.
pub struct Memories {
    pub store: Arc<dyn MemoryStore>,
    pub session_scope: String,
    pub agent_scopes: HashMap<String, String>,
}

impl Memories {
    /// The scope an agent reads and writes: its own when it declared one.
    pub fn scope_for(&self, agent_name: &str) -> &str {
        self.agent_scopes
            .get(agent_name)
            .unwrap_or(&self.session_scope)
    }

    /// Every distinct scope this session addresses, session-level first.
    ///
    /// Deduplicated because an agent naming the same scope as the session is a
    /// supported thing to do, and a store should not have to tolerate being
    /// told to open one scope twice.
    fn scopes(&self) -> Vec<&str> {
        let mut scopes = vec![self.session_scope.as_str()];
        for scope in self.agent_scopes.values() {
            if !scopes.contains(&scope.as_str()) {
                scopes.push(scope);
            }
        }
        scopes
    }
}

/// Store *note* against *agent_name*, after *filter* has seen it.
///
/// The one path model output takes into memory. Both the `write_memory` tool
/// and the `@MEMORY:` marker pass call this, so a caller that installs a filter
/// cannot have it cover one and miss the other, and no store can be reached
/// around it — a free function rather than a method because the tool handler
/// outlives any borrow of the session and holds only the `Arc`s.
///
/// Returns whether the note was stored. A filter that returns `None` drops it,
/// and the writer learns only that; a rejection saying *which* rule refused it
/// would teach a model how to word the next attempt.
fn remember(
    memories: &Mutex<Memories>,
    filter: Option<&Arc<dyn MemoryFilter>>,
    agent_name: &str,
    note: &str,
) -> Result<bool> {
    let note = match filter {
        Some(filter) => match filter.filter(note, agent_name) {
            Some(note) => note,
            None => return Ok(false),
        },
        None => note.to_string(),
    };
    let (store, scope) = store_for(memories, agent_name);
    store.append(&scope, &note)?;
    Ok(true)
}

/// Replace the entry *old* addresses with *new*, after *filter* has seen it.
///
/// The revising half of [`remember`], and here for the same reason: *new* is
/// model output that lands in every other agent's system prompt, so a store
/// reached around the filter through this path would be a route around it. The
/// store is cloned out from under the lock before the call for `remember`'s
/// reason as well.
///
/// A removal — an empty *new* — is not filtered and is always attempted. The
/// filter's contract is the text to store, and a removal stores none; a filter
/// asked to vet one would have nothing to answer about.
fn revise_memory(
    memories: &Mutex<Memories>,
    filter: Option<&Arc<dyn MemoryFilter>>,
    agent_name: &str,
    old: &str,
    new: &str,
) -> Result<bool> {
    let new = match (new.trim().is_empty(), filter) {
        (false, Some(filter)) => match filter.filter(new, agent_name) {
            Some(new) => new,
            None => return Ok(false),
        },
        _ => new.to_string(),
    };
    let (store, scope) = store_for(memories, agent_name);
    store.revise(&scope, old, &new)?;
    Ok(true)
}

/// The store and the scope *agent_name* addresses it by, taken together.
///
/// Every reader wants both and neither is useful alone, and taking them in one
/// lock is what keeps the lock off the store call that follows.
fn store_for(memories: &Mutex<Memories>, agent_name: &str) -> (Arc<dyn MemoryStore>, String) {
    let memories = lock(memories);
    (
        Arc::clone(&memories.store),
        memories.scope_for(agent_name).to_string(),
    )
}

/// The parts of a session that outlive a borrow of it.
///
/// Tool handlers are `Arc<dyn ToolHandler>` and the dispatcher's tool source is
/// an `Arc<dyn Fn>`: both are `Send + Sync + 'static`, so neither can hold a
/// reference to the session that built them. What a built-in handler actually
/// needs — the access manager, the memory files, the channel — lives here
/// instead, behind one `Arc` every handler closes over.
struct Shared {
    channel: Arc<dyn Channel>,
    provider: Option<Arc<dyn Provider>>,
    show_reasoning: Option<bool>,
    memory_write: bool,
    access: Arc<Mutex<AccessManager>>,
    memories: Arc<Mutex<Memories>>,
    tools: Mutex<Vec<ToolSpec>>,
    /// Tool names the harness permits, resolved during preparation.
    /// `None` means "not yet resolved"; an empty list is a real answer (a
    /// gameplan declaring `tools: []` grants none), so the two cannot share a
    /// representation.
    allowed_tools: Mutex<Option<Vec<String>>>,
    /// Tool names each agent that declared its own may call. An agent absent
    /// from the map declared nothing and takes the
    /// harness-permitted set whole.
    agent_tools: Mutex<HashMap<String, Vec<String>>>,
    /// The agent whose turn is in progress, recorded alongside its activation.
    /// The dispatcher is `'static` and shared by every agent, so this is how a
    /// per-agent narrowing reaches it.
    turn_agent: Mutex<Option<String>>,
    /// The skill activation for the turn in progress. Replaced at the start of
    /// every turn, which is what bounds a loaded body — and the tool gate it
    /// may carry — to that turn.
    activation: Mutex<Option<Arc<SkillActivation>>>,
    skills_cache: Arc<Mutex<HashMap<String, Vec<SkillConfig>>>>,
    skills_registry: SkillRegistry,
    /// Context blocks rendered once per agent during preparation, so sources
    /// are not called again for each prompt.
    context_cache: Mutex<HashMap<String, Vec<(String, String)>>>,
    /// Filter over what agents write to memory; `None` stores notes as written.
    memory_filter: Option<Arc<dyn MemoryFilter>>,
}

/// Recover access to session state after a lock holder unwinds.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared {
    /// The resolved skills an agent may load.
    fn skills_for(&self, agent_name: &str) -> Vec<SkillConfig> {
        lock(&self.skills_cache)
            .get(agent_name)
            .cloned()
            .unwrap_or_default()
    }

    /// The standing context blocks an agent reads, in registration order.
    fn context_for(&self, agent_name: &str) -> Vec<(String, String)> {
        lock(&self.context_cache)
            .get(agent_name)
            .cloned()
            .unwrap_or_default()
    }

    /// Memory for the prompt. Read failures are logged and omit the block;
    /// they do not abort the agent's turn.
    fn memory_text(&self, agent_name: &str) -> String {
        let (store, scope) = store_for(&self.memories, agent_name);
        match store.read(&scope) {
            Ok(text) => text,
            Err(err) => {
                logging::warning(&format!("Could not read memory for {agent_name}: {err}"));
                String::new()
            }
        }
    }

    /// How old an agent's memory is, in whole days.
    fn memory_age(&self, agent_name: &str) -> Option<u64> {
        let (store, scope) = store_for(&self.memories, agent_name);
        store.age(&scope)
    }

    /// Store *note* against *agent_name*, after the memory filter has seen it.
    fn remember(&self, agent_name: &str, note: &str) -> Result<bool> {
        remember(
            &self.memories,
            self.memory_filter.as_ref(),
            agent_name,
            note,
        )
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
    /// Five steps, applied in order, and the order matters:
    ///
    /// 1. The harness narrows the registered set. Before `run()` resolves it
    ///    this is every registered tool; after, it is the permitted set.
    /// 2. The turn's agent narrows what the harness left, if it declared a
    ///    list of its own. Static, like the harness's, and applied next to it.
    /// 3. The turn's `Skill` tool is added, built from the agent's own skill
    ///    list so its `enum` names only skills that agent can load.
    /// 4. An active skill's `allowed-tools` narrows further, restrictively.
    /// 5. An active skill's `requires-tools` adds back, out of the registered
    ///    set. Last, because a skill that brings instructions for a tool must
    ///    outrank every narrowing above it — otherwise a gameplan's `tools:`
    ///    list turns the skill into prose about something the agent cannot
    ///    call. An agent's own list is outranked the same way, and the skill
    ///    that does it is one that agent chose to load.
    ///
    /// Both the prompt and the dispatcher read this, so a tool the gameplan
    /// excluded is neither advertised to the model nor callable if it asks
    /// anyway — and the same holds for a tool an active skill gated out.
    fn active_tools(&self) -> Vec<ToolSpec> {
        let registered = lock(&self.tools);
        let tools = resolve(&registered, lock(&self.allowed_tools).as_deref());
        let tools = {
            let declared = lock(&self.agent_tools);
            let turn_agent = lock(&self.turn_agent);
            let names = turn_agent.as_deref().and_then(|name| declared.get(name));
            resolve(&tools, names.map(Vec::as_slice))
        };
        let Some(activation) = lock(&self.activation).clone() else {
            return tools;
        };
        let mut tools = tools;
        if let Some(skill_tool) = self.skills_registry.build_tool(&activation) {
            tools.push(skill_tool);
        }
        let tools = apply_gate(&tools, activation.gate().as_ref());
        admit_required(tools, &registered, &activation.required())
    }

    /// Start a fresh activation for one agent's turn, and record whose turn it
    /// is so `active_tools` can find that agent's own tool list.
    fn start_activation(&self, agent_name: &str) {
        *lock(&self.turn_agent) = Some(agent_name.to_string());
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
        .with_memory_age(|agent: &Agent| self.memory_age(&agent.name))
        .with_context(|agent: &Agent| self.context_for(&agent.name))
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
    system_prompt: Option<String>,
    /// What each agent takes for the options it leaves unset. Applied once by
    /// [`Session::resolve_agents`] at the top of the run.
    defaults: AgentDefaults,
    tool_results_in_history: bool,
    max_context_tokens: usize,
    session_file: Option<String>,
    compactions: i64,

    agents: Vec<Agent>,
    skills: Vec<SkillConfig>,
    /// Registered context sources, in registration order — which is the order
    /// their blocks appear in a prompt. Kept here rather than in `Shared`
    /// because only preparation calls them.
    context: Vec<(String, Arc<dyn ContextSource>)>,
    conversation: Conversation,
    dispatcher: ToolDispatcher,
    shared: Arc<Shared>,

    /// Resolved prompt and checkpoint identity, populated during preparation.
    orch_prompt: String,
    identity: Map<String, Value>,
    /// Loaded or last published scheduler position, used by the compatibility
    /// adapter's saves. The owned run checkpoints its live scheduler directly.
    loop_position: Map<String, Value>,
    contextual_tools: HashMap<String, Arc<dyn ContextToolHandler>>,
    store_open: bool,
    prepared_prompts: HashMap<String, String>,
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
        let manager = AccessManager::new(policy);
        let store: Arc<dyn MemoryStore> = config
            .memory_store
            .unwrap_or_else(|| Arc::new(FileMemory::new()));
        // The session's own files are not allowlisted — they are the caller's
        // choices, not something a model asked for — but a workspace that let them
        // sit outside it would confine only the half of the run that goes
        // through a tool. Checked here so a misplaced path fails at
        // construction rather than on the first write, mid-turn.
        //
        // The store names the file, because only it knows whether the scope is
        // one. A store keeping nothing on disk answers `None` and is checked
        // against nothing, exactly as a channel writing no file is.
        if let Some(path) = store.path(&config.memory) {
            manager.check_path("The memory file", &path.display().to_string(), "")?;
        }
        if let Some(session_file) = &config.session_file {
            manager.check_path("The session file", session_file, "")?;
        }
        for path in channel.paths() {
            manager.check_path(
                &format!("The {} destination", channel.type_name()),
                &path.display().to_string(),
                "",
            )?;
        }
        let access = Arc::new(Mutex::new(manager));
        let memories = Arc::new(Mutex::new(Memories {
            store,
            session_scope: config.memory,
            agent_scopes: HashMap::new(),
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

        let memory_filter = config.memory_filter;
        let shared = Arc::new(Shared {
            tools: Mutex::new(default_tools(
                &access,
                &memories,
                &channel,
                config.memory_write,
                memory_filter.as_ref(),
            )),
            channel,
            provider: config.provider,
            show_reasoning: config.show_reasoning,
            memory_write: config.memory_write,
            access,
            memories,
            allowed_tools: Mutex::new(None),
            agent_tools: Mutex::new(HashMap::new()),
            turn_agent: Mutex::new(None),
            activation: Mutex::new(None),
            skills_cache,
            skills_registry: SkillRegistry::new(skills_for, Some(grant_paths)),
            context_cache: Mutex::new(HashMap::new()),
            memory_filter,
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
            defaults: AgentDefaults {
                model: config.model,
                reasoning_effort: config.reasoning_effort,
                persona: config.persona,
                language: config.language,
            },
            tool_results_in_history: config.tool_results_in_history,
            max_context_tokens: config.max_context_tokens,
            session_file: config.session_file,
            compactions: 0,
            agents: Vec::new(),
            skills: Vec::new(),
            context: Vec::new(),
            conversation: Conversation::new(),
            dispatcher,
            shared,
            orch_prompt: String::new(),
            identity: Map::new(),
            loop_position: Map::new(),
            contextual_tools: HashMap::new(),
            store_open: false,
            prepared_prompts: HashMap::new(),
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

    /// The session-level memory, read through the store.
    pub fn memory(&self) -> Result<String> {
        let (store, scope) = {
            let memories = lock(&self.shared.memories);
            (Arc::clone(&memories.store), memories.session_scope.clone())
        };
        store.read(&scope)
    }

    /// The session's memory itself, for a caller that needs to read a scope
    /// after the run rather than at the moment it asked.
    pub fn memories(&self) -> Arc<Mutex<Memories>> {
        Arc::clone(&self.shared.memories)
    }

    /// The rounds limit in force, after the gameplan's default is applied.
    pub fn max_rounds(&self) -> i64 {
        self.max_rounds
    }

    /// The agents registered so far.
    pub fn agents(&self) -> &[Agent] {
        &self.agents
    }

    /// Add an agent to the session, seated by the role it named.
    ///
    /// [`Agent::role`] is the whole of the choice: a built-in role name, a path
    /// to a `.md` role file, or prose. Whichever it is, the file's
    /// `position:` frontmatter — or `participant`, for prose and for an agent
    /// that named no role — decides where the agent sits, and that answer is
    /// written onto [`Agent::position`] here.
    ///
    /// Read now rather than at [`Session::run`], unlike every option in
    /// [`AgentDefaults`], because there is nothing to wait for: role has no
    /// session-level default to inherit, so a mistyped path or a second
    /// orchestrator is knowable at the call that made it and reported there.
    /// A `.md` spec is pinned to an absolute path, resolved against the
    /// gameplan's own directory, so the same file is read for the rest of the
    /// run no matter what the working directory does.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when the role names a file that does not resolve,
    /// and [`Error::Session`] when the role seats a second orchestrator.
    pub fn add_agent(&mut self, agent: Agent) -> Result<&mut Self> {
        let mut agent = agent;
        let search: Vec<PathBuf> = self.gameplan.directory().into_iter().collect();
        if let Some(spec) = agent.role.clone() {
            // Naming the agent matters — the loader knows the path but not who
            // asked for it, and "which agent?" is the first question a caller
            // has.
            let found = role_file(&spec, &search).map_err(|err| {
                Error::session(format!(
                    "Agent '{}' has role '{spec}' which could not be loaded. {err}",
                    agent.name
                ))
            })?;
            if let Some(path) = found {
                agent.position = load_role(&path.display().to_string(), &[])?.position;
                agent.role = Some(absolute(&path).display().to_string());
            }
        }
        if agent.is_orchestrator() {
            if let Some(existing) = self.agents.iter().find(|a| a.is_orchestrator()) {
                return Err(Error::session(format!(
                    "Session already has an orchestrator: '{}'. Only one \
                     orchestrator is allowed.",
                    existing.name
                )));
            }
        }
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
        self.add_tool_spec(ToolSpec::new(name, description, parameters, handler))
    }

    /// Register a complete specification, preserving its actor contract.
    pub fn add_tool_spec(&mut self, spec: ToolSpec) -> Result<&mut Self> {
        let name = spec.name.as_str();
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
        tools.push(spec);
        drop(tools);
        Ok(self)
    }

    /// Register a tool whose capabilities and identity are supplied by the run.
    pub fn add_contextual_tool(&mut self, tool: ContextToolSpec) -> Result<&mut Self> {
        let name = tool.name.clone();
        self.add_tool_spec(
            ToolSpec::new(
                &name,
                tool.description,
                tool.parameters,
                Arc::new(|_: &Arguments, _: &str| {
                    Err(Error::session(
                        "This tool requires a running session context.",
                    ))
                }),
            )
            .with_actor(),
        )?;
        self.contextual_tools.insert(name, tool.handler);
        Ok(self)
    }

    /// Register standing background text for agents to read.
    ///
    /// *name* becomes the subheading the block arrives under, and a gameplan's
    /// `context:` key narrows by it. The source is called once per agent at the
    /// start of a run, in registration order.
    ///
    /// # Errors
    ///
    /// [`Error::Session`] when *name* is empty, or when a source of that name
    /// is already registered — two blocks under one heading would leave an
    /// agent no way to say which it is quoting, and a gameplan no way to name
    /// one of them.
    pub fn add_context(&mut self, name: &str, source: Arc<dyn ContextSource>) -> Result<&mut Self> {
        if name.trim().is_empty() {
            return Err(Error::session(
                "A context source needs a name: it becomes the heading its \
                 block arrives under and the name a gameplan's 'context:' key \
                 narrows by.",
            ));
        }
        if self.context.iter().any(|(existing, _)| existing == name) {
            return Err(Error::session(format!(
                "A context source named '{name}' is already registered. \
                 Context source names must be unique."
            )));
        }
        self.context.push((name.to_string(), source));
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
    /// Drives [`SessionRun`] with legacy result, approval and provider-error
    /// behavior, then returns the committed result.
    pub fn run(&mut self) -> Result<SessionResult> {
        let mut run = SessionRun::new(self.run_copy(), RunOptions::legacy(), true)?;
        let finished = run.run_to_completion();
        *self = run.session.run_copy();
        match finished? {
            RunOutcome {
                error: Some(error), ..
            } => Err(error),
            outcome => Ok(outcome.result),
        }
    }

    /// Prepare an owned run. Consuming the session freezes its configuration;
    /// subsequent control goes through the returned run and its control handle.
    pub fn start(self, options: RunOptions) -> Result<SessionRun> {
        SessionRun::new(self, options, false)
    }

    // Used only by the blocking adapter while it exclusively borrows self.
    // No independently accessible Session shares mutable configuration with a run.
    fn run_copy(&self) -> Self {
        Self {
            gameplan: self.gameplan.clone(),
            topic: self.topic.clone(),
            max_rounds: self.max_rounds,
            max_turns: self.max_turns,
            max_tool_iterations: self.max_tool_iterations,
            orchestrator_retries: self.orchestrator_retries,
            turn_delay: self.turn_delay,
            system_prompt: self.system_prompt.clone(),
            defaults: self.defaults.clone(),
            tool_results_in_history: self.tool_results_in_history,
            max_context_tokens: self.max_context_tokens,
            session_file: self.session_file.clone(),
            compactions: self.compactions,
            agents: self.agents.clone(),
            skills: self.skills.clone(),
            context: self.context.clone(),
            conversation: self.conversation.clone(),
            dispatcher: ToolDispatcher::new({
                let shared = Arc::clone(&self.shared);
                Arc::new(move || shared.active_tools())
            }),
            shared: Arc::clone(&self.shared),
            orch_prompt: self.orch_prompt.clone(),
            identity: self.identity.clone(),
            loop_position: self.loop_position.clone(),
            contextual_tools: self.contextual_tools.clone(),
            store_open: false,
            prepared_prompts: self.prepared_prompts.clone(),
        }
    }

    fn prepare(&mut self, mode: RunMode, legacy: bool) -> Result<OrchestratorLoop> {
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
        self.resolve_agents()?;
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
            None if mode == RunMode::HostDriven => String::new(),
            None => {
                return Err(Error::session(
                    "No orchestrator agent added. The session loop is \
                     orchestrator-driven, so one is required even when the \
                     gameplan declares 'agents.orchestrator: false'. Add an \
                     agent whose role is 'orchestrator'.",
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
        let registered_context: Vec<String> =
            self.context.iter().map(|(name, _)| name.clone()).collect();
        let permitted = validate_harness(
            &harness,
            &participants,
            (!orchestrator.is_empty()).then_some(orchestrator.as_str()),
            &registered,
            &registered_context,
        )?;
        let Permitted { tools, context } = permitted;
        self.resolve_agent_tools(&tools)?;
        *lock(&self.shared.allowed_tools) = Some(tools);

        // Render the permitted sources once per agent, here rather than per
        // prompt: a source that walks a tree would otherwise pay for it several
        // times a turn, and one that fails would do so mid-run.
        self.resolve_context(&context)?;

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
        check_required_tools(&cache, &registered)?;
        *lock(&self.shared.skills_cache) = cache;

        self.prepared_prompts.clear();
        for agent in &self.agents {
            if agent.is_participant() {
                self.prepared_prompts
                    .insert(agent.name.clone(), self.participant_prompt(agent)?);
            }
        }
        let orch_prompt = if orchestrator.is_empty() {
            String::new()
        } else {
            self.build_orchestrator_prompt(&participants)?
        };

        // Collect the scopes: session-level, then every agent keeping its own,
        // and open each through the store. Opening here rather than at the
        // first read is what makes an unreadable scope fail before a single
        // provider call has been paid for.
        let store = Arc::clone(&lock(&self.shared.memories).store);
        let mut agent_scopes = HashMap::new();
        for agent in &self.agents {
            let Some(scope) = &agent.memory else { continue };
            if let Some(path) = store.path(scope) {
                lock(&self.shared.access).check_path(
                    &format!("The memory file for {}", agent.name),
                    &path.display().to_string(),
                    &agent.name,
                )?;
            }
            agent_scopes.insert(agent.name.clone(), scope.clone());
        }
        let scopes = {
            let mut memories = lock(&self.shared.memories);
            memories.agent_scopes = agent_scopes;
            memories
                .scopes()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        self.store_open = true;
        for scope in &scopes {
            store.open(scope)?;
        }

        self.identity = identity_for(
            &self.gameplan.name,
            &self.topic,
            &participants,
            &orchestrator,
        );

        // Continue a previous run, or seed the conversation with the topic.
        self.conversation = Conversation::new();
        let resume_state = self.resume()?.map(|mut state| {
            // The blocking API historically continued at the saved phase with
            // a new closing verdict, including when its turn ceiling increased.
            if legacy {
                let terminal = state.get("runtime").and_then(|value| value.get("terminal"));
                let kind = terminal
                    .and_then(|value| value.get("reason"))
                    .and_then(|value| value.get("kind"))
                    .and_then(Value::as_str);
                if matches!(kind, Some("completed" | "invalid_result")) {
                    state.retain(|key, _| key == "turn_count" || key == "phases");
                } else if terminal.is_some_and(|value| !value.is_null()) {
                    if let Some(runtime) = state.get_mut("runtime").and_then(Value::as_object_mut) {
                        runtime.insert("terminal".into(), Value::Null);
                    }
                }
            }
            state
        });
        if resume_state.is_none() {
            self.conversation.directive(self.topic.clone());
        }

        self.orch_prompt = orch_prompt;

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
            self.loop_position = state.clone();
            orchestrator_loop = orchestrator_loop.with_resume_state(state);
        }

        Ok(orchestrator_loop)
    }

    /// Run one agent's turn, and compact and try again if the provider says the
    /// request was too long.
    ///
    /// The token count the pre-turn check works from is a character heuristic,
    /// so the provider is the authority and it disagrees sometimes. Once, not in
    /// a loop: a second refusal means the shortfall is not in the conversation,
    /// and going round again would buy a summary call per attempt and be refused
    /// each time.
    fn turn(
        &mut self,
        agent: &Agent,
        base_prompt: &str,
        purpose: &str,
        instruction: Option<&str>,
    ) -> Result<String> {
        match self.attempt_turn(agent, base_prompt, purpose, instruction) {
            Err(error) if error.is_context_overflow() => {
                logging::warning(&format!(
                    "Provider refused {purpose} as too long; compacting to \
                     {OVERFLOW_RETRY_FRACTION} of the estimated allowance and \
                     trying once more."
                ));
                self.fit_conversation(agent, base_prompt, OVERFLOW_RETRY_FRACTION)?;
                self.attempt_turn(agent, base_prompt, purpose, instruction)
            }
            other => other,
        }
    }

    /// Run one agent's turn, tool loop included.
    ///
    /// The activation is started before the history is measured: it decides the
    /// turn's tool set, and the prompt overhead the history has to fit inside
    /// is measured against exactly that set.
    fn attempt_turn(
        &mut self,
        agent: &Agent,
        base_prompt: &str,
        purpose: &str,
        instruction: Option<&str>,
    ) -> Result<String> {
        self.shared.start_activation(&agent.name);
        self.fit_conversation(agent, base_prompt, 1.0)?;
        let history = as_values(&self.conversation.render());

        let provider = self.shared.provider_for(agent).ok_or_else(|| {
            Error::session(format!(
                "No provider configured for agent '{}'.",
                agent.name
            ))
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
                    message
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    message
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
            });
        }
        runner.run(&history, purpose, instruction)
    }

    /// The base system prompt one participant starts from.
    ///
    /// Three answers, most specific first. A role the agent named is that
    /// agent's own job description, so it beats a prompt written once for the
    /// whole session; `SessionConfig::system_prompt` is the answer for
    /// everyone who named no role; and the built-in `participant` role is the
    /// answer when nobody said anything at all.
    ///
    /// [`Agent::system_prompt`] sits above all three and is applied later, by
    /// [`Agent::build_system_prompt`], which is what keeps an agent's own
    /// prompt beating an agent's own role.
    fn participant_prompt(&self, agent: &Agent) -> Result<String> {
        if let Some(prompt) = self.prepared_prompts.get(&agent.name) {
            return Ok(prompt.clone());
        }
        if agent.role.is_none() {
            if let Some(prompt) = &self.system_prompt {
                return Ok(prompt.clone());
            }
        }
        agent.resolve_role()
    }

    fn orchestrator_agent(&self) -> Result<Agent> {
        self.agents
            .iter()
            .find(|agent| agent.is_orchestrator())
            .cloned()
            .ok_or_else(|| Error::session("No orchestrator agent added."))
    }

    /// Fill the orchestrator's role template from the gameplan and the harness.
    ///
    /// The layout and every literal word belong to the role file — the
    /// built-in `orchestrator` role, or whichever one the agent named. What
    /// this builds is the set of values only the harness contract knows: the
    /// roster, the phase block, the end and flow rules. They are substituted by
    /// name, which is the same `{topic}`/`{bot_name}` mechanism
    /// [`Agent::decorate_system_prompt`] already applies to every prompt.
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
            if let Some(prompt) = &agent.system_prompt {
                return Ok(prompt.replace("{topic}", &self.topic));
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
            if agent.persona.is_some() {
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

        // Flow control is the orchestrator's where there are no phases. Where
        // there are, the loop owns it: phases advance when every participant
        // has spoken, and the briefing names who is still owed a turn. Telling
        // a phased orchestrator it decides "when to move phases" and may
        // "summarize at any point" contradicts the phase block directly above
        // it, and the contradiction resolves the expensive way — the
        // orchestrator stops calling the roster and starts writing the missing
        // participants' contributions itself, which reads exactly like a real
        // round and is not one.
        let flow_rules = if spec.phases.is_empty() {
            "- You control the flow: decide who speaks, when to move phases, \
             when to summarize, when to check consensus\n\
             - You can summarize, comment, redirect, or challenge participants \
             at any point\n"
        } else {
            "- You decide who speaks next; the phases advance on their own\n\
             - Call on the participants your briefing lists as yet to speak, \
             one per turn, until none are left\n\
             - Never write a participant's turn for them: until a participant \
             has answered you do not know what it found, and a contribution you \
             compose on its behalf is invention\n"
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

        let template = match orchestrator {
            Some(agent) => agent.resolve_role()?,
            None => return Err(Error::session("No orchestrator agent added.")),
        };
        // Ordered, and the gameplan body goes in last of the two that carry
        // caller text: a body holding `{roster}` is the gameplan's own words
        // about itself, not a request to be handed the cast list. `{topic}` is
        // the exception and stays global, because a gameplan writing `{topic}`
        // into its body is exactly how a gameplan asks for the topic.
        let prompt = [
            ("{gameplan}", self.gameplan.name.as_str()),
            ("{gameplan_description}", summary.as_str()),
            ("{roster}", roster.as_str()),
            ("{phases}", phase_block.as_str()),
            ("{first_participant}", participants[0].as_str()),
            ("{end_rules}", end_rules.as_str()),
            ("{flow_rules}", flow_rules),
            ("{rounds_rule}", rounds_rule.as_str()),
            ("{orchestrator_instruction}", extra.as_str()),
            ("{gameplan_body}", self.gameplan.body.as_str()),
        ]
        .into_iter()
        .fold(template, |prompt, (placeholder, replacement)| {
            prompt.replace(placeholder, replacement)
        });
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
                .map(
                    |name| match self.skills.iter().find(|skill| &skill.name == name) {
                        Some(skill) => Ok(skill.clone()),
                        None => load_skill(name),
                    },
                )
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

    /// Record the tool list of every agent that declared one, against the set
    /// the harness permits.
    ///
    /// Agents narrow, so a name outside *permitted* is refused rather than
    /// granted: an agent list is a way of paying for fewer schemas and calling
    /// fewer tools, never a way around the gameplan. Refused here, before the
    /// first provider call, and named with the agent so the caller knows which
    /// list to fix.
    fn resolve_agent_tools(&self, permitted: &[String]) -> Result<()> {
        let mut declared = HashMap::new();
        for agent in &self.agents {
            let Some(names) = &agent.tools else { continue };
            let unknown: Vec<&str> = names
                .iter()
                .filter(|name| !permitted.contains(name))
                .map(String::as_str)
                .collect();
            if !unknown.is_empty() {
                let joined = permitted.join(", ");
                let permitted_list = if joined.is_empty() { "(none)" } else { &joined };
                return Err(Error::session(format!(
                    "Agent '{}' declares tool(s) {} which this session does not permit. \
                     An agent narrows the permitted set and cannot add to it. \
                     Permitted: {permitted_list}",
                    agent.name,
                    unknown.join(", "),
                )));
            }
            declared.insert(agent.name.clone(), names.clone());
        }
        *lock(&self.shared.agent_tools) = declared;
        Ok(())
    }

    /// Render the *permitted* context sources for every agent, and cache what
    /// they said.
    ///
    /// One call per source per agent, and the text is what every turn reads.
    /// A source that returns nothing is dropped here rather than in the prompt,
    /// so the cache holds what an agent actually sees.
    fn resolve_context(&self, permitted: &[String]) -> Result<()> {
        let sources: Vec<(String, Arc<dyn ContextSource>)> = self
            .context
            .iter()
            .filter(|(name, _)| permitted.contains(name))
            .map(|(name, source)| (name.clone(), Arc::clone(source)))
            .collect();
        let mut cache: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for agent in &self.agents {
            let mut blocks = Vec::new();
            for (name, source) in &sources {
                let text = source.render(&agent.name)?;
                if text.trim().is_empty() {
                    continue;
                }
                blocks.push((name.clone(), text));
            }
            cache.insert(agent.name.clone(), blocks);
        }
        *lock(&self.shared.context_cache) = cache;
        Ok(())
    }

    /// Settle every agent's options against the session's defaults.
    ///
    /// The one place the session-default/agent-override rule is applied. Run
    /// once at the top of [`Session::run`], before anything reads an agent, so
    /// the rest of the framework never repeats the fallback and cannot disagree
    /// with it about what an unset option means.
    ///
    /// Deliberately *not* done in [`Session::add_agent`]: an agent added before
    /// the caller finished configuring the session would otherwise freeze
    /// defaults that were not written yet. `add_agent` settles only the one
    /// thing that has no session-level default to wait for — the agent's
    /// [`Position`](crate::role::Position).
    fn resolve_agents(&mut self) -> Result<()> {
        for agent in &mut self.agents {
            agent.inherit(&self.defaults)?;
        }
        // The one option that composes rather than overrides, so it is settled
        // against the access manager rather than against `defaults`: an agent
        // workspace has to be *checked* against the session's, not merely
        // preferred over it.
        let mut manager = lock(&self.shared.access);
        for agent in &self.agents {
            let Some(workspace) = &agent.workspace else {
                continue;
            };
            manager.confine_agent(&agent.name, workspace)?;
        }
        Ok(())
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
            let Some(persona) = agent.persona.as_deref().filter(|p| p.ends_with(".md")) else {
                continue;
            };
            // Naming the agent matters — the loader knows the path but not who
            // asked for it, and "which agent?" is the first question a caller
            // has.
            let resolved = resolve_persona_path(persona, &search).map_err(|err| {
                Error::session(format!(
                    "Agent '{}' has persona '{}' which could not be loaded. {err}",
                    agent.name, persona
                ))
            })?;
            agent.persona = Some(absolute(&resolved).display().to_string());
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
        for note in &notes {
            self.shared.remember(agent_name, note)?;
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
        let messages = self
            .shared
            .prompts()
            .messages_for(agent, &[], base_prompt)?;
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

    /// The context ceiling for one agent's turn.
    ///
    /// `max_context_tokens` is what the caller is willing to spend and the
    /// provider's window is what the model can physically hold, so the smaller
    /// of the two binds. Read per agent because a mixed-provider session has one
    /// figure per model, and compacting the whole run against the largest of
    /// them would fail on every turn taken by the smallest.
    fn context_ceiling(&self, agent: &Agent) -> usize {
        self.shared
            .provider_for(agent)
            .and_then(|provider| provider.context_window(agent.model_name()))
            .map_or(self.max_context_tokens, |window| {
                window.min(self.max_context_tokens)
            })
    }

    /// Compact the conversation until it fits *fraction* of what this agent's
    /// turn leaves for it.
    ///
    /// *fraction* is `1.0` for the ordinary pre-turn check. It is lower only
    /// when a provider has already refused the request as too long, where
    /// re-measuring against the same allowance would conclude the conversation
    /// fits and change nothing.
    fn fit_conversation(&mut self, agent: &Agent, base_prompt: &str, fraction: f64) -> Result<()> {
        let ceiling = self.context_ceiling(agent);
        let overhead = self.prompt_overhead(agent, base_prompt)?;
        if overhead >= ceiling {
            // Not a provider's problem to discover. Compaction cannot touch the
            // system prompt, so no amount of summarizing makes this fit;
            // continuing would buy a summary call per turn and still 400.
            return Err(Error::session(format!(
                "The prompt for {} needs about {overhead} tokens before any \
                 conversation, which already meets or exceeds the context \
                 ceiling of {ceiling}. Compaction shrinks the conversation \
                 only, so nothing it can do would make this request fit. Raise \
                 max_context_tokens, or cut the persona, skill index, tool set, \
                 context sources, or memory.",
                agent.name
            )));
        }
        let available = (((ceiling - overhead) as f64) * fraction) as usize;

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
                "Compacted conversation: {overhead} of {ceiling} estimated \
                 tokens are prompt overhead, leaving {available} for the \
                 conversation."
            );
            self.emit_system(&message)?;
            self.save()?;
        }
        Ok(())
    }

    /// Summarize turns being dropped, using the orchestrator's model.
    ///
    /// A plain provider call, not an [`AgentRunner`] turn: a summarizer has no
    /// persona to hold, no skills to load, and nothing to gain from a tool
    /// loop. An empty string on failure is a real answer — `compact` reads it
    /// as "leave the conversation alone", which is the right outcome when the
    /// alternative is trading real turns for nothing.
    fn summarize(&self, turns: &[Turn]) -> Result<String> {
        let Some(agent) = self
            .orchestrator_agent()
            .ok()
            .or_else(|| self.agents.first().cloned())
        else {
            return Ok(String::new());
        };
        let Some(provider) = self.shared.provider_for(&agent) else {
            return Ok(String::new());
        };
        let messages = as_values(&summary_request(turns));
        match crate::usage::observe_provider_call(
            provider.name(),
            agent.model_name(),
            "compaction",
            || {
                provider.chat_with_retries(
                    agent.model_name(),
                    &messages,
                    "compaction",
                    None,
                    agent.effort(),
                )
            },
        ) {
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
// Compatibility adapter for callers using OrchestratorLoop directly.

impl LoopHost for Session {
    fn orchestrator_turn(&mut self, purpose: &str, instruction: Option<&str>) -> Result<String> {
        let agent = self.orchestrator_agent()?;
        let prompt = self.orch_prompt.clone();
        self.turn(&agent, &prompt, purpose, instruction)
    }

    fn participant_turn(&mut self, name: &str, instruction: &str) -> Result<String> {
        let agent = self
            .agents
            .iter()
            .find(|agent| agent.is_participant() && agent.name == name)
            .cloned()
            .ok_or_else(|| Error::session(format!("Unknown participant: {name}")))?;
        let prompt = self.participant_prompt(&agent)?;
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
        // No briefing: the phases are done, and there is nobody left to call.
        self.orchestrator_turn("final summary", None)
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
    let persona = agent.persona.as_deref().unwrap_or_default();
    if !persona.ends_with(".md") {
        return Ok(persona.to_string());
    }
    Ok(load_persona(persona, &[])?.name)
}

/// Refuse a skill that requires a tool nobody registered.
///
/// The check is here, before the first provider call, because the alternative
/// is a skill whose body reads "call `write_file` with…" reaching a model that
/// has no such tool — a run that burns tokens to arrive at an agent apologising
/// for a capability the harness author thought they had shipped. The names are
/// the *registered* ones deliberately: a gameplan narrowing a tool away is not
/// the failure, since [`admit_required`] gives it back.
fn check_required_tools(
    skills: &HashMap<String, Vec<SkillConfig>>,
    registered: &[String],
) -> Result<()> {
    // Keyed by skill and tool rather than by agent: the same skill attached to
    // every agent is one gap reported once, and the ordering is what makes a
    // session with several report the same one every run.
    let mut gaps: BTreeSet<(&str, &str)> = BTreeSet::new();
    for skills in skills.values() {
        for skill in skills {
            for tool in &skill.requires_tools {
                if !registered.iter().any(|name| name == tool) {
                    gaps.insert((skill.name.as_str(), tool.as_str()));
                }
            }
        }
    }
    let Some((skill, tool)) = gaps.first() else {
        return Ok(());
    };
    let available = if registered.is_empty() {
        "(none)".to_string()
    } else {
        registered.join(", ")
    };
    Err(Error::session(format!(
        "Skill {} requires the tool {}, which nobody registered. Register it \
         with add_tool(...), or drop it from the skill's requires-tools. \
         Registered: {available}",
        pyfmt::repr_str(skill),
        pyfmt::repr_str(tool),
    )))
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
    let manager = lock(access).clone();
    // A command with no working directory of its own starts at the session
    // workspace, so a confined session's commands are *in* the confinement
    // rather than merely unable to name their way out of it.
    let cwd = cwd.unwrap_or_else(|| manager.workspace_for(actor));
    let outcome = exec::run_command(&manager, command, Some(cwd), timeout, actor);
    if actor.is_empty() {
        return outcome;
    }
    log_command(channel, command, actor, &outcome)?;
    outcome
}

fn log_command(
    channel: &dyn Channel,
    command: &str,
    actor: &str,
    outcome: &Result<String>,
) -> Result<()> {
    let status = match outcome {
        Ok(_) => "approved",
        Err(Error::AccessDenied(_)) => "denied",
        Err(_) => "failed",
    };
    channel.send_system(&format!("[Command:{status}] {actor}: {command}"))
}

/// Register the built-in tools: `cmd`, `read_file`, `list_dir`, and memory.
///
/// These take the calling agent's name because the access prompt, the command
/// log, and per-agent memory all identify who asked; `add_tool` handlers do
/// not, so the specs are built here rather than through the public method.
///
/// `write_memory` is registered only for a writing session, and `edit_memory`
/// only for one whose store also answers
/// [`budget`](crate::memory::MemoryStore::budget). Advertising a tool whose
/// every call would be discarded is worse than not offering it, and under a
/// store that keeps notes as they were written every revision is refused. The
/// memory tools are ordinary registered tools rather than reserved
/// runtime-owned names, which is what lets a gameplan narrow them away through
/// its `tools:` list — `tools: [cmd, read_memory]` is read-only memory with no
/// extra machinery.
fn default_tools(
    access: &Arc<Mutex<AccessManager>>,
    memories: &Arc<Mutex<Memories>>,
    channel: &Arc<dyn Channel>,
    memory_write: bool,
    memory_filter: Option<&Arc<dyn MemoryFilter>>,
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
            "Read this session's memory and return it as written.",
            json!({"type": "object", "properties": {}}),
            {
                let memories = Arc::clone(memories);
                Arc::new(move |_arguments: &Arguments, actor: &str| {
                    let (store, scope) = store_for(&memories, actor);
                    let content = store.read(&scope)?.trim().to_string();
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
                "Append a note to this session's memory, which every agent \
                 in the session reads. The note is stored as written: include \
                 whoever and whenever matters, because nothing is added around \
                 it. A session may filter what it keeps, so a note is not \
                 guaranteed to be stored.",
                json!({
                    "type": "object",
                    "properties": {
                        "note": {"type": "string", "description": "The note to store, as written."}
                    },
                    "required": ["note"],
                }),
                {
                    let memories = Arc::clone(memories);
                    let filter = memory_filter.cloned();
                    Arc::new(move |arguments: &Arguments, actor: &str| {
                        let stored = remember(
                            &memories,
                            filter.as_ref(),
                            actor,
                            &argument(arguments, "note"),
                        )?;
                        Ok(if stored {
                            "Saved to memory.".to_string()
                        } else {
                            "Not saved: this session did not keep that note.".to_string()
                        })
                    })
                },
            )
            .with_actor(),
        );

        if let Some(budget) = lock(memories).store.budget() {
            tools.push(
                ToolSpec::new(
                    "edit_memory",
                    format!(
                        "Revise this session's memory, which is capped at {budget} \
                         characters. `old_text` is any fragment appearing in exactly \
                         one stored entry and is how that entry is found; giving a \
                         fragment that matches none or several changes nothing and \
                         says so. `new_text` replaces that entry whole — not just \
                         the fragment — and leaving it out removes the entry \
                         instead. Use this when a write is refused for want of \
                         room: merge two overlapping entries into one shorter \
                         entry, or remove one that no longer matters, then write \
                         the note again.",
                    ),
                    json!({
                        "type": "object",
                        "properties": {
                            "old_text": {
                                "type": "string",
                                "description": "A fragment appearing in exactly one stored entry.",
                            },
                            "new_text": {
                                "type": "string",
                                "description": "The entry's replacement. Omit to remove the entry.",
                            },
                        },
                        "required": ["old_text"],
                    }),
                    {
                        let memories = Arc::clone(memories);
                        let filter = memory_filter.cloned();
                        Arc::new(move |arguments: &Arguments, actor: &str| {
                            let new = argument(arguments, "new_text");
                            let revised = revise_memory(
                                &memories,
                                filter.as_ref(),
                                actor,
                                &argument(arguments, "old_text"),
                                &new,
                            )?;
                            Ok(match (revised, new.trim().is_empty()) {
                                (true, true) => "Entry removed from memory.".to_string(),
                                (true, false) => "Entry replaced in memory.".to_string(),
                                (false, _) => {
                                    "Not changed: this session did not keep that revision."
                                        .to_string()
                                }
                            })
                        })
                    },
                )
                .with_actor(),
            );
        }
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

    use crate::memory::CuratedMemory;
    use crate::provider::{ProviderBase, ProviderResponse, ReasoningEffort};
    use crate::testing::TempDir;

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
            _effort: ReasoningEffort,
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

    /// A policy whose workspace is *temp*, where a test keeps its files.
    ///
    /// Not optional decoration: an unset workspace is the process's current
    /// directory, and a scratch directory under `/tmp` is not inside it. Every
    /// test that writes a memory or session file needs this for the same reason
    /// a real caller working outside its launch directory does.
    fn confined(temp: &TempDir) -> Option<AccessPolicy> {
        let mut policy = AccessPolicy::new();
        policy.workspace = Some(temp.text());
        Some(policy)
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
            memory: temp.child("memory.md"),
            access_policy: confined(temp),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("the debate gameplan loads");
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");
        session
            .add_agent(Agent::new("Bob").with_model("m"))
            .expect("add agent");
        session
            .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
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
            debate(
                &temp,
                provider,
                channel,
                "Is pineapple acceptable on pizza?",
            )
            .run()
            .expect("a run")
        };

        assert!(!ended.consensus_reached);
        assert!(ended.turns_completed >= 4);
        assert_eq!(ended.topic, "Is pineapple acceptable on pizza?");
        assert!(!ended.final_summary.is_empty());
        assert!(ended
            .history
            .iter()
            .any(|message| message.msg_type == "turn"));
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
            debate(
                &temp,
                provider,
                channel,
                "Is pineapple acceptable on pizza?",
            )
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
        let mut session = debate(
            &temp,
            provider,
            Arc::clone(&channel) as Arc<dyn Channel>,
            "T",
        );
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
            .add_agent(Agent::new("Mod1").with_model("m").with_role("orchestrator"))
            .expect("the first is accepted");

        let error = session
            .add_agent(Agent::new("Mod2").with_model("m").with_role("orchestrator"))
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
            memory: temp.child("memory.md"),
            access_policy: confined(&temp),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };

        // No provider anywhere, and the error names who is missing one.
        let mut session = Session::new(SessionConfig {
            provider: None,
            ..base()
        })
        .expect("loads");
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");
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
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");
        assert!(error_text(&session.run().expect_err("no topic")).contains("No topic set."));

        // No participants.
        let mut session = Session::new(SessionConfig {
            provider: Some(Arc::new(SequenceProvider::new(&["x"]))),
            ..base()
        })
        .expect("loads");
        session
            .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
            .expect("accepted");
        assert!(error_text(&session.run().expect_err("no participants"))
            .contains("No participant agents added."));

        // No orchestrator, which no harness can waive.
        let mut session = Session::new(SessionConfig {
            provider: Some(Arc::new(SequenceProvider::new(&["x"]))),
            ..base()
        })
        .expect("loads");
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");
        session
            .add_agent(Agent::new("Bob").with_model("m"))
            .expect("add agent");
        let error = error_text(&session.run().expect_err("no orchestrator"));
        assert!(error.contains("No orchestrator agent added."));
        assert!(error.contains("role is 'orchestrator'"));
    }

    #[test]
    fn add_tool_refuses_a_name_it_could_not_honour() {
        let temp = TempDir::new("add-tool");
        let mut session = Session::new(SessionConfig {
            topic: "T".to_string(),
            memory: temp.child("memory.md"),
            access_policy: confined(&temp),
            ..SessionConfig::default()
        })
        .expect("loads");
        let handler: Arc<dyn ToolHandler> = Arc::new(|_: &Arguments, _: &str| Ok("ok".to_string()));
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
            .add_tool("cmd", "d", schema.clone(), Arc::clone(&handler))
            .map(|_| ())
            .expect_err("a built-in name is refused");
        assert!(error_text(&error).contains("already registered"));
        for (name, expected) in [
            (SKILL_TOOL_NAME, "reserved tool name"),
            ("mine", "already registered"),
            ("cmd", "already registered"),
        ] {
            let error = session
                .add_tool_spec(
                    ToolSpec::new(name, "d", schema.clone(), Arc::clone(&handler)).with_actor(),
                )
                .map(|_| ())
                .expect_err("complete specs obey the same collision rules");
            assert!(error_text(&error).contains(expected), "{name}: {error}");
        }
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
        // Phases own the flow, so the orchestrator is not told it does. The
        // generic licence contradicts the phase block above it, and an
        // orchestrator resolving that contradiction stops calling the roster.
        assert!(!prompt.contains("You control the flow"));
        assert!(!prompt.contains("at any point"));
        assert!(prompt.contains("the phases advance on their own"));
        assert!(prompt.contains("Never write a participant's turn for them"));
    }

    #[test]
    fn a_phase_less_gameplan_keeps_the_orchestrator_in_charge_of_the_flow() {
        // The counterpart: with no phases there is no loop-owned rotation to
        // contradict, and withholding the licence would leave nobody driving.
        let temp = TempDir::new("prompt-flow");
        let provider = Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]));
        let channel = Arc::new(CaptureChannel::default());
        let path = temp.child("flat.md");
        std::fs::write(
            &path,
            "---\nname: flat\nagents:\n  orchestrator:\n    required: true\n\
             loop:\n  max_rounds: 2\n---\n\nBody.\n",
        )
        .expect("write the gameplan");
        let config = SessionConfig {
            gameplan: path,
            topic: "Anything".to_string(),
            provider: Some(Arc::clone(&provider) as Arc<dyn Provider>),
            channel: Some(channel),
            memory: temp.child("memory.md"),
            access_policy: confined(&temp),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("a session");
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");
        session
            .add_agent(Agent::new("Bob").with_model("m"))
            .expect("add agent");
        session
            .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
            .expect("accepted");
        session.run().expect("a run");

        let prompt = provider.system_prompts().remove(0);
        assert!(prompt.contains("You control the flow"));
        assert!(prompt.contains("The session ends after 2 rounds"));
        assert!(!prompt.contains("Never write a participant's turn for them"));
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
            memory: temp.child("memory.md"),
            access_policy: confined(&temp),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("loads");
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");
        session
            .add_agent(Agent::new("Bob").with_model("m"))
            .expect("add agent");
        let mut orchestrator = Agent::new("Mod").with_model("m").with_role("orchestrator");
        orchestrator.system_prompt = Some("Judge this: {topic}".to_string());
        session.add_agent(orchestrator).expect("accepted");

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
            memory: temp.child("memory.md"),
            access_policy: confined(&temp),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("loads");
        let mut alice = Agent::new("Alice").with_model("m");
        alice.persona = Some("pragmatic_engineer.md".to_string());
        let mut bob = Agent::new("Bob").with_model("m");
        bob.persona = Some("A cautious lawyer".to_string());
        session.add_agent(alice).expect("add agent");
        session.add_agent(bob).expect("add agent");
        session
            .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
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
            memory: temp.child("memory.md"),
            access_policy: confined(&temp),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("loads");
        let mut alice = Agent::new("Alice").with_model("m");
        alice.persona = Some("nope_not_here.md".to_string());
        session.add_agent(alice).expect("add agent");
        session
            .add_agent(Agent::new("Bob").with_model("m"))
            .expect("add agent");
        session
            .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
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

        let provider = Arc::new(
            SequenceProvider::new(&["END_SESSION", "Done."]).with_dialect(ToolDialect::Openai),
        );
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
            (result, session.memory().expect("memory reads"))
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

        let memory_path = temp.child("written.md");
        let writing = {
            let provider = Arc::new(SequenceProvider::new(&replies));
            let config = SessionConfig {
                topic: "T".to_string(),
                provider: Some(provider),
                channel: Some(Arc::new(CaptureChannel::default())),
                memory: memory_path.clone(),
                access_policy: confined(&temp),
                memory_write: true,
                turn_delay: Duration::ZERO,
                ..SessionConfig::default()
            };
            let mut session = Session::new(config).expect("loads");
            session
                .add_agent(Agent::new("Alice").with_model("m"))
                .expect("add agent");
            session
                .add_agent(Agent::new("Bob").with_model("m"))
                .expect("add agent");
            session
                .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
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
            memory: temp.child("memory.md"),
            access_policy: confined(&temp),
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
            memory: temp.child("memory.md"),
            access_policy: confined(&temp),
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
        let path = temp.child("memory.md");
        let mut session = Session::new(SessionConfig {
            topic: "T".to_string(),
            memory: path.clone(),
            access_policy: confined(&temp),
            memory_write: true,
            ..SessionConfig::default()
        })
        .expect("loads");
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");

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

    /// The filter has to cover both ways model output reaches the file, or a
    /// note the tool refuses lands anyway by being written as a marker.
    #[test]
    fn a_filter_rewrites_or_drops_what_an_agent_writes() {
        struct NoSecrets;
        impl MemoryFilter for NoSecrets {
            fn filter(&self, note: &str, actor: &str) -> Option<String> {
                if note.contains("sk-") {
                    return None;
                }
                Some(format!("{actor}: {note}"))
            }
        }

        let temp = TempDir::new("memory-filter");
        let path = temp.child("memory.md");
        let mut session = Session::new(SessionConfig {
            topic: "T".to_string(),
            memory: path.clone(),
            access_policy: confined(&temp),
            memory_write: true,
            memory_filter: Some(Arc::new(NoSecrets)),
            ..SessionConfig::default()
        })
        .expect("loads");
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");

        let tools = session.shared.active_tools();
        let write = tools
            .iter()
            .find(|tool| tool.name == "write_memory")
            .expect("write_memory is registered");
        let note = |text: &str| {
            let mut arguments = Map::new();
            arguments.insert("note".to_string(), Value::String(text.to_string()));
            write.handler.call(&arguments, "Alice").expect("a write")
        };

        assert_eq!(note("Ship it."), "Saved to memory.");
        assert_eq!(
            note("The key is sk-4242."),
            "Not saved: this session did not keep that note."
        );
        // The marker pass is the other door into the same file.
        session
            .process_memory_markers("Agreed.\n@MEMORY: Also sk-9999.", "Alice")
            .expect("markers");
        session
            .process_memory_markers("Agreed.\n@MEMORY: Bob dissented.", "Alice")
            .expect("markers");

        let written = std::fs::read_to_string(&path).expect("the file exists");
        assert!(written.contains("Alice: Ship it."), "{written}");
        assert!(written.contains("Alice: Bob dissented."), "{written}");
        assert!(!written.contains("sk-"), "{written}");
    }

    #[test]
    fn edit_memory_is_offered_only_where_the_store_sets_a_ceiling() {
        // Under a store that keeps notes as they were written every revision is
        // refused, so the tool would be one whose every call fails.
        let temp = TempDir::new("edit-memory-tools");
        let names = |store: Option<Arc<dyn MemoryStore>>| {
            Session::new(SessionConfig {
                topic: "T".to_string(),
                memory: temp.child("memory.md"),
                access_policy: confined(&temp),
                memory_write: true,
                memory_store: store,
                ..SessionConfig::default()
            })
            .expect("loads")
            .shared
            .active_tools()
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<String>>()
        };

        assert!(!names(None).contains(&"edit_memory".to_string()));

        let curated: Arc<dyn MemoryStore> = Arc::new(CuratedMemory::new(&temp.0));
        let offered = names(Some(curated));
        assert!(offered.contains(&"edit_memory".to_string()));
        let description = Session::new(SessionConfig {
            topic: "T".to_string(),
            memory: temp.child("memory.md"),
            access_policy: confined(&temp),
            memory_write: true,
            memory_store: Some(Arc::new(CuratedMemory::new(&temp.0).with_budget(600))),
            ..SessionConfig::default()
        })
        .expect("loads")
        .shared
        .active_tools()
        .iter()
        .find(|tool| tool.name == "edit_memory")
        .expect("registered")
        .description
        .clone();
        assert!(
            description.contains("capped at 600 characters"),
            "the ceiling the agent is curating towards is in the tool: {description}"
        );
    }

    #[test]
    fn edit_memory_revises_through_the_filter_and_removes_without_it() {
        struct NoSecrets;
        impl MemoryFilter for NoSecrets {
            fn filter(&self, note: &str, _actor: &str) -> Option<String> {
                (!note.contains("sk-")).then(|| note.to_string())
            }
        }

        let temp = TempDir::new("edit-memory");
        let mut session = Session::new(SessionConfig {
            topic: "T".to_string(),
            memory: "shared".to_string(),
            access_policy: confined(&temp),
            memory_write: true,
            memory_filter: Some(Arc::new(NoSecrets)),
            memory_store: Some(Arc::new(CuratedMemory::new(&temp.0))),
            ..SessionConfig::default()
        })
        .expect("loads");
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");

        let tools = session.shared.active_tools();
        let call = |name: &str, arguments: Map<String, Value>| {
            tools
                .iter()
                .find(|tool| tool.name == name)
                .expect("registered")
                .handler
                .call(&arguments, "Alice")
        };
        let one = |key: &str, value: &str| {
            let mut arguments = Map::new();
            arguments.insert(key.to_string(), Value::String(value.to_string()));
            arguments
        };
        let edit = |old: &str, new: &str| {
            let mut arguments = one("old_text", old);
            arguments.insert("new_text".to_string(), Value::String(new.to_string()));
            arguments
        };

        call("write_memory", one("note", "Alice chose blue")).expect("a write");
        call("write_memory", one("note", "Bob chose green")).expect("a write");

        assert_eq!(
            call("edit_memory", edit("Alice", "Alice and Bob chose blue")).expect("a revision"),
            "Entry replaced in memory."
        );
        // The revision is model output landing in every agent's system prompt,
        // so it goes through the same filter an appended note does.
        assert_eq!(
            call("edit_memory", edit("green", "The key is sk-4242.")).expect("a refusal"),
            "Not changed: this session did not keep that revision."
        );
        // "Bob" now appears in both entries, so it addresses neither of them.
        let ambiguous =
            call("edit_memory", edit("Bob", "one entry")).expect_err("two entries contain it");
        assert!(
            ambiguous.to_string().contains("2 entries contain"),
            "{ambiguous}"
        );
        // A removal writes no text, so there is nothing for a filter to vet.
        assert_eq!(
            call("edit_memory", one("old_text", "green")).expect("a removal"),
            "Entry removed from memory."
        );
        // A fragment matching nothing is an error the agent reads and retries on.
        let missed = call("edit_memory", edit("Carol", "who?")).expect_err("no such entry");
        assert!(missed.to_string().contains("No entry"), "{missed}");

        let stored = call("read_memory", Map::new()).expect("a read");
        assert!(stored.contains("1 entries"), "{stored}");
        assert!(stored.contains("Alice and Bob chose blue"), "{stored}");
        assert!(!stored.contains("sk-"), "{stored}");
    }

    // ---- the memory store --------------------------------------------------

    /// A store that keeps nothing and remembers everything it was asked to do.
    #[derive(Default)]
    struct RecordingStore {
        calls: Mutex<Vec<String>>,
        notes: Mutex<Vec<(String, String)>>,
        path_probe: Mutex<Option<std::sync::Weak<Mutex<Memories>>>>,
    }

    impl RecordingStore {
        fn calls(&self) -> Vec<String> {
            lock(&self.calls).clone()
        }

        fn notes(&self) -> Vec<(String, String)> {
            lock(&self.notes).clone()
        }
    }

    impl MemoryStore for RecordingStore {
        fn path(&self, _: &str) -> Option<PathBuf> {
            if let Some(memories) = lock(&self.path_probe)
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
            {
                let _guard = memories
                    .try_lock()
                    .expect("store callbacks run outside the memories lock");
            }
            None
        }

        fn read(&self, scope: &str) -> Result<String> {
            lock(&self.calls).push(format!("read {scope}"));
            Ok(lock(&self.notes)
                .iter()
                .filter(|(seen, _)| seen == scope)
                .map(|(_, note)| note.clone())
                .collect::<Vec<_>>()
                .join("\n"))
        }

        fn append(&self, scope: &str, note: &str) -> Result<()> {
            lock(&self.calls).push(format!("append {scope}"));
            lock(&self.notes).push((scope.to_string(), note.to_string()));
            Ok(())
        }

        fn open(&self, scope: &str) -> Result<()> {
            lock(&self.calls).push(format!("open {scope}"));
            Ok(())
        }

        fn close(&self) -> Result<()> {
            lock(&self.calls).push("close".to_string());
            Ok(())
        }
    }

    #[test]
    fn an_installed_store_is_opened_read_written_and_closed() {
        // The whole slot in one run: every scope opened before the first turn,
        // notes routed to it instead of to a file, and closed once at the end.
        let temp = TempDir::new("store-lifecycle");
        let store = Arc::new(RecordingStore::default());
        let provider = Arc::new(SequenceProvider::new(&[
            "@Alice, go.",
            "My view.\n@MEMORY: Alice prefers Rust.",
            "END_SESSION",
            "Done.",
        ]));
        let config = SessionConfig {
            topic: "T".to_string(),
            provider: Some(provider),
            channel: Some(Arc::new(CaptureChannel::default())),
            memory: "session".to_string(),
            memory_store: Some(Arc::clone(&store) as Arc<dyn MemoryStore>),
            memory_write: true,
            access_policy: confined(&temp),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config.clone()).expect("loads");
        let mut alice = Agent::new("Alice").with_model("m");
        alice.memory = Some("alice".to_string());
        session.add_agent(alice).expect("add agent");
        session
            .add_agent(Agent::new("Bob").with_model("m"))
            .expect("add agent");
        session
            .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
            .expect("accepted");
        session.run().expect("a run");

        let calls = store.calls();
        assert_eq!(calls.first().map(String::as_str), Some("open session"));
        assert!(calls.contains(&"open alice".to_string()), "{calls:?}");
        assert_eq!(calls.last().map(String::as_str), Some("close"));
        // Every open precedes every read: a scope that cannot be opened fails
        // the run before a provider is paid for a turn against it.
        let first_read = calls
            .iter()
            .position(|call| call.starts_with("read "))
            .expect("the prompt read memory");
        assert!(calls[..first_read]
            .iter()
            .all(|call| call.starts_with("open")));

        // Alice's marker went to her own scope; the result block to the
        // session's. Neither reached the filesystem.
        let notes = store.notes();
        assert!(
            notes
                .iter()
                .any(|(scope, note)| scope == "alice" && note == "Alice prefers Rust."),
            "{notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|(scope, note)| scope == "session" && note.contains("## Session Result")),
            "{notes:?}"
        );
        assert!(!temp.0.join("session").exists());

        for mode in ["complete", "cancel", "drop", "failure"] {
            let store = Arc::new(RecordingStore::default());
            let mut session = Session::new(SessionConfig {
                memory_store: Some(store.clone()),
                ..config.clone()
            })
            .unwrap();
            let memories = session.memories();
            *lock(&store.path_probe) = Some(Arc::downgrade(&memories));
            session
                .add_agent(Agent {
                    memory: Some("alice".into()),
                    ..Agent::new("Alice").with_model("m")
                })
                .unwrap();
            session
                .add_agent(Agent::new("Bob").with_model("m"))
                .unwrap();
            session
                .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
                .unwrap();
            let mut options = RunOptions {
                mode: RunMode::HostDriven,
                ..Default::default()
            };
            if mode == "failure" {
                options.event_sink = Some(Arc::new(|_: &RunEvent| {
                    Err(Error::Value("sink failure".into()))
                }));
            }
            let mut run = session.start(options).unwrap();
            if mode == "cancel" {
                run.control().cancel();
            }
            if mode != "drop" {
                let input = if mode == "complete" {
                    RunInput::Finish {
                        result: json!({"consensus": false, "summary": "Host result"}),
                    }
                } else {
                    RunInput::Continue
                };
                let StepOutcome::Finished { outcome } = run.step(input).unwrap() else {
                    panic!("expected terminal {mode}")
                };
                assert_eq!(
                    outcome.reason,
                    match mode {
                        "complete" => RunReason::Completed,
                        "cancel" => RunReason::Cancelled,
                        _ => RunReason::Failed,
                    }
                );
                assert!(matches!(
                    run.step(RunInput::Continue).unwrap(),
                    StepOutcome::Finished { .. }
                ));
            }
            drop(run);
            let calls = store.calls();
            assert!(calls.contains(&"open alice".into()), "{mode}: {calls:?}");
            assert_eq!(
                calls.iter().filter(|call| call.as_str() == "close").count(),
                1,
                "{mode}: {calls:?}"
            );
        }
    }

    #[test]
    fn the_filter_runs_before_an_installed_store_sees_a_note() {
        // A third-party store must not be a way around the caller's filter, so
        // the filter runs on the way in and the store never sees a refusal.
        struct NoSecrets;
        impl MemoryFilter for NoSecrets {
            fn filter(&self, note: &str, actor: &str) -> Option<String> {
                if note.contains("sk-") {
                    return None;
                }
                Some(format!("{actor}: {note}"))
            }
        }

        let temp = TempDir::new("store-filter");
        let store = Arc::new(RecordingStore::default());
        let mut session = Session::new(SessionConfig {
            topic: "T".to_string(),
            memory: "session".to_string(),
            memory_store: Some(Arc::clone(&store) as Arc<dyn MemoryStore>),
            memory_write: true,
            memory_filter: Some(Arc::new(NoSecrets)),
            access_policy: confined(&temp),
            ..SessionConfig::default()
        })
        .expect("loads");
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");

        let tools = session.shared.active_tools();
        let write = tools
            .iter()
            .find(|tool| tool.name == "write_memory")
            .expect("write_memory is registered");
        let note = |text: &str| {
            let mut arguments = Map::new();
            arguments.insert("note".to_string(), Value::String(text.to_string()));
            write.handler.call(&arguments, "Alice").expect("a write")
        };

        assert_eq!(note("Ship it."), "Saved to memory.");
        assert_eq!(
            note("The key is sk-4242."),
            "Not saved: this session did not keep that note."
        );

        // Rewritten on the way in, and the refusal never reached the store at
        // all: one append for two calls to the tool.
        let notes = store.notes();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(
            notes[0],
            ("session".to_string(), "Alice: Ship it.".to_string())
        );
        assert_eq!(
            store
                .calls()
                .iter()
                .filter(|call| call.starts_with("append"))
                .count(),
            1
        );
    }

    #[test]
    fn a_store_that_names_no_file_is_checked_against_no_workspace() {
        // The workspace confines files, and only the store knows whether a
        // scope is one. The default says it is and the path is rejected; a
        // store that keeps nothing on disk answers `None` and is left alone.
        let temp = TempDir::new("store-paths");
        let outside = "/nowhere/kerness-memory.md".to_string();

        let denied = Session::new(SessionConfig {
            topic: "T".to_string(),
            memory: outside.clone(),
            access_policy: confined(&temp),
            ..SessionConfig::default()
        });
        assert!(
            error_text(&denied.err().expect("the file is outside the workspace"))
                .contains("memory file")
        );

        Session::new(SessionConfig {
            topic: "T".to_string(),
            memory: outside,
            memory_store: Some(Arc::new(RecordingStore::default())),
            access_policy: confined(&temp),
            ..SessionConfig::default()
        })
        .expect("a store keeping no file has no path to confine");
    }

    #[test]
    fn a_store_that_cannot_open_stops_the_run_before_the_first_turn() {
        struct Sealed;
        impl MemoryStore for Sealed {
            fn read(&self, _scope: &str) -> Result<String> {
                Ok(String::new())
            }
            fn append(&self, _scope: &str, _note: &str) -> Result<()> {
                Ok(())
            }
            fn open(&self, scope: &str) -> Result<()> {
                Err(Error::Io(format!("{scope} is sealed")))
            }
        }

        let temp = TempDir::new("store-sealed");
        let provider = Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]));
        let mut session = Session::new(SessionConfig {
            topic: "T".to_string(),
            provider: Some(Arc::clone(&provider) as Arc<dyn Provider>),
            channel: Some(Arc::new(CaptureChannel::default())),
            memory: "session".to_string(),
            memory_store: Some(Arc::new(Sealed)),
            access_policy: confined(&temp),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        })
        .expect("loads");
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");
        session
            .add_agent(Agent::new("Bob").with_model("m"))
            .expect("add agent");
        session
            .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
            .expect("accepted");

        let error = session
            .run()
            .expect_err("an unopenable scope fails the run");
        assert!(error_text(&error).contains("sealed"), "{error}");
        assert!(provider.calls().is_empty());
    }

    // ---- harness narrowing ------------------------------------------------

    #[test]
    fn the_tools_key_decides_what_the_prompt_advertises() {
        let temp = TempDir::new("narrowing");
        let gameplan = temp.child("narrow.md");
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
            memory: temp.child("memory.md"),
            access_policy: confined(&temp),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("loads");
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");
        session
            .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
            .expect("accepted");
        session.run().expect("a run");

        let prompt = provider.system_prompts().remove(0);
        assert!(prompt.contains("read_memory"));
        assert!(!prompt.contains("read_file"));
    }

    #[test]
    fn a_gameplan_naming_an_unregistered_tool_fails_the_session() {
        let temp = TempDir::new("unregistered");
        let gameplan = temp.child("ghost.md");
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
            memory: temp.child("memory.md"),
            access_policy: confined(&temp),
            turn_delay: Duration::ZERO,
            ..SessionConfig::default()
        };
        let mut session = Session::new(config).expect("loads");
        session
            .add_agent(Agent::new("Alice").with_model("m"))
            .expect("add agent");
        session
            .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
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
        let path = temp.child("run.json");

        let run = |replies: &[&str]| -> SessionResult {
            let provider = Arc::new(SequenceProvider::new(replies));
            let config = SessionConfig {
                topic: "T".to_string(),
                provider: Some(provider),
                channel: Some(Arc::new(CaptureChannel::default())),
                memory: temp.child("memory.md"),
                access_policy: confined(&temp),
                session_file: Some(path.clone()),
                turn_delay: Duration::ZERO,
                ..SessionConfig::default()
            };
            let mut session = Session::new(config).expect("loads");
            session
                .add_agent(Agent::new("Alice").with_model("m"))
                .expect("add agent");
            session
                .add_agent(Agent::new("Bob").with_model("m"))
                .expect("add agent");
            session
                .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
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
        let path = temp.child("run.json");
        let run = |topic: &str| -> Result<SessionResult> {
            let provider = Arc::new(SequenceProvider::new(&["END_SESSION", "Done."]));
            let config = SessionConfig {
                topic: topic.to_string(),
                provider: Some(provider),
                channel: Some(Arc::new(CaptureChannel::default())),
                memory: temp.child("memory.md"),
                access_policy: confined(&temp),
                session_file: Some(path.clone()),
                turn_delay: Duration::ZERO,
                ..SessionConfig::default()
            };
            let mut session = Session::new(config).expect("loads");
            session
                .add_agent(Agent::new("Alice").with_model("m"))
                .expect("add agent");
            session
                .add_agent(Agent::new("Bob").with_model("m"))
                .expect("add agent");
            session
                .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
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
        assert!(error.contains("context ceiling of 10"), "{error}");
        assert!(provider.calls().is_empty());
    }

    /// A window the provider declares is what the model can hold; the session's
    /// figure is what the caller will pay for. Neither outranks the other, so
    /// the smaller has to bind both ways round.
    #[test]
    fn the_smaller_of_the_session_and_the_provider_window_is_the_ceiling() {
        struct Windowed(usize);
        impl Provider for Windowed {
            fn name(&self) -> &str {
                "Windowed"
            }
            fn base(&self) -> &ProviderBase {
                static BASE: std::sync::OnceLock<ProviderBase> = std::sync::OnceLock::new();
                BASE.get_or_init(ProviderBase::default)
            }
            fn context_window(&self, _model: &str) -> Option<usize> {
                Some(self.0)
            }
            fn chat(
                &self,
                _: &str,
                _: &[Value],
                _: Option<&[ToolSpec]>,
                _: ReasoningEffort,
            ) -> Result<ProviderResponse> {
                Ok(ProviderResponse::text("hi"))
            }
        }

        let temp = TempDir::new("ceiling");
        let agent = Agent::new("Alice").with_model("m");
        let ceiling = |session_limit: usize, window: usize| {
            let mut session = debate(
                &temp,
                Arc::new(Windowed(window)) as Arc<dyn Provider>,
                Arc::new(CaptureChannel::default()),
                "T",
            );
            session.max_context_tokens = session_limit;
            session.context_ceiling(&agent)
        };

        assert_eq!(ceiling(200_000, 8_000), 8_000);
        assert_eq!(ceiling(4_000, 8_000), 4_000);
    }

    /// The pre-turn check counts characters, so the provider is the authority
    /// and it disagrees sometimes. Writing the turn off would lose it over a
    /// heuristic the framework already documents as approximate.
    #[test]
    fn a_request_the_provider_calls_too_long_is_compacted_and_sent_again() {
        struct Overflowing {
            base: ProviderBase,
            calls: AtomicUsize,
        }
        impl Provider for Overflowing {
            fn name(&self) -> &str {
                "Overflowing"
            }
            fn base(&self) -> &ProviderBase {
                &self.base
            }
            fn chat(
                &self,
                _: &str,
                _: &[Value],
                _: Option<&[ToolSpec]>,
                _: ReasoningEffort,
            ) -> Result<ProviderResponse> {
                if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    return Err(Error::ProviderHttp {
                        status_code: 400,
                        url: "https://api.example.com/v1".to_string(),
                        body: "maximum context length is 8000 tokens".to_string(),
                    });
                }
                Ok(ProviderResponse::text("END_SESSION"))
            }
        }

        let temp = TempDir::new("overflow-retry");
        let provider = Arc::new(Overflowing {
            base: ProviderBase::new(0, 0.0, None),
            calls: AtomicUsize::new(0),
        });
        let mut session = debate(
            &temp,
            Arc::clone(&provider) as Arc<dyn Provider>,
            Arc::new(CaptureChannel::default()),
            "T",
        );

        let text = session
            .turn(
                &Agent::new("Mod").with_model("m").with_role("orchestrator"),
                "BASE",
                "orchestrator turn",
                None,
            )
            .expect("the retry salvages the turn");

        assert_eq!(text, "END_SESSION");
        assert_eq!(provider.calls.load(Ordering::Relaxed), 2);
    }

    /// Once, not in a loop. A second refusal means the length is not in the
    /// conversation, so going round again buys a summary call per attempt and
    /// is refused every time.
    #[test]
    fn a_second_refusal_reaches_the_caller() {
        struct AlwaysOverflowing {
            base: ProviderBase,
            calls: AtomicUsize,
        }
        impl Provider for AlwaysOverflowing {
            fn name(&self) -> &str {
                "AlwaysOverflowing"
            }
            fn base(&self) -> &ProviderBase {
                &self.base
            }
            fn chat(
                &self,
                _: &str,
                _: &[Value],
                _: Option<&[ToolSpec]>,
                _: ReasoningEffort,
            ) -> Result<ProviderResponse> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                Err(Error::ProviderHttp {
                    status_code: 400,
                    url: "https://api.example.com/v1".to_string(),
                    body: "prompt is too long".to_string(),
                })
            }
        }

        let temp = TempDir::new("overflow-twice");
        let provider = Arc::new(AlwaysOverflowing {
            base: ProviderBase::new(0, 0.0, None),
            calls: AtomicUsize::new(0),
        });
        let mut session = debate(
            &temp,
            Arc::clone(&provider) as Arc<dyn Provider>,
            Arc::new(CaptureChannel::default()),
            "T",
        );

        let error = session
            .turn(
                &Agent::new("Mod").with_model("m").with_role("orchestrator"),
                "BASE",
                "orchestrator turn",
                None,
            )
            .expect_err("the second refusal is the caller's");
        assert!(error.is_context_overflow(), "{error}");
        assert_eq!(provider.calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn a_provider_that_names_no_window_leaves_the_session_figure_alone() {
        let temp = TempDir::new("no-window");
        let mut session = debate(
            &temp,
            Arc::new(SequenceProvider::new(&["hi"])) as Arc<dyn Provider>,
            Arc::new(CaptureChannel::default()),
            "T",
        );
        session.max_context_tokens = 12_345;

        assert_eq!(
            session.context_ceiling(&Agent::new("Alice").with_model("m")),
            12_345
        );
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
        debate(
            &temp,
            provider,
            Arc::clone(&channel) as Arc<dyn Channel>,
            "T",
        )
        .run()
        .expect("a run");

        assert!(!channel.contains("Compacted conversation:"));
    }
}
