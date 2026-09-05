//! Owned synchronous session execution, control, and durable suspension.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::outcome;
use super::{
    as_values, lock, ApprovalMode::External, PreflightAction, ResultDiagnostics, ResultValidation,
    Session, SessionResult, ToolContext, ToolIdentity,
};
use crate::access::AccessRequest;
use crate::agent_runtime::{AgentRunner, AgentTurn};
use crate::error::{Error, Result};
use crate::jsonschema::validate_arguments;
use crate::orchestrator::{LoopAction, LoopTurnKind, OrchestratorLoop};
use crate::sessionfile::{save_snapshot, SessionSnapshot};
use crate::tooling::{ToolCall, INVALID_CALL};
use crate::toolkit::ToolResult;
use crate::usage::{BudgetExceeded, RunBudget, TokenPricing, UsageCollector, UsageLedger};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    #[default]
    Automatic,
    HostDriven,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    Callback,
    #[default]
    External,
}

/// A cancellation flag independent of a borrow of the run. Cancellation is
/// cooperative: arbitrary synchronous providers and handlers may block.
#[derive(Clone, Default)]
pub struct RunControl(Arc<AtomicBool>);
impl RunControl {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// An observer. Decisions enter through `RunInput` or `RunControl`. Delivery
/// is at most once; an error terminates the run without replaying its action.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: &RunEvent) -> Result<()>;
}
impl<F> EventSink for F
where
    F: Fn(&RunEvent) -> Result<()> + Send + Sync,
{
    fn emit(&self, event: &RunEvent) -> Result<()> {
        self(event)
    }
}

#[derive(Clone, Default)]
pub struct RunOptions {
    pub mode: RunMode,
    pub approvals: ApprovalMode,
    pub budget: RunBudget,
    pub pricing: Vec<TokenPricing>,
    pub event_sink: Option<Arc<dyn EventSink>>,
    pub result_validation: ResultValidation,
    /// Host version for providers/handlers whose implementation cannot be
    /// serialized. Change it when their contract changes between resumes.
    pub binding_version: String,
}
impl RunOptions {
    pub(super) fn legacy() -> Self {
        Self {
            approvals: ApprovalMode::Callback,
            result_validation: ResultValidation::LegacyCoercion,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    pub request_id: String,
    pub identity: ToolIdentity,
    pub call: ToolCall,
    pub action: PreflightAction,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunInput {
    #[default]
    Continue,
    SelectAgent {
        agent: String,
        instruction: String,
    },
    UserMessage {
        text: String,
    },
    Approve {
        request_id: String,
        approved: bool,
    },
    /// Supply a known result for an action interrupted between durable intent
    /// and completion. Cancellation is the alternative to reconciliation.
    Reconcile {
        action_id: String,
        result: ToolResult,
    },
    Finish {
        result: Value,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitReason {
    Input,
    Approval {
        request: ApprovalRequest,
    },
    Indeterminate {
        action_id: String,
        identity: ToolIdentity,
        call: ToolCall,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunReason {
    Completed,
    Cancelled,
    BudgetExceeded { budget: BudgetExceeded },
    InvalidResult,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOutcome {
    pub reason: RunReason,
    pub result: SessionResult,
    pub diagnostics: ResultDiagnostics,
    pub usage: UsageLedger,
    pub error: Option<Error>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
// These owned values let callers move a wait or result directly out of the
// match; the synchronous boundary does not allocate an enum for every step.
#[allow(clippy::large_enum_variant)]
pub enum StepOutcome {
    Progress,
    Waiting { reason: WaitReason },
    Finished { outcome: RunOutcome },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunEventKind {
    Started,
    ProviderStarted {
        actor: String,
        purpose: String,
    },
    ProviderFinished {
        actor: String,
        purpose: String,
    },
    ToolStarted {
        identity: ToolIdentity,
        call: ToolCall,
    },
    ToolFinished {
        identity: ToolIdentity,
        result: ToolResult,
    },
    ApprovalRequested {
        request: ApprovalRequest,
    },
    TurnCommitted {
        actor: String,
        text: String,
    },
    Terminal {
        reason: RunReason,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvent {
    pub sequence: u64,
    pub run_id: String,
    pub turn_id: u64,
    pub event: RunEventKind,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActiveTurn {
    agent: String,
    purpose: String,
    instruction: Option<String>,
    base_prompt: String,
    state: Option<AgentTurn>,
    needs_fit: bool,
    overflow_retry: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionIntent {
    identity: ToolIdentity,
    call: ToolCall,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosingState {
    outcome: RunOutcome,
    scopes: Vec<String>,
    next_scope: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSnapshot {
    run_id: String,
    turn_id: u64,
    next_call: u64,
    next_event: u64,
    started: bool,
    contract: Value,
    active: Option<ActiveTurn>,
    approval: Option<ApprovalRequest>,
    approved: bool,
    intent: Option<ActionIntent>,
    loaded_skills: Vec<String>,
    context: std::collections::HashMap<String, Vec<(String, String)>>,
    usage: UsageLedger,
    terminal: Option<RunOutcome>,
    closing: Option<ClosingState>,
}

/// An owned run. A step dispatches at most one engine-selected provider
/// operation, tool invocation or memory-maintenance scope, and may settle
/// multiple local effects. Providers can retry and callbacks can make nested
/// calls synchronously. Dropping closes resources; durable suspension requires
/// a configured session file and a successful checkpoint.
pub struct SessionRun {
    pub(super) session: Session,
    scheduler: OrchestratorLoop,
    options: RunOptions,
    legacy: bool,
    control: RunControl,
    usage: UsageCollector,
    run_id: String,
    turn_id: u64,
    next_call: u64,
    next_event: u64,
    started: bool,
    contract: Value,
    active: Option<ActiveTurn>,
    approval: Option<ApprovalRequest>,
    approved: bool,
    intent: Option<ActionIntent>,
    events: Vec<RunEvent>,
    sink_failed: bool,
    terminal: Option<RunOutcome>,
    closing: Option<ClosingState>,
}

impl SessionRun {
    pub(super) fn new(mut session: Session, options: RunOptions, legacy: bool) -> Result<Self> {
        let usage = UsageCollector::new(options.budget.clone(), options.pricing.clone())?;
        let scheduler = match session.prepare(options.mode, legacy) {
            Ok(scheduler) => scheduler,
            Err(error) => {
                if session.store_open {
                    let store = Arc::clone(&lock(&session.shared.memories).store);
                    let _ = crate::usage::without_provider_calls(|| store.close_run());
                }
                return Err(error);
            }
        };
        static NEXT_RUN: AtomicU64 = AtomicU64::new(0);
        let run_id = format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT_RUN.fetch_add(1, Ordering::Relaxed)
        );
        let contract = contract(&session, &options, legacy);
        let saved = session.loop_position.get("runtime").cloned();
        let mut run = Self {
            session,
            scheduler,
            options,
            legacy,
            control: RunControl::default(),
            usage,
            run_id,
            turn_id: 0,
            next_call: 0,
            next_event: 0,
            started: false,
            contract,
            active: None,
            approval: None,
            approved: false,
            intent: None,
            events: Vec::new(),
            sink_failed: false,
            terminal: None,
            closing: None,
        };
        if run.options.mode == RunMode::HostDriven {
            run.scheduler.host_limit_reached()?;
        } else {
            run.scheduler.next_action()?;
        }
        if let Some(saved) = saved {
            run.restore(saved)?;
        }
        Ok(run)
    }

    pub fn control(&self) -> RunControl {
        self.control.clone()
    }
    pub fn outcome(&self) -> Option<&RunOutcome> {
        self.terminal.as_ref()
    }
    pub fn usage(&self) -> UsageLedger {
        self.usage.snapshot()
    }
    pub fn drain_events(&mut self) -> Vec<RunEvent> {
        std::mem::take(&mut self.events)
    }

    /// Persist a coherent boundary, including pending approval or tool intent.
    /// A configured session file is required for a durable checkpoint.
    pub fn checkpoint(&self) -> Result<()> {
        let Some(path) = &self.session.session_file else {
            return Ok(());
        };
        let mut loop_state = self.scheduler.snapshot();
        loop_state.insert(
            "runtime".into(),
            serde_json::to_value(self.snapshot())
                .map_err(|error| Error::session(error.to_string()))?,
        );
        save_snapshot(
            Path::new(path),
            &SessionSnapshot {
                identity: self.session.identity.clone(),
                turns: self.session.conversation.turns().to_vec(),
                transcript: self.session.conversation.transcript().to_vec(),
                loop_state,
                compactions: self.session.compactions,
            },
        )
    }

    pub fn step(&mut self, input: RunInput) -> Result<StepOutcome> {
        if let Some(outcome) = &self.terminal {
            return if matches!(input, RunInput::Continue) {
                Ok(StepOutcome::Finished {
                    outcome: outcome.clone(),
                })
            } else {
                Err(Error::session("Run is already finished."))
            };
        }
        if self.control.is_cancelled() {
            return self.finish(RunReason::Cancelled, None, None);
        }
        if let Err(error) = self.apply_input(input) {
            if let Some(budget) = self.usage.blocked_reason() {
                return self.finish(RunReason::BudgetExceeded { budget }, Some(error), None);
            }
            if std::mem::take(&mut self.sink_failed) {
                return self.finish(RunReason::Failed, Some(error), None);
            }
            return Err(error);
        }
        if let Some(outcome) = &self.terminal {
            return Ok(StepOutcome::Finished {
                outcome: outcome.clone(),
            });
        }
        match self.advance() {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let reason = if self.control.is_cancelled() {
                    RunReason::Cancelled
                } else if let Some(budget) = self.usage.blocked_reason() {
                    RunReason::BudgetExceeded { budget }
                } else {
                    RunReason::Failed
                };
                self.finish(reason, Some(error), None)
            }
        }
    }

    pub(super) fn run_to_completion(&mut self) -> Result<RunOutcome> {
        loop {
            match self.step(RunInput::Continue)? {
                StepOutcome::Finished { outcome } => return Ok(outcome),
                StepOutcome::Waiting { .. } => {
                    return Err(Error::session(
                        "Run needs host input; use step() to answer it.",
                    ))
                }
                StepOutcome::Progress => {}
            }
        }
    }

    fn apply_input(&mut self, input: RunInput) -> Result<()> {
        match input {
            RunInput::Continue => Ok(()),
            RunInput::Approve {
                request_id,
                approved,
            } => {
                let request = self
                    .approval
                    .as_ref()
                    .ok_or_else(|| Error::session("No approval is pending."))?;
                if request.request_id != request_id || self.approved || self.intent.is_some() {
                    return Err(Error::session(
                        "Approval is stale or does not match the pending request.",
                    ));
                }
                if approved {
                    self.approved = true;
                    self.checkpoint()
                } else {
                    let identity = request.identity.clone();
                    let result = tool_error(&request.call.name, "Approval denied.");
                    self.accept_tool_result(&identity, result)
                }
            }
            RunInput::Reconcile { action_id, result } => {
                let intent = self
                    .intent
                    .as_ref()
                    .ok_or_else(|| Error::session("No indeterminate action is pending."))?;
                if action_id != intent.identity.call_id() || result.name != intent.call.name {
                    return Err(Error::session(
                        "Reconciliation does not match the pending action.",
                    ));
                }
                let identity = intent.identity.clone();
                self.accept_tool_result(&identity, result)
            }
            RunInput::SelectAgent { agent, instruction } => {
                self.require_boundary()?;
                if self.options.mode != RunMode::HostDriven {
                    return Err(Error::session("Agent selection requires host_driven mode."));
                }
                if self.scheduler.host_limit_reached()? {
                    return Err(Error::session(
                        "The harness turn or phase limit is reached; supply Finish.",
                    ));
                }
                let instruction = self.scheduler.host_instruction(&instruction);
                self.begin_turn(
                    agent,
                    "host turn".into(),
                    Some(instruction),
                    LoopTurnKind::Participant,
                )
            }
            RunInput::UserMessage { text } => {
                self.require_boundary()?;
                self.session.conversation.directive(text);
                self.checkpoint()
            }
            RunInput::Finish { result } => {
                self.require_boundary()?;
                if self.options.mode != RunMode::HostDriven {
                    return Err(Error::session("Finish input requires host_driven mode."));
                }
                self.finish(RunReason::Completed, None, Some(result))
                    .map(|_| ())
            }
        }
    }

    fn require_boundary(&self) -> Result<()> {
        if self.active.is_some()
            || self.approval.is_some()
            || self.intent.is_some()
            || self.closing.is_some()
        {
            return Err(Error::session(
                "User input and agent selection require a turn boundary.",
            ));
        }
        Ok(())
    }

    fn advance(&mut self) -> Result<StepOutcome> {
        if !self.started {
            self.started = true;
            self.emit(RunEventKind::Started)?;
            return Ok(StepOutcome::Progress);
        }
        if self.closing.is_some() {
            return self.advance_closing();
        }
        if let Some(intent) = &self.intent {
            self.usage.check_next()?;
            return Ok(StepOutcome::Waiting {
                reason: WaitReason::Indeterminate {
                    action_id: intent.identity.call_id.clone(),
                    identity: intent.identity.clone(),
                    call: intent.call.clone(),
                },
            });
        }
        if let Some(request) = &self.approval {
            if !self.approved {
                if self.options.approvals == External {
                    self.usage.check_next()?;
                    return Ok(StepOutcome::Waiting {
                        reason: WaitReason::Approval {
                            request: request.clone(),
                        },
                    });
                }
                let request = request.clone();
                let prompt = lock(&self.session.shared.access)
                    .policy()
                    .approve_prompt
                    .clone();
                let (kind, target) = match &request.action {
                    PreflightAction::Command { command, .. } => ("command", command.as_str()),
                    PreflightAction::Confirm { description } => ("tool", description.as_str()),
                };
                if prompt.is_none() {
                    let result = tool_error(&request.call.name, &format!(
                        "Approval required for {target}. No approve_prompt is configured, so unlisted access is refused — allow it in the AccessPolicy, or pass approve_prompt=prompt_on_console to ask a human."
                    ));
                    self.accept_tool_result(&request.identity, result)?;
                    return Ok(StepOutcome::Progress);
                }
                let approved = prompt.is_some_and(|prompt| {
                    prompt.approve(&AccessRequest::new(
                        kind,
                        "run",
                        target,
                        request.identity.actor(),
                    ))
                });
                self.apply_input(RunInput::Approve {
                    request_id: request.request_id,
                    approved,
                })?;
                return Ok(StepOutcome::Progress);
            }
        }
        if self.active.is_some() {
            return self.advance_turn();
        }
        if self.options.mode == RunMode::HostDriven {
            self.usage.check_next()?;
            return Ok(StepOutcome::Waiting {
                reason: WaitReason::Input,
            });
        }
        match self.scheduler.next_action()? {
            LoopAction::Turn {
                agent,
                purpose,
                instruction,
                kind,
            } => {
                self.begin_turn(agent, purpose, instruction, kind)?;
            }
            LoopAction::Complete { .. } => return self.finish(RunReason::Completed, None, None),
            effect => self.apply_effect(effect)?,
        }
        Ok(StepOutcome::Progress)
    }

    fn apply_effect(&mut self, effect: LoopAction) -> Result<()> {
        match effect {
            LoopAction::Deliver {
                sender,
                text,
                turn,
                msg_type,
            } => {
                self.scheduler.acknowledge()?;
                self.deliver(&sender, &text, turn, &msg_type)?;
            }
            LoopAction::Directive { text } => {
                self.scheduler.acknowledge()?;
                self.session.conversation.directive(text);
                self.checkpoint()?;
            }
            LoopAction::Note { text } => {
                self.scheduler.acknowledge()?;
                self.session.conversation.note(&text);
                self.checkpoint()?;
                self.session.shared.channel.send_system(&text)?;
            }
            LoopAction::Summary { text, turn } => {
                self.scheduler.acknowledge()?;
                let actor = self.session.orchestrator_agent()?.name;
                self.session
                    .conversation
                    .say(&actor, &text, turn, "final_summary");
                self.checkpoint()?;
                self.session.shared.channel.send(&actor, &text)?;
                self.emit(RunEventKind::TurnCommitted { actor, text })?;
            }
            _ => return Err(Error::session("Expected a committed loop effect.")),
        }
        Ok(())
    }

    // Commit already-paid responses before applying cancellation or a budget
    // to the next operation. This never starts a provider or invokes a tool.
    fn settle_ready(&mut self) -> Result<()> {
        if let Some(active) = &self.active {
            let Some(turn) = &active.state else {
                return Ok(());
            };
            let Some(text) = turn.text() else {
                return Ok(());
            };
            let actor = active.agent.clone();
            let text = text.to_string();
            self.active = None;
            if self.options.mode == RunMode::HostDriven {
                self.scheduler.commit_host_turn(&actor)?;
                self.deliver(&actor, &text, self.scheduler.state().turn_count, "turn")?;
            } else {
                self.scheduler.submit_reply(text)?;
                self.checkpoint()?;
            }
        }
        if self.options.mode == RunMode::Automatic {
            loop {
                match self.scheduler.next_action()? {
                    LoopAction::Turn { .. } | LoopAction::Complete { .. } => break,
                    effect => self.apply_effect(effect)?,
                }
            }
        }
        Ok(())
    }

    fn begin_turn(
        &mut self,
        name: String,
        purpose: String,
        instruction: Option<String>,
        kind: LoopTurnKind,
    ) -> Result<()> {
        self.usage.check_next()?;
        let agent = self
            .session
            .agents
            .iter()
            .find(|agent| agent.name == name)
            .ok_or_else(|| Error::session(format!("Unknown agent: {name}")))?;
        let base_prompt = if kind == LoopTurnKind::Participant {
            if !agent.is_participant() {
                return Err(Error::session("Select a participant agent."));
            }
            self.session.participant_prompt(agent)?
        } else {
            self.session.orch_prompt.clone()
        };
        if kind == LoopTurnKind::Closing {
            if let Some(text) = &instruction {
                self.session.conversation.directive(text);
            }
        }
        self.session.shared.start_activation(&name);
        self.turn_id = self
            .turn_id
            .checked_add(1)
            .ok_or_else(|| Error::session("Run turn identity exhausted."))?;
        self.active = Some(ActiveTurn {
            agent: name,
            purpose,
            instruction: if kind == LoopTurnKind::Closing {
                None
            } else {
                instruction
            },
            base_prompt,
            state: None,
            needs_fit: true,
            overflow_retry: false,
        });
        self.checkpoint()
    }

    fn advance_turn(&mut self) -> Result<StepOutcome> {
        let active = self.active.as_ref().expect("active turn checked").clone();
        let agent = self
            .session
            .agents
            .iter()
            .find(|agent| agent.name == active.agent)
            .cloned()
            .ok_or_else(|| Error::session("Saved turn names an unregistered agent."))?;
        if active.needs_fit {
            self.usage.check_next()?;
            let collector = self.usage.clone();
            // Compaction is its own step; the agent call follows on a later one.
            let summarizer = self
                .session
                .orchestrator_agent()
                .ok()
                .or_else(|| self.session.agents.first().cloned())
                .unwrap();
            let fitted = collector.with_scope(&summarizer.name, "compaction", || {
                self.session.fit_conversation(
                    &agent,
                    &active.base_prompt,
                    if active.overflow_retry {
                        super::OVERFLOW_RETRY_FRACTION
                    } else {
                        1.0
                    },
                )
            });
            fitted?;
            let history = as_values(&self.session.conversation.render());
            let mut turn = match active.state {
                Some(mut turn) => {
                    turn.replace_history(&history)?;
                    turn
                }
                None => AgentTurn::new(
                    &history,
                    &active.purpose,
                    active.instruction.as_deref(),
                    self.session.shared.dialect_for(&agent),
                    self.session.max_tool_iterations,
                ),
            };
            self.record_exchanges(&mut turn);
            let current = self.active.as_mut().unwrap();
            current.state = Some(turn);
            current.needs_fit = false;
            self.checkpoint()?;
            return Ok(StepOutcome::Progress);
        }
        let mut turn = active
            .state
            .ok_or_else(|| Error::session("Active turn has no state."))?;
        if turn.is_complete() {
            self.settle_ready()?;
            return Ok(StepOutcome::Progress);
        }
        if let Some(call) = turn.pending_call().cloned() {
            return self.tool_step(call);
        }
        self.emit(RunEventKind::ProviderStarted {
            actor: agent.name.clone(),
            purpose: active.purpose.clone(),
        })?;
        if self.control.is_cancelled() {
            return self.finish(RunReason::Cancelled, None, None);
        }
        let provider = self
            .session
            .shared
            .provider_for(&agent)
            .ok_or_else(|| Error::session("Provider missing."))?;
        let assembling = Arc::clone(&self.session.shared);
        let advertising = Arc::clone(&self.session.shared);
        let mut runner = AgentRunner::new(
            &agent,
            provider.as_ref(),
            move |agent, history, base| assembling.prompts().messages_for(agent, history, base),
            &self.session.dispatcher,
            &active.base_prompt,
        )
        .with_tools(move || advertising.active_tools());
        if !self.legacy {
            runner = runner.with_strict_errors();
        }
        let result = self
            .usage
            .with_scope(&agent.name, &active.purpose, || runner.advance(&mut turn));
        drop(runner);
        self.record_exchanges(&mut turn);
        self.active.as_mut().unwrap().state = Some(turn);
        match result {
            Err(error) if error.is_context_overflow() && !active.overflow_retry => {
                let current = self.active.as_mut().unwrap();
                current.overflow_retry = true;
                current.needs_fit = true;
            }
            Err(error) => {
                self.checkpoint()?;
                return Err(error);
            }
            Ok(_) => {}
        }
        self.checkpoint()?;
        self.emit(RunEventKind::ProviderFinished {
            actor: agent.name,
            purpose: active.purpose,
        })?;
        Ok(StepOutcome::Progress)
    }

    fn tool_step(&mut self, call: ToolCall) -> Result<StepOutcome> {
        let actor = self.active.as_ref().unwrap().agent.clone();
        let tools = self.session.shared.active_tools();
        let spec = tools.iter().find(|spec| spec.name == call.name).cloned();
        let invalid = match spec.as_ref().filter(|_| call.name != INVALID_CALL) {
            None => Some(self.session.dispatcher.execute(&call, &actor)),
            Some(spec) => {
                let errors = validate_arguments(&spec.parameters, &call.arguments);
                (!errors.is_empty()).then(|| tool_error(&call.name, &errors.join("; ")))
            }
        };
        let identity = if let Some(request) = &self.approval {
            request.identity.clone()
        } else {
            self.next_call = self
                .next_call
                .checked_add(1)
                .ok_or_else(|| Error::session("Run call identity exhausted."))?;
            ToolIdentity {
                actor,
                run_id: self.run_id.clone(),
                turn_id: self.turn_id,
                call_id: format!("{}:{}:{}", self.run_id, self.turn_id, self.next_call),
            }
        };
        if let Some(result) = invalid {
            self.accept_tool_result(&identity, result)?;
            return Ok(StepOutcome::Progress);
        }
        if self.approval.is_none() {
            let action = if let Some(handler) = self.session.contextual_tools.get(&call.name) {
                handler.preflight(&call.arguments, &identity)
            } else if call.name == "cmd" {
                Ok(Some(PreflightAction::Command {
                    command: super::argument(&call.arguments, "command"),
                    cwd: None,
                }))
            } else {
                Ok(None)
            };
            let action = match action.and_then(|action| self.preflight(action, &identity)) {
                Ok(action) => action,
                Err(error) => {
                    self.accept_tool_result(&identity, tool_error(&call.name, &error.to_string()))?;
                    return Ok(StepOutcome::Progress);
                }
            };
            if let Some(action) = action {
                let request = ApprovalRequest {
                    request_id: identity.call_id.clone(),
                    identity,
                    call,
                    action,
                };
                self.approval = Some(request.clone());
                self.emit(RunEventKind::ApprovalRequested {
                    request: request.clone(),
                })?;
                return if self.options.approvals == External {
                    Ok(StepOutcome::Waiting {
                        reason: WaitReason::Approval { request },
                    })
                } else {
                    Ok(StepOutcome::Progress)
                };
            }
        }
        self.usage.begin_tool()?;
        self.intent = Some(ActionIntent {
            identity: identity.clone(),
            call: call.clone(),
        });
        self.emit(RunEventKind::ToolStarted {
            identity: identity.clone(),
            call: call.clone(),
        })?;
        if self.control.is_cancelled() {
            self.intent = None;
            return self.finish(RunReason::Cancelled, None, None);
        }
        let grant = self
            .approval
            .as_ref()
            .filter(|_| self.approved)
            .map(|request| &request.action);
        let context = ToolContext::new(
            identity.clone(),
            Arc::clone(&self.session.shared),
            self.control.clone(),
            self.options.approvals,
            grant,
        );
        let lease = context.lease();
        let called =
            self.usage
                .with_scope(identity.actor(), &format!("tool {}", call.name), || {
                    if let Some(handler) = self.session.contextual_tools.get(&call.name) {
                        handler.call(&call.arguments, &context)
                    } else if call.name == "cmd" {
                        context.run_command(
                            &super::argument(&call.arguments, "command"),
                            None,
                            Some(crate::exec::DEFAULT_TIMEOUT),
                        )
                    } else {
                        let spec = spec.unwrap();
                        spec.handler.call(
                            &call.arguments,
                            if spec.takes_actor {
                                identity.actor()
                            } else {
                                ""
                            },
                        )
                    }
                });
        drop(lease);
        let result = match called {
            Ok(content) => ToolResult {
                name: call.name,
                content,
                is_error: false,
            },
            Err(error) => tool_error(&call.name, &error.to_string()),
        };
        self.accept_tool_result(&identity, result)?;
        Ok(StepOutcome::Progress)
    }

    fn preflight(
        &self,
        action: Option<PreflightAction>,
        identity: &ToolIdentity,
    ) -> Result<Option<PreflightAction>> {
        let Some(PreflightAction::Command { command, cwd }) = action else {
            return Ok(action);
        };
        let manager = lock(&self.session.shared.access).clone();
        let cwd = cwd
            .as_deref()
            .unwrap_or_else(|| manager.workspace_for(identity.actor()));
        let cwd = manager.check_path(
            "Command working directory",
            &cwd.display().to_string(),
            identity.actor(),
        )?;
        // Syntax and hard denials are checked before any approval is exposed.
        if shell_words::split(&command)
            .map_err(|error| Error::session(format!("Invalid command syntax: {error}")))?
            .is_empty()
        {
            return Err(Error::session("Command has no program to run."));
        }
        match manager.command_approval(&command, identity.actor())? {
            None => Ok(None),
            Some(_) => Ok(Some(PreflightAction::Command {
                command,
                cwd: Some(cwd),
            })),
        }
    }

    fn accept_tool_result(&mut self, identity: &ToolIdentity, result: ToolResult) -> Result<()> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| Error::session("No tool invocation is active."))?;
        let mut turn = active
            .state
            .as_ref()
            .ok_or_else(|| Error::session("No turn state."))?
            .clone();
        let denied_command = (self.intent.is_none() && result.name == "cmd" && result.is_error)
            .then(|| {
                turn.pending_call()
                    .map(|call| super::argument(&call.arguments, "command"))
            })
            .flatten();
        turn.accept_tool_result(result.clone())?;
        self.record_exchanges(&mut turn);
        self.active.as_mut().unwrap().state = Some(turn);
        self.intent = None;
        self.approval = None;
        self.approved = false;
        self.checkpoint()?;
        if let Some(command) = denied_command {
            self.session
                .shared
                .channel
                .send_system(&format!("[Command:denied] {}: {command}", identity.actor()))?;
        }
        self.emit(RunEventKind::ToolFinished {
            identity: identity.clone(),
            result,
        })
    }

    fn record_exchanges(&mut self, turn: &mut AgentTurn) {
        for message in turn.take_recorded() {
            if self.session.tool_results_in_history {
                self.session.conversation.raw(
                    message
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    message
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
            }
        }
    }

    fn deliver(&mut self, actor: &str, text: &str, turn: i64, kind: &str) -> Result<()> {
        let (text, memory_error) = match self.session.process_memory_markers(text, actor) {
            Ok(cleaned) => (cleaned, None),
            Err(error) => (crate::utils::parse_memory_markers(text).0, Some(error)),
        };
        self.session.conversation.say(actor, &text, turn, kind);
        self.session.loop_position = self.scheduler.snapshot();
        self.checkpoint()?;
        if let Some(error) = memory_error {
            return Err(error);
        }
        self.session.shared.channel.send(actor, &text)?;
        self.emit(RunEventKind::TurnCommitted {
            actor: actor.into(),
            text,
        })?;
        if !self.session.turn_delay.is_zero() {
            std::thread::sleep(self.session.turn_delay);
        }
        Ok(())
    }

    fn finish(
        &mut self,
        mut reason: RunReason,
        mut error: Option<Error>,
        supplied: Option<Value>,
    ) -> Result<StepOutcome> {
        if let Some(closing) = self.closing.take() {
            let mut outcome = closing.outcome;
            outcome.reason = reason;
            outcome.error = error;
            return self.finalize(outcome);
        }
        if let Err(failure) = self.settle_ready() {
            if error.is_none() {
                reason = RunReason::Failed;
                error = Some(failure);
            }
        }
        let state = self.scheduler.state();
        let mut result = SessionResult {
            topic: self.session.topic.clone(),
            turns_completed: state.turn_count,
            consensus_reached: state.consensus_reached,
            history: self.session.conversation.transcript().to_vec(),
            final_summary: state.final_summary.clone(),
            fields: state.fields.clone(),
            rounds_run: state.rounds_run,
            phase_reached: state.phase_reached.clone(),
            end_reason: state.end_reason.as_str().into(),
        };
        let diagnostics = if let Some(value) = supplied {
            let diagnostics = outcome::validate(&value, &self.session.gameplan.harness.result);
            result.fields = value.as_object().cloned().unwrap_or_default();
            result.end_reason = "host_finished".into();
            diagnostics
        } else if reason == RunReason::Completed {
            let (fields, diagnostics) = outcome::parse(
                self.scheduler.raw_closing_result().unwrap_or_default(),
                &self.session.gameplan.harness.result,
            );
            if self.options.result_validation == ResultValidation::Strict {
                result.fields = fields;
            }
            diagnostics
        } else {
            ResultDiagnostics::default()
        };
        if reason == RunReason::Completed
            && !diagnostics.valid
            && self.options.result_validation == ResultValidation::Strict
        {
            reason = RunReason::InvalidResult;
        }
        if reason == RunReason::Completed && self.session.shared.memory_write {
            let store = Arc::clone(&lock(&self.session.shared.memories).store);
            let scope = lock(&self.session.shared.memories).session_scope.clone();
            let mut block = format!(
                "## Session Result\n- Consensus: {}",
                crate::pyfmt::repr(&Value::Bool(result.consensus_reached))
            );
            if !result.final_summary.is_empty() {
                block.push_str(&format!("\n- Summary: {}", result.final_summary));
            }
            if let Err(failure) = store.append(&scope, &block) {
                reason = RunReason::Failed;
                error = Some(failure);
            }
        }
        let outcome = RunOutcome {
            reason: reason.clone(),
            result,
            diagnostics,
            usage: self.usage.snapshot(),
            error,
        };
        if reason == RunReason::Completed {
            let store = Arc::clone(&lock(&self.session.shared.memories).store);
            let scopes = store.maintenance_scopes();
            if !scopes.is_empty() {
                self.closing = Some(ClosingState {
                    outcome,
                    scopes,
                    next_scope: 0,
                });
                self.checkpoint()?;
                return Ok(StepOutcome::Progress);
            }
        }
        self.finalize(outcome)
    }

    fn advance_closing(&mut self) -> Result<StepOutcome> {
        let closing = self.closing.as_ref().unwrap();
        if let Some(scope) = closing.scopes.get(closing.next_scope).cloned() {
            self.usage.check_next()?;
            let store = Arc::clone(&lock(&self.session.shared.memories).store);
            self.usage
                .with_scope("", "memory consolidation", || store.maintain_scope(&scope))?;
            // Stores may preserve their notes and absorb a provider error. A
            // denied retry must still become durable terminal state before
            // checkpointing the maintenance cursor past this scope.
            if let Some(budget) = self.usage.blocked_reason() {
                return Err(Error::session(budget.to_string()));
            }
            self.closing.as_mut().unwrap().next_scope += 1;
            self.checkpoint()?;
            Ok(StepOutcome::Progress)
        } else {
            let closing = self.closing.take().unwrap();
            self.finalize(closing.outcome)
        }
    }

    fn finalize(&mut self, mut outcome: RunOutcome) -> Result<StepOutcome> {
        if outcome.reason == RunReason::Completed {
            if let Some(budget) = self.usage.blocked_reason() {
                outcome.error = Some(Error::session(budget.to_string()));
                outcome.reason = RunReason::BudgetExceeded { budget };
            }
        }
        if let Err(failure) = self.close() {
            if outcome.error.is_none() {
                outcome.reason = RunReason::Failed;
                outcome.error = Some(failure);
            }
        }
        outcome.usage = self.usage.snapshot();
        let reason = outcome.reason.clone();
        self.terminal = Some(outcome);
        self.checkpoint()?;
        if let Err(failure) = self.emit(RunEventKind::Terminal { reason }) {
            let outcome = self.terminal.as_mut().unwrap();
            if outcome.error.is_none() {
                outcome.reason = RunReason::Failed;
                outcome.error = Some(failure);
            }
            self.checkpoint()?;
        }
        Ok(StepOutcome::Finished {
            outcome: self.terminal.clone().unwrap(),
        })
    }

    fn close(&mut self) -> Result<()> {
        if !self.session.store_open {
            return Ok(());
        }
        self.session.store_open = false;
        let store = Arc::clone(&lock(&self.session.shared.memories).store);
        self.usage.with_scope("", "memory close", || {
            crate::usage::without_provider_calls(|| store.close_run())
        })
    }

    fn emit(&mut self, event: RunEventKind) -> Result<()> {
        let event = RunEvent {
            sequence: self.next_event,
            run_id: self.run_id.clone(),
            turn_id: self.turn_id,
            event,
        };
        self.next_event = self
            .next_event
            .checked_add(1)
            .ok_or_else(|| Error::session("Run event sequence exhausted."))?;
        self.events.push(event.clone());
        self.checkpoint()?;
        if let Some(sink) = &self.options.event_sink {
            if let Err(error) = sink.emit(&event) {
                self.sink_failed = true;
                return Err(error);
            }
        }
        Ok(())
    }

    fn snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            run_id: self.run_id.clone(),
            turn_id: self.turn_id,
            next_call: self.next_call,
            next_event: self.next_event,
            started: self.started,
            contract: self.contract.clone(),
            active: self.active.clone(),
            approval: self.approval.clone(),
            approved: self.approved,
            intent: self.intent.clone(),
            loaded_skills: if self.active.is_some() {
                lock(&self.session.shared.activation)
                    .as_ref()
                    .map(|activation| activation.loaded())
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            context: lock(&self.session.shared.context_cache).clone(),
            usage: self.usage.snapshot(),
            terminal: self.terminal.clone(),
            closing: self.closing.clone(),
        }
    }

    fn restore(&mut self, value: Value) -> Result<()> {
        let saved: RuntimeSnapshot = serde_json::from_value(value)
            .map_err(|error| Error::session(format!("Invalid run checkpoint: {error}")))?;
        if saved.contract != self.contract {
            return Err(Error::session("Run checkpoint contracts differ. Re-register the same providers, tools, gameplan, options, and binding_version."));
        }
        if saved.run_id.is_empty() {
            return Err(Error::session("Run checkpoint has no run identity."));
        }
        if [saved.turn_id, saved.next_call, saved.next_event].contains(&u64::MAX) {
            return Err(Error::session(
                "Run checkpoint has exhausted identity counters.",
            ));
        }
        if let Some(request) = &saved.approval {
            if request.request_id != request.identity.call_id {
                return Err(Error::session(
                    "Run checkpoint approval identity is inconsistent.",
                ));
            }
        }
        if saved.approved && saved.approval.is_none() {
            return Err(Error::session(
                "Run checkpoint has an approval decision without a request.",
            ));
        }
        if let Some(closing) = &saved.closing {
            if closing.next_scope > closing.scopes.len()
                || saved.active.is_some()
                || saved.terminal.is_some()
            {
                return Err(Error::session(
                    "Run checkpoint has inconsistent memory maintenance progress.",
                ));
            }
        }
        if let Some(active) = &saved.active {
            if !self
                .session
                .agents
                .iter()
                .any(|agent| agent.name == active.agent)
            {
                return Err(Error::session("Run checkpoint names an unknown agent."));
            }
            if let Some(turn) = &active.state {
                turn.validate()?;
            }
            self.session.shared.start_activation(&active.agent);
            let activation = lock(&self.session.shared.activation).clone().unwrap();
            for skill in &saved.loaded_skills {
                activation.load(skill)?;
            }
            for (identity, call) in saved
                .approval
                .iter()
                .map(|request| (&request.identity, &request.call))
                .chain(
                    saved
                        .intent
                        .iter()
                        .map(|intent| (&intent.identity, &intent.call)),
                )
            {
                if identity.actor != active.agent
                    || identity.run_id != saved.run_id
                    || identity.turn_id != saved.turn_id
                    || identity.call_id
                        != format!("{}:{}:{}", saved.run_id, saved.turn_id, saved.next_call)
                    || active.state.as_ref().and_then(AgentTurn::pending_call) != Some(call)
                {
                    return Err(Error::session(
                        "Run checkpoint action does not match its active turn.",
                    ));
                }
            }
        } else if saved.approval.is_some()
            || saved.intent.is_some()
            || !saved.loaded_skills.is_empty()
        {
            return Err(Error::session(
                "Run checkpoint has suspended work without an active turn.",
            ));
        }
        self.usage = UsageCollector::restore(
            self.options.budget.clone(),
            self.options.pricing.clone(),
            saved.usage,
        )?;
        *lock(&self.session.shared.context_cache) = saved.context;
        self.run_id = saved.run_id;
        self.turn_id = saved.turn_id;
        self.next_call = saved.next_call;
        self.next_event = saved.next_event;
        self.started = saved.started;
        self.active = saved.active;
        self.approval = saved.approval;
        self.approved = saved.approved;
        self.intent = saved.intent;
        self.terminal = saved.terminal;
        self.closing = saved.closing;
        if self.terminal.is_some() {
            self.close()?;
        }
        Ok(())
    }
}

impl Drop for SessionRun {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            crate::logging::warning(&format!("Could not close run memory: {error}"));
        }
    }
}

fn tool_error(name: &str, content: &str) -> ToolResult {
    ToolResult {
        name: name.into(),
        content: format!("{name}: {content}"),
        is_error: true,
    }
}

fn contract(session: &Session, options: &RunOptions, legacy: bool) -> Value {
    let tools: Vec<_> = lock(&session.shared.tools).iter().map(|tool| json!({
        "name": tool.name, "parameters": tool.parameters, "description": tool.description,
        "takes_actor": tool.takes_actor, "contextual": session.contextual_tools.contains_key(&tool.name),
    })).collect();
    let agents: Vec<_> = session.agents.iter().map(|agent| json!({
        "name": agent.name, "model": agent.model, "provider": session.shared.provider_for(agent).map(|provider| provider.name().to_string()),
        "role": agent.role, "persona": agent.persona, "language": agent.language,
        "system_prompt": agent.system_prompt, "effort": format!("{:?}", agent.reasoning_effort),
        "tools": agent.tools, "skills": agent.skills, "workspace": agent.workspace, "memory": agent.memory,
        "resolved_skills": session.shared.skills_for(&agent.name).iter().map(|skill| format!("{skill:?}")).collect::<Vec<_>>(),
    })).collect();
    json!({ "gameplan": session.gameplan.raw_text, "agents": agents, "tools": tools,
        "max_rounds": session.max_rounds, "max_turns": session.max_turns,
        "max_tool_iterations": session.max_tool_iterations, "orchestrator_retries": session.orchestrator_retries,
        "access": format!("{:?}", lock(&session.shared.access).policy()),
        "memory_scope": lock(&session.shared.memories).session_scope,
        "memory_write": session.shared.memory_write, "max_context_tokens": session.max_context_tokens,
        "tool_results_in_history": session.tool_results_in_history,
        "prompts": session.prepared_prompts, "orchestrator_prompt": session.orch_prompt,
        "mode": options.mode, "approvals": options.approvals, "result_validation": options.result_validation,
        "budget": options.budget, "pricing": options.pricing, "binding_version": options.binding_version, "legacy": legacy })
}
