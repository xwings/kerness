//! The orchestrator-driven command loop.
//!
//! The loop asks the orchestrator who speaks next, routes the turn to that
//! participant, checks for termination, and runs the closing turn. Routing
//! lives here once, so the normal path and the retry path cannot drift apart.
//!
//! Everything that bounds the loop comes from [`LoopSpec`], which is parsed
//! from the gameplan's frontmatter. That is the point: a gameplan declaring
//! `terminate_on: [ALL_DONE]` ends on `ALL_DONE` and does not end on
//! `END_SESSION`, with no Rust change.
//!
//! The loop owns control flow and nothing else. Provider calls, the
//! conversation, the channel, and memory all stay behind [`LoopHost`], so this
//! module has no idea what an LLM is.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::error::Result;
use crate::harness::{LoopSpec, ResultField, ResultType};
use crate::pyfmt;
use crate::utils::{keyword_in_text, parse_orchestrator_call, parse_session_end};

/// Emitted when the orchestrator cannot produce a routable reply. The literal
/// is a message to a human, not a protocol keyword — termination itself comes
/// from [`LoopSpec::terminate_on`].
pub const FORCED_END_NOTE: &str = "Orchestrator output unparseable after retries. Forcing END_SESSION.";

/// What a participant is told when the active phase is a rethink phase.
const RETHINK_INSTRUCTION: &str = "This is a rethink phase. Re-examine the position you took \
earlier against everything said since, and state plainly whether it changed and why. Repeating \
your opening position unchanged is only acceptable if you say what you considered and why it did \
not move you.";

static JSON_FENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```(?:json)?\s*\n(\{.*?\})\s*\n```").expect("static pattern"));

/// What the loop needs from the session, and nothing more.
///
/// Every call can fail, because every one of them reaches a provider, a
/// channel, or the session file. The loop propagates those to its caller and
/// takes no view on which are recoverable: retry policy belongs to the
/// provider, and a run that cannot reach its channel has nowhere left to
/// report a recovery anyway.
pub trait LoopHost {
    /// Run one orchestrator turn and return its reply.
    fn orchestrator_turn(&mut self, purpose: &str) -> Result<String>;

    /// Run one participant turn and return its reply.
    fn participant_turn(&mut self, name: &str, instruction: &str) -> Result<String>;

    /// Process memory markers, record the turn, and emit it.
    fn deliver(&mut self, sender: &str, text: &str, turn: i64, msg_type: &str) -> Result<()>;

    /// Emit a system message.
    fn note(&mut self, message: &str) -> Result<()>;

    /// Append a user-role directive to the conversation.
    fn directive(&mut self, text: &str) -> Result<()>;

    /// Run the final orchestrator turn and return its raw reply.
    fn closing_turn(&mut self, prompt: &str) -> Result<String>;

    /// Record and emit the closing summary.
    fn record_summary(&mut self, text: &str, turn: i64) -> Result<()>;

    /// Take a copy of the loop's position, for a host that persists it.
    ///
    /// A host that persists its state needs the position at the moment it
    /// writes, but it cannot ask for it: [`OrchestratorLoop::run`] borrows the
    /// host for the whole run, so the host cannot also hold the loop and call
    /// [`OrchestratorLoop::snapshot`] on it. The loop pushes instead of being
    /// polled: this is called whenever the position changes and always before
    /// a callback that might write it to disk. A host that keeps no session
    /// file ignores it, which is the default.
    fn record_position(&mut self, _snapshot: Map<String, Value>) {}
}

/// Why the loop stopped, for callers that need to tell "the harness said so"
/// from "the budget ran out".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EndReason {
    /// A terminator from `terminate_on` appeared in an orchestrator reply.
    Keyword,
    /// The declared phase list ran out.
    PhasesComplete,
    /// A phase-less harness ran its `max_rounds`.
    MaxRounds,
    /// The turn budget ran out.
    #[default]
    MaxTurns,
    /// The orchestrator never produced a routable reply.
    Forced,
}

impl EndReason {
    /// The name this reason is reported with, as it reaches callers on
    /// `SessionResult::end_reason`.
    pub fn as_str(self) -> &'static str {
        match self {
            EndReason::Keyword => "keyword",
            EndReason::PhasesComplete => "phases_complete",
            EndReason::MaxRounds => "max_rounds",
            EndReason::MaxTurns => "max_turns",
            EndReason::Forced => "forced",
        }
    }
}

/// Everything the loop tracks across turns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoopState {
    pub turn_count: i64,
    pub consensus_reached: bool,
    pub final_summary: String,
    pub fields: Map<String, Value>,
    /// Rounds completed across the whole session. A round completes when every
    /// participant has spoken once since the last one closed.
    pub rounds_run: i64,
    /// Name of the phase the session was in when it stopped; empty for a
    /// harness that declares none.
    pub phase_reached: String,
    pub end_reason: EndReason,
}

/// A phase with its round budget already clamped by `max_rounds`.
#[derive(Clone, Debug)]
struct Phase {
    name: String,
    instruction: String,
    rethink: bool,
    rounds: i64,
}

/// Owns the round counter and the phase pointer.
///
/// `loop.phases`, `PhaseSpec::rounds`, `loop.max_rounds` and `loop.advance_on`
/// are enforced here rather than rendered into the orchestrator's system prompt
/// and hoped for. Prompt text alone would leave `advance_on` worse than inert:
/// the prompt tells the orchestrator to write `NEXT_PHASE`, and a reply that
/// does so matches neither a terminator nor an `@Name`, so without a tracker it
/// would fall into the retry path and burn turns — the harness instructing the
/// model to do something the loop then punishes.
///
/// This type is what makes the declared structure real. It does not decide
/// *who* speaks — the loop stays orchestrator-driven — only what structure they
/// speak within, and when the structure is finished.
///
/// The rules, all of them:
///
/// - A **round** completes when every participant has taken a turn since the
///   last one closed. Participants who speak twice before a straggler speaks
///   once do not advance it; that is the point, and it is what stops an
///   orchestrator looping on its favourite participant forever.
/// - A **phase** lasts `min(phase.rounds, max_rounds)` rounds. `max_rounds` is
///   therefore a ceiling on any single phase, never a total: capping the total
///   would let a harness whose phases outlast it stop before its `rethink`
///   phase, destroying the one guarantee the phase list exists to give.
/// - A harness declaring **no** phases is one implicit phase of `max_rounds`
///   rounds. That is the only configuration in which `max_rounds` bounds the
///   whole session.
/// - **Exhausting the last phase ends the run** and hands over to the closing
///   turn. `terminate_on` remains an *early* exit, and `max_turns` remains the
///   hard stop above both.
struct PhaseTracker {
    participants: Vec<String>,
    max_rounds: i64,
    advance_on: String,
    phases: Vec<Phase>,
    index: usize,
    round_in_phase: i64,
    pending: Vec<String>,
    rounds_run: i64,
    exhausted: bool,
}

impl PhaseTracker {
    fn new(spec: &LoopSpec, participants: &[String]) -> Self {
        // Parsed gameplans require at least one, but a caller passing
        // `max_rounds = 0` means "skip participant rounds and go straight to
        // the closing verdict".
        let max_rounds = spec.max_rounds.max(0);
        let phases = spec
            .phases
            .iter()
            .map(|p| Phase {
                name: p.name.clone(),
                instruction: p.instruction.trim().to_string(),
                rethink: p.rethink,
                rounds: p.rounds.max(1).min(max_rounds),
            })
            .collect();
        PhaseTracker {
            participants: participants.to_vec(),
            max_rounds,
            advance_on: spec.advance_on.trim().to_string(),
            phases,
            index: 0,
            round_in_phase: 0,
            pending: participants.to_vec(),
            rounds_run: 0,
            exhausted: max_rounds == 0,
        }
    }

    /// Whether this harness declared phases.
    ///
    /// When it did not, the tracker still counts rounds so `max_rounds` can
    /// bound the session, but composes no instruction and issues no briefing —
    /// there is no phase to name, and a harness that declared no structure
    /// should not have one narrated at it.
    fn phased(&self) -> bool {
        !self.phases.is_empty()
    }

    fn active(&self) -> Option<&Phase> {
        self.phases.get(self.index)
    }

    fn phase_name(&self) -> &str {
        self.active().map(|p| p.name.as_str()).unwrap_or("")
    }

    /// The standing requirement for a turn taken right now.
    fn instruction(&self) -> String {
        let Some(phase) = self.active() else {
            return String::new();
        };
        let mut parts = vec![format!("[Phase: {}]", phase.name)];
        if !phase.instruction.is_empty() {
            parts.push(phase.instruction.clone());
        }
        if phase.rethink {
            parts.push(RETHINK_INSTRUCTION.to_string());
        }
        parts.join(" ")
    }

    /// A directive naming the active phase and who still owes a turn.
    ///
    /// Injected at the start and at every boundary. Without it an orchestrator
    /// has no way to know the loop closed a round underneath it, and will
    /// happily re-call a participant who has already spoken while the round
    /// never closes.
    fn briefing(&self) -> String {
        let Some(phase) = self.active() else {
            return String::new();
        };
        let total = phase.rounds;
        let current = (self.round_in_phase + 1).min(total);
        let owing = if self.pending.is_empty() {
            "nobody".to_string()
        } else {
            self.pending.join(", ")
        };
        let mut lines = vec![format!(
            "Active phase: {} (round {current} of {total}).",
            phase.name
        )];
        if !phase.instruction.is_empty() {
            lines.push(format!(
                "Instruction for participants: {}",
                phase.instruction
            ));
        }
        lines.push(format!("Yet to speak this round: {owing}."));
        if !self.advance_on.is_empty() {
            lines.push(format!(
                "Write {} to move to the next phase before its rounds are up.",
                self.advance_on
            ));
        }
        lines.join("\n")
    }

    /// Note that *name* spoke.
    ///
    /// Returns true when that turn closed a round — the caller's cue to
    /// re-brief the orchestrator, and to check [`PhaseTracker::exhausted`].
    fn record_turn(&mut self, name: &str) -> bool {
        if let Some(at) = self.pending.iter().position(|p| p == name) {
            self.pending.remove(at);
        }
        if !self.pending.is_empty() {
            return false;
        }
        self.rounds_run += 1;
        self.round_in_phase += 1;
        self.pending = self.participants.clone();
        let limit = self.active().map(|p| p.rounds).unwrap_or(self.max_rounds);
        if self.round_in_phase >= limit {
            self.next_phase();
        }
        true
    }

    /// Capture the phase pointer for a session file.
    ///
    /// The pointer is five private fields, none of which reach [`LoopState`].
    /// Without them a resumed run restarts its phase: a debate interrupted
    /// halfway through `rethink` would replay the whole rethink phase, having
    /// already paid for it once.
    fn snapshot(&self) -> Map<String, Value> {
        let mut state = Map::new();
        state.insert("index".to_string(), Value::from(self.index));
        state.insert(
            "round_in_phase".to_string(),
            Value::from(self.round_in_phase),
        );
        state.insert(
            "pending".to_string(),
            Value::from(self.pending.as_slice().to_vec()),
        );
        state.insert("rounds_run".to_string(), Value::from(self.rounds_run));
        state.insert("exhausted".to_string(), Value::from(self.exhausted));
        state
    }

    /// Put the phase pointer back where [`PhaseTracker::snapshot`] found it.
    ///
    /// The phase *list* is rebuilt from the harness rather than restored, so a
    /// clamped index is not paranoia: a gameplan edited between runs can leave
    /// a saved index pointing past the end.
    fn restore(&mut self, state: &Map<String, Value>) {
        if !self.phases.is_empty() {
            let last = self.phases.len() as i64 - 1;
            self.index = int_at(state, "index", 0).clamp(0, last) as usize;
        }
        self.round_in_phase = int_at(state, "round_in_phase", 0);
        // Participants who left the roster between runs owe nothing now.
        if let Some(Value::Array(pending)) = state.get("pending") {
            let kept = pending
                .iter()
                .filter_map(Value::as_str)
                .filter(|name| self.participants.iter().any(|p| p == name))
                .map(str::to_string)
                .collect();
            self.pending = kept;
        }
        self.rounds_run = int_at(state, "rounds_run", 0);
        self.exhausted = state
            .get("exhausted")
            .map(pyfmt::truthy)
            .unwrap_or(self.max_rounds == 0);
    }

    /// Whether *reply* asks to end the current phase early.
    ///
    /// This is the read-back that closes `advance_on`'s dead round-trip.
    fn advance_requested(&mut self, reply: &str) -> bool {
        if self.advance_on.is_empty() || !self.phased() {
            return false;
        }
        if !keyword_in_text(reply, &self.advance_on) {
            return false;
        }
        self.next_phase();
        true
    }

    fn next_phase(&mut self) {
        self.round_in_phase = 0;
        self.pending = self.participants.clone();
        if self.phases.is_empty() {
            // No phase list: the implicit single phase just ended, and there is
            // no next one. This is where max_rounds stops a phase-less harness.
            self.exhausted = true;
            return;
        }
        self.index += 1;
        if self.index >= self.phases.len() {
            self.index = self.phases.len() - 1;
            self.exhausted = true;
        }
    }
}

/// Runs one session's worth of turns.
pub struct OrchestratorLoop {
    spec: LoopSpec,
    orchestrator: String,
    participants: Vec<String>,
    result_fields: Vec<ResultField>,
    max_turns: i64,
    retries: i64,
    phases: PhaseTracker,
    resume: Map<String, Value>,
    state: LoopState,
}

impl OrchestratorLoop {
    /// A loop bound by *spec*, driven by *orchestrator_name*, routing to
    /// *participant_names*.
    ///
    /// The harness's own `max_turns` and `orchestrator_retries` apply until a
    /// caller overrides them, and the closing turn asks for prose until result
    /// fields are declared.
    pub fn new(
        spec: LoopSpec,
        orchestrator_name: impl Into<String>,
        participant_names: Vec<String>,
    ) -> Self {
        let phases = PhaseTracker::new(&spec, &participant_names);
        OrchestratorLoop {
            max_turns: spec.max_turns,
            retries: spec.orchestrator_retries,
            spec,
            orchestrator: orchestrator_name.into(),
            participants: participant_names,
            result_fields: Vec::new(),
            phases,
            resume: Map::new(),
            state: LoopState::default(),
        }
    }

    /// Declare the result shape the closing turn must report.
    pub fn with_result_fields(mut self, fields: Vec<ResultField>) -> Self {
        self.result_fields = fields;
        self
    }

    /// Override the harness's turn budget.
    pub fn with_max_turns(mut self, max_turns: i64) -> Self {
        self.max_turns = max_turns;
        self
    }

    /// Override the harness's retry budget.
    pub fn with_retries(mut self, retries: i64) -> Self {
        self.retries = retries;
        self
    }

    /// Continue from a previous run's [`OrchestratorLoop::snapshot`].
    pub fn with_resume_state(mut self, state: Map<String, Value>) -> Self {
        if let Some(Value::Object(phases)) = state.get("phases") {
            self.phases.restore(phases);
        }
        self.resume = state;
        self
    }

    /// Drive the session to termination and return what happened.
    pub fn run(&mut self, host: &mut dyn LoopHost) -> Result<LoopState> {
        self.state = self.initial_state();
        self.publish(host);
        self.brief(host)?;

        if !self.structure_complete() {
            while self.state.turn_count < self.max_turns {
                if self.advance(host)? {
                    break;
                }
            }
        }

        self.record_progress();
        if !self.orchestrator.is_empty() {
            self.closing_turn(host)?;
        }
        Ok(self.state.clone())
    }

    /// Capture the loop's position for a session file.
    ///
    /// Only what a continuation needs. `consensus_reached`, `final_summary`,
    /// `fields` and `end_reason` are verdicts of a *finished* run — an
    /// interrupted one has none, and a resumed one earns its own in the closing
    /// turn rather than inheriting stale ones.
    pub fn snapshot(&self) -> Map<String, Value> {
        let mut state = Map::new();
        state.insert("turn_count".to_string(), Value::from(self.state.turn_count));
        state.insert("phases".to_string(), Value::Object(self.phases.snapshot()));
        state
    }

    /// Start fresh, or where the last run stopped.
    ///
    /// `max_turns` is a budget for the whole session rather than for one
    /// process, so a resumed run carries its predecessor's turn count instead
    /// of getting a second full allowance.
    fn initial_state(&self) -> LoopState {
        LoopState {
            turn_count: int_at(&self.resume, "turn_count", 0),
            ..LoopState::default()
        }
    }

    fn publish(&self, host: &mut dyn LoopHost) {
        host.record_position(self.snapshot());
    }

    /// Tell the orchestrator where in the declared structure it is.
    fn brief(&self, host: &mut dyn LoopHost) -> Result<()> {
        if !self.phases.phased() || self.phases.exhausted {
            return Ok(());
        }
        let text = self.phases.briefing();
        if !text.is_empty() {
            host.directive(&text)?;
        }
        Ok(())
    }

    fn record_progress(&mut self) {
        self.state.rounds_run = self.phases.rounds_run;
        self.state.phase_reached = self.phases.phase_name().to_string();
    }

    /// Take one orchestrator turn plus whatever it routes to.
    ///
    /// Returns true when the session should stop.
    fn advance(&mut self, host: &mut dyn LoopHost) -> Result<bool> {
        let reply = self.orchestrator_says(host, "orchestrator turn")?;
        if self.terminates(&reply, host)? {
            return Ok(true);
        }

        // Read `advance_on` back before routing, so a reply that both ends the
        // phase and calls on someone ("NEXT_PHASE. @Alice, go.") runs that
        // participant under the phase they were just moved into.
        let advanced = self.phases.advance_requested(&reply);
        if advanced {
            if self.structure_complete() {
                return Ok(true);
            }
            self.brief(host)?;
        }

        if self.routes(&reply, host)? {
            return Ok(self.structure_complete());
        }
        if advanced {
            // A bare advance is a legitimate orchestrator move, not the
            // unparseable reply the retry path exists for.
            return Ok(false);
        }
        self.retry(host)
    }

    /// Whether the declared round structure has run out.
    ///
    /// Exhaustion ends the loop and hands over to the closing turn — the
    /// "agents run the complete round, then the judge answers" shape. It is not
    /// a failure, so nothing is noted to the channel beyond the reason.
    fn structure_complete(&mut self) -> bool {
        if !self.phases.exhausted {
            return false;
        }
        self.state.end_reason = if self.phases.phased() {
            EndReason::PhasesComplete
        } else {
            EndReason::MaxRounds
        };
        true
    }

    /// Re-ask an orchestrator whose reply named nobody and ended nothing.
    ///
    /// Returns true when the session should stop — either because the retry
    /// ended it, or because a budget ran out.
    fn retry(&mut self, host: &mut dyn LoopHost) -> Result<bool> {
        for _ in 0..self.retries {
            // Retries spend turns like any other, so they answer to max_turns
            // too. Without this a harness declaring max_turns: 4 could run 5,
            // because the retry loop only counted against its own budget.
            if self.state.turn_count >= self.max_turns {
                break;
            }
            let hint = self.hint();
            host.directive(&hint)?;
            let reply = self.orchestrator_says(host, "orchestrator retry")?;
            if self.terminates(&reply, host)? {
                return Ok(true);
            }
            let advanced = self.phases.advance_requested(&reply);
            if advanced && self.structure_complete() {
                return Ok(true);
            }
            if advanced {
                self.brief(host)?;
            }
            if self.routes(&reply, host)? {
                return Ok(self.structure_complete());
            }
            if advanced {
                return Ok(false);
            }
        }

        if self.state.turn_count >= self.max_turns {
            return Ok(true);
        }
        self.state.end_reason = EndReason::Forced;
        host.note(FORCED_END_NOTE)?;
        Ok(true)
    }

    fn orchestrator_says(&mut self, host: &mut dyn LoopHost, purpose: &str) -> Result<String> {
        let reply = host.orchestrator_turn(purpose)?;
        self.state.turn_count += 1;
        self.publish(host);
        host.deliver(
            &self.orchestrator,
            &reply,
            self.state.turn_count,
            "orchestrator",
        )?;
        Ok(reply)
    }

    /// Check the reply against the harness's own terminator list.
    fn terminates(&mut self, reply: &str, host: &mut dyn LoopHost) -> Result<bool> {
        let Some(keyword) = parse_session_end(reply, &self.spec.terminate_on) else {
            return Ok(false);
        };
        let consensus = self.spec.consensus_keyword() == Some(keyword.as_str());
        self.state.consensus_reached = consensus;
        self.state.end_reason = EndReason::Keyword;
        host.note(&format!("Session ended: {keyword}"))?;
        Ok(true)
    }

    /// Run the participant the orchestrator addressed, if it addressed one.
    fn routes(&mut self, reply: &str, host: &mut dyn LoopHost) -> Result<bool> {
        let Some((name, instruction)) = parse_orchestrator_call(reply, &self.participants) else {
            return Ok(false);
        };
        if self.state.turn_count >= self.max_turns {
            // The orchestrator's own turn consumed the last of the budget.
            // Routing anyway would run a participant past the declared limit.
            return Ok(true);
        }
        let asked = if instruction.is_empty() {
            reply
        } else {
            &instruction
        };
        let answer = host.participant_turn(&name, &self.turn_instruction(asked))?;
        self.state.turn_count += 1;
        self.publish(host);
        host.deliver(&name, &answer, self.state.turn_count, "turn")?;
        if self.phases.record_turn(&name) && !self.phases.exhausted {
            self.brief(host)?;
        }
        Ok(true)
    }

    /// Compose the orchestrator's ask with the active phase's requirement.
    ///
    /// The phase text goes last, and reaches the participant whether or not the
    /// orchestrator remembered to relay it. Relying on the orchestrator to
    /// carry it is what made the phase list advisory: one forgetful routing
    /// turn and a participant answers with no idea it is in a rethink phase.
    fn turn_instruction(&self, asked: &str) -> String {
        let standing = self.phases.instruction();
        if standing.is_empty() {
            return asked.to_string();
        }
        if asked.is_empty() {
            standing
        } else {
            format!("{asked}\n\n{standing}")
        }
    }

    fn hint(&self) -> String {
        format!(
            "Your last response didn't contain an @Name mention or one of these keywords: {}. \
             Please either call on a participant using @Name or end the session.",
            self.spec.terminate_on.join(", ")
        )
    }

    /// Ask for the summary, and for the declared result fields with it.
    ///
    /// Two passes when `loop.verdict_rethink` is on. The orchestrator is also
    /// the judge, and the phase list gives its rethink to participants only —
    /// the verdict itself was written once, in a single call, and committed
    /// unread. The second pass hands the draft back and asks the judge to check
    /// it against the transcript it just watched.
    ///
    /// Only the committed pass is recorded. The draft is never delivered, never
    /// emitted, and never reaches memory: a session must not end with two
    /// summaries in its transcript, one of them superseded.
    fn closing_turn(&mut self, host: &mut dyn LoopHost) -> Result<()> {
        let mut raw = host.closing_turn(&closing_prompt(&self.result_fields))?;
        if self.spec.verdict_rethink {
            raw = host.closing_turn(&verdict_rethink_prompt(&raw, &self.result_fields))?;
        }
        self.state.fields = parse_result_fields(&raw, &self.result_fields);
        self.state.final_summary = strip_result_block(&raw);
        self.publish(host);
        host.record_summary(&self.state.final_summary, self.state.turn_count)
    }
}

/// Build the closing instruction, including the declared result shape.
///
/// *fields* is the gameplan's `result:` declaration; empty asks for prose.
pub fn closing_prompt(fields: &[ResultField]) -> String {
    let prose = "The session has ended. Provide a final, neutral summary of the discussion and \
                 any conclusions reached, in 3-5 sentences.";
    if fields.is_empty() {
        return prose.to_string();
    }

    let lines = fields
        .iter()
        .map(|f| {
            let mut line = format!("  \"{}\": <{}>", f.name, f.type_name);
            if !f.description.is_empty() {
                line.push_str(&format!("   // {}", f.description));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{prose}\n\nThen, after the summary, append a fenced JSON block with exactly these \
         fields:\n\n```json\n{{\n{lines}\n}}\n```"
    )
}

/// Build the second closing pass: hand the draft back for revision.
///
/// The draft is embedded rather than referred to, because the judge's own reply
/// is not necessarily still in its context window by the time this is asked,
/// and "revise your summary" against a summary it cannot see produces a fresh
/// one written from the same single reading.
pub fn verdict_rethink_prompt(draft: &str, fields: &[ResultField]) -> String {
    let mut ask = String::from(
        "That was a draft. Before it is recorded, check it against the transcript you have just \
         moderated:\n\
         - Does it represent every participant's final position, or only the loudest?\n\
         - Did anyone change their mind? A summary that misses a reversal misses the session.\n\
         - Is anything asserted that nobody actually said?\n\n\
         Now write the final version. Reply with the summary alone — not your critique of the \
         draft, and not both versions. If the draft was already right, say so by repeating it.",
    );
    if !fields.is_empty() {
        ask.push_str(
            " Re-check the JSON block against your revised summary and include it, in the same \
             format, below the summary.",
        );
    }
    format!("Your draft summary:\n\n{draft}\n\n{ask}")
}

/// Read the declared result fields out of a closing reply.
///
/// A model that ignores the instruction, or emits malformed JSON, yields
/// type-appropriate defaults rather than an error. The closing turn is the last
/// thing that happens in a session; failing it would discard the whole
/// transcript over a formatting mistake.
///
/// Returns one entry per declared field, always — never a partial map.
pub fn parse_result_fields(text: &str, fields: &[ResultField]) -> Map<String, Value> {
    let mut out = Map::new();
    if fields.is_empty() {
        return out;
    }

    let parsed = extract_json(text);
    for spec in fields {
        let kind = spec.result_type();
        let value = match parsed.get(&spec.name) {
            Some(raw) if !raw.is_null() => coerce(raw, kind),
            _ => default_for(kind),
        };
        out.insert(spec.name.clone(), value);
    }
    out
}

/// Remove the fenced result JSON so it does not read as prose.
pub fn strip_result_block(text: &str) -> String {
    JSON_FENCE_RE.replace_all(text, "").trim().to_string()
}

/// Find the result object: a fenced block first, then a bare trailing one.
fn extract_json(text: &str) -> Map<String, Value> {
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(captures) = JSON_FENCE_RE.captures(text) {
        candidates.push(captures.get(1).expect("group 1 is not optional").as_str());
    }
    if let Some(start) = text.rfind('{') {
        let end = text.rfind('}').map(|at| at + 1).unwrap_or(0);
        if end > start {
            candidates.push(&text[start..end]);
        }
    }
    for candidate in candidates {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(candidate) {
            return map;
        }
    }
    Map::new()
}

/// A fresh default for one declared result field.
fn default_for(kind: ResultType) -> Value {
    match kind {
        ResultType::Bool => Value::Bool(false),
        ResultType::Int => Value::from(0),
        ResultType::Float => Value::from(0.0),
        ResultType::List => Value::Array(Vec::new()),
        ResultType::Dict => Value::Object(Map::new()),
        ResultType::Str => Value::String(String::new()),
    }
}

/// Bend a model-supplied value to the declared type, or fall back.
fn coerce(value: &Value, kind: ResultType) -> Value {
    match kind {
        ResultType::Bool => match value {
            Value::Bool(_) => value.clone(),
            other => {
                let text = pyfmt::str(other).trim().to_lowercase();
                Value::Bool(matches!(text.as_str(), "true" | "yes" | "1"))
            }
        },
        ResultType::Int => Value::from(as_int(value).unwrap_or(0)),
        ResultType::Float => Value::from(as_float(value).unwrap_or(0.0)),
        ResultType::List => match value {
            Value::Array(_) => value.clone(),
            other => Value::Array(vec![other.clone()]),
        },
        ResultType::Dict => match value {
            Value::Object(_) => value.clone(),
            _ => Value::Object(Map::new()),
        },
        ResultType::Str => match value {
            Value::String(_) => value.clone(),
            other => Value::String(pyfmt::json_dumps(other)),
        },
    }
}

/// An integer, for the values JSON can hold.
///
/// A bool is refused rather than counted as 0 or 1. A harness that declared
/// `type: int` and got `true` has a field the orchestrator filled in wrongly,
/// and quietly reading it as 1 would put a made-up number in the result.
fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(_) => None,
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|f| f.trunc() as i64)),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// `float(value)`, with the same refusal of bools.
fn as_float(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(_) => None,
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// Read an integer out of a restored snapshot, tolerating the spellings a
/// hand-edited session file might carry.
fn int_at(state: &Map<String, Value>, key: &str, fallback: i64) -> i64 {
    state.get(key).and_then(as_int).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use serde_json::json;

    use super::*;
    use crate::harness::PhaseSpec;

    /// Replays canned orchestrator replies and records what the loop did.
    struct StubHost {
        replies: VecDeque<String>,
        participant_reply: String,
        /// When set, the closing turn answers with this instead of popping the
        /// queue — so a test can oversupply routing replies without the
        /// leftovers standing in for the summary.
        closing_reply: Option<String>,
        /// An explicit script for the closing passes, in order. Only tests that
        /// care about the draft-vs-committed distinction need it.
        closing_replies: VecDeque<String>,
        last_closing: Option<String>,
        delivered: Vec<(String, String, String)>,
        notes: Vec<String>,
        directives: Vec<String>,
        /// Every closing prompt in order — `[0]` is the draft ask, the last is
        /// the one whose answer was committed.
        closing_prompts: Vec<String>,
        summary: Option<String>,
        /// `(name, instruction)` for every routed turn — what a participant was
        /// actually told, which is where the phase contract either lands or
        /// does not.
        routed: Vec<(String, String)>,
    }

    impl StubHost {
        fn new<S: AsRef<str>>(replies: &[S]) -> Self {
            StubHost {
                replies: replies.iter().map(|r| r.as_ref().to_string()).collect(),
                participant_reply: "Said something.".to_string(),
                closing_reply: None,
                closing_replies: VecDeque::new(),
                last_closing: None,
                delivered: Vec::new(),
                notes: Vec::new(),
                directives: Vec::new(),
                closing_prompts: Vec::new(),
                summary: None,
                routed: Vec::new(),
            }
        }

        fn closing(mut self, reply: &str) -> Self {
            self.closing_reply = Some(reply.to_string());
            self
        }

        fn closing_script(mut self, replies: &[&str]) -> Self {
            self.closing_replies = replies.iter().map(|r| r.to_string()).collect();
            self
        }

        fn senders(&self) -> Vec<&str> {
            self.delivered.iter().map(|d| d.0.as_str()).collect()
        }
    }

    impl LoopHost for StubHost {
        fn orchestrator_turn(&mut self, _purpose: &str) -> Result<String> {
            Ok(self
                .replies
                .pop_front()
                .unwrap_or_else(|| "(exhausted)".to_string()))
        }

        fn participant_turn(&mut self, name: &str, instruction: &str) -> Result<String> {
            self.routed
                .push((name.to_string(), instruction.to_string()));
            Ok(self.participant_reply.clone())
        }

        fn deliver(&mut self, sender: &str, text: &str, _turn: i64, msg_type: &str) -> Result<()> {
            self.delivered.push((
                sender.to_string(),
                text.to_string(),
                msg_type.to_string(),
            ));
            Ok(())
        }

        fn note(&mut self, message: &str) -> Result<()> {
            self.notes.push(message.to_string());
            Ok(())
        }

        fn directive(&mut self, text: &str) -> Result<()> {
            self.directives.push(text.to_string());
            Ok(())
        }

        fn closing_turn(&mut self, prompt: &str) -> Result<String> {
            self.closing_prompts.push(prompt.to_string());
            let reply = if let Some(scripted) = self.closing_replies.pop_front() {
                scripted
            } else if let Some(fixed) = &self.closing_reply {
                fixed.clone()
            } else if let Some(last) = &self.last_closing {
                // The verdict rethink hands the draft back and asks for a
                // revision. A stub with nothing further to say stands by its
                // draft, which is what the prompt tells a real orchestrator to
                // do when the draft was already right.
                last.clone()
            } else {
                self.replies
                    .pop_front()
                    .unwrap_or_else(|| "Final summary.".to_string())
            };
            self.last_closing = Some(reply.clone());
            Ok(reply)
        }

        fn record_summary(&mut self, text: &str, _turn: i64) -> Result<()> {
            self.summary = Some(text.to_string());
            Ok(())
        }
    }

    fn names() -> Vec<String> {
        vec!["Alice".to_string(), "Bob".to_string()]
    }

    fn driver(spec: LoopSpec) -> OrchestratorLoop {
        OrchestratorLoop::new(spec, "Mod", names())
    }

    fn run(host: &mut StubHost, spec: LoopSpec) -> LoopState {
        driver(spec).run(host).expect("the stub never fails")
    }

    fn think() -> PhaseSpec {
        PhaseSpec {
            name: "think".to_string(),
            instruction: "State your own view.".to_string(),
            rounds: 1,
            rethink: false,
        }
    }

    fn argue() -> PhaseSpec {
        PhaseSpec {
            name: "argue".to_string(),
            instruction: "Pick a side.".to_string(),
            rounds: 2,
            rethink: false,
        }
    }

    fn rethink() -> PhaseSpec {
        PhaseSpec {
            name: "rethink".to_string(),
            instruction: "Revisit your opening.".to_string(),
            rounds: 1,
            rethink: true,
        }
    }

    /// A spec whose structure is the thing under test.
    fn phased(phases: Vec<PhaseSpec>) -> LoopSpec {
        LoopSpec {
            phases,
            max_turns: 200,
            max_rounds: 10,
            ..LoopSpec::default()
        }
    }

    /// *n* orchestrator replies that each call on someone, alternating.
    fn routing(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("@{}, your turn.", ["Alice", "Bob"][i % 2]))
            .collect()
    }

    fn field(name: &str, type_name: &str) -> ResultField {
        ResultField {
            name: name.to_string(),
            type_name: type_name.to_string(),
            description: String::new(),
        }
    }

    // ---- Termination comes from the harness -------------------------------

    #[test]
    fn the_declared_keyword_ends_it_and_the_old_literal_does_not() {
        let spec = LoopSpec {
            terminate_on: vec!["ALL_DONE".to_string()],
            ..LoopSpec::default()
        };

        let mut declared = StubHost::new(&["ALL_DONE", "Summary."]);
        let state = run(&mut declared, spec.clone());
        assert_eq!(state.end_reason, EndReason::Keyword);
        assert!(declared
            .notes
            .contains(&"Session ended: ALL_DONE".to_string()));

        let mut undeclared =
            StubHost::new(&["END_SESSION", "@Alice, go.", "ALL_DONE", "Summary."]);
        let state = run(&mut undeclared, spec);
        assert!(undeclared
            .notes
            .contains(&"Session ended: ALL_DONE".to_string()));
        assert!(state.turn_count > 2, "{state:?}");
    }

    #[test]
    fn only_the_consensus_keyword_sets_the_consensus_flag() {
        let spec = LoopSpec {
            terminate_on: vec!["END_SESSION".to_string(), "CONSENSUS_REACHED".to_string()],
            ..LoopSpec::default()
        };

        let mut agreed = StubHost::new(&["CONSENSUS_REACHED", "Summary."]);
        assert!(run(&mut agreed, spec.clone()).consensus_reached);

        let mut plain = StubHost::new(&["END_SESSION", "Summary."]);
        assert!(!run(&mut plain, spec).consensus_reached);
    }

    #[test]
    fn the_hint_quotes_the_declared_keywords() {
        let mut host = StubHost::new(&["mumble", "ALL_DONE", "Summary."]);
        run(
            &mut host,
            LoopSpec {
                terminate_on: vec!["ALL_DONE".to_string()],
                ..LoopSpec::default()
            },
        );

        assert!(host.directives[0].contains("ALL_DONE"), "{:?}", host.directives);
    }

    // ---- Routing ----------------------------------------------------------

    #[test]
    fn an_at_mention_routes_only_to_a_participant_the_loop_knows() {
        let mut known = StubHost::new(&["@Alice, open.", "END_SESSION", "Summary."]);
        let state = run(&mut known, LoopSpec::default());
        assert_eq!(known.senders(), ["Mod", "Alice", "Mod"]);
        assert_eq!(state.turn_count, 3);

        let mut unknown = StubHost::new(&["@Carol, open.", "END_SESSION", "Summary."]);
        run(&mut unknown, LoopSpec::default());
        assert!(
            !unknown.directives.is_empty(),
            "an unroutable reply must be re-asked"
        );
    }

    // ---- Retries ----------------------------------------------------------

    #[test]
    fn retries_exhaust_into_a_forced_end_and_zero_means_none() {
        let mut exhausted = StubHost::new(&["mumble", "mumble", "mumble", "Summary."]);
        let state = driver(LoopSpec::default())
            .with_retries(2)
            .run(&mut exhausted).unwrap();
        assert_eq!(state.end_reason, EndReason::Forced);
        assert!(exhausted.notes.contains(&FORCED_END_NOTE.to_string()));

        let mut none = StubHost::new(&["mumble", "Summary."]);
        let state = driver(LoopSpec::default()).with_retries(0).run(&mut none).unwrap();
        assert_eq!(state.end_reason, EndReason::Forced);
        assert!(none.directives.is_empty());
    }

    #[test]
    fn a_recovering_retry_continues_the_session() {
        let mut host = StubHost::new(&["mumble", "@Alice, go.", "END_SESSION", "Summary."]);
        let state = driver(LoopSpec::default()).with_retries(2).run(&mut host).unwrap();

        assert_ne!(state.end_reason, EndReason::Forced);
        assert_eq!(host.senders(), ["Mod", "Mod", "Alice", "Mod"]);
    }

    #[test]
    fn the_retry_budget_comes_from_the_harness() {
        let mut replies = vec!["mumble".to_string(); 6];
        replies.push("Summary.".to_string());
        let mut host = StubHost::new(&replies);
        run(
            &mut host,
            LoopSpec {
                orchestrator_retries: 4,
                ..LoopSpec::default()
            },
        );

        assert_eq!(host.directives.len(), 4);
    }

    #[test]
    fn hitting_max_turns_during_retries_is_not_a_forced_end() {
        let mut host = StubHost::new(&["mumble"; 5]).closing("Summary.");
        let state = driver(LoopSpec::default())
            .with_max_turns(1)
            .with_retries(5)
            .run(&mut host).unwrap();

        assert_eq!(state.end_reason, EndReason::MaxTurns);
        assert!(!host.notes.contains(&FORCED_END_NOTE.to_string()));
    }

    // ---- Limits -----------------------------------------------------------

    #[test]
    fn max_turns_stops_a_loop_that_never_ends() {
        let mut host = StubHost::new(&["@Alice, go."; 20]);
        let state = driver(LoopSpec::default()).with_max_turns(6).run(&mut host).unwrap();
        assert!(state.turn_count <= 6, "{state:?}");
        assert_eq!(state.end_reason, EndReason::MaxTurns);
        assert!(
            host.summary.is_some(),
            "the limit must not skip the closing turn"
        );

        let harness = LoopSpec {
            max_turns: 4,
            ..LoopSpec::default()
        };
        let mut host = StubHost::new(&["@Alice, go."; 20]);
        assert!(run(&mut host, harness).turn_count <= 4);

        let mut host = StubHost::new(&["@Alice, go."; 20]);
        let state = driver(LoopSpec {
            max_turns: 40,
            ..LoopSpec::default()
        })
        .with_max_turns(2)
        .run(&mut host).unwrap();
        assert!(state.turn_count <= 2, "{state:?}");

        let mut host = StubHost::new(&["@Alice, go."; 20]);
        let state = driver(LoopSpec::default()).with_max_turns(3).run(&mut host).unwrap();
        assert_eq!(state.turn_count, 3);
    }

    #[test]
    fn retries_answer_to_max_turns_too() {
        let mut host = StubHost::new(&["mumble"; 20]);
        let state = driver(LoopSpec::default())
            .with_max_turns(2)
            .with_retries(5)
            .run(&mut host).unwrap();
        assert_eq!(state.turn_count, 2);
    }

    // ---- The closing prompt -----------------------------------------------

    #[test]
    fn declared_fields_are_named_in_the_prompt_and_nothing_else_is() {
        assert!(!closing_prompt(&[]).contains("json"));

        let prompt = closing_prompt(&[
            ResultField {
                name: "consensus".to_string(),
                type_name: "bool".to_string(),
                description: "Whether they agreed.".to_string(),
            },
            field("summary", "str"),
        ]);
        assert!(prompt.contains("\"consensus\": <bool>"), "{prompt}");
        assert!(prompt.contains("Whether they agreed."), "{prompt}");
    }

    #[test]
    fn the_loop_uses_the_declared_shape() {
        let mut host = StubHost::new(&["END_SESSION", "Done."]);
        OrchestratorLoop::new(LoopSpec::default(), "Mod", vec!["Alice".to_string()])
            .with_result_fields(vec![field("verdict", "str")])
            .run(&mut host).unwrap();

        assert!(host.closing_prompts[0].contains("\"verdict\""));
    }

    // ---- Result parsing ---------------------------------------------------

    fn shape() -> Vec<ResultField> {
        vec![
            field("consensus", "bool"),
            field("summary", "str"),
            field("points", "list"),
            field("score", "int"),
        ]
    }

    #[test]
    fn the_object_is_read_fenced_or_bare() {
        let fenced = "They agreed.\n\n```json\n{\"consensus\": true, \"summary\": \"Agreed.\", \
                      \"points\": [\"a\", \"b\"], \"score\": 7}\n```";
        assert_eq!(
            Value::Object(parse_result_fields(fenced, &shape())),
            json!({"consensus": true, "summary": "Agreed.", "points": ["a", "b"], "score": 7})
        );

        let bare = "Summary.\n\n{\"consensus\": false, \"summary\": \"No.\"}";
        let parsed = parse_result_fields(bare, &shape());
        assert_eq!(parsed["consensus"], json!(false));
        assert_eq!(parsed["summary"], json!("No."));
    }

    #[test]
    fn the_declared_fields_alone_decide_what_comes_back() {
        let defaults = json!({"consensus": false, "summary": "", "points": [], "score": 0});

        assert_eq!(
            Value::Object(parse_result_fields(
                "```json\n{\"summary\": \"S\"}\n```",
                &shape()
            )),
            json!({"consensus": false, "summary": "S", "points": [], "score": 0})
        );
        assert_eq!(
            Value::Object(parse_result_fields("nothing", &shape())),
            defaults
        );
        assert_eq!(
            Value::Object(parse_result_fields(
                "Summary.\n\n```json\n{not json\n```",
                &shape()
            )),
            defaults
        );
        assert_eq!(
            Value::Object(parse_result_fields(
                "nothing",
                &[field("score", "float"), field("meta", "dict")]
            )),
            json!({"score": 0.0, "meta": {}})
        );
        assert!(parse_result_fields("```json\n{\"a\": 1}\n```", &[]).is_empty());
    }

    #[test]
    fn every_declared_type_and_alias_is_coerced() {
        let fields = [
            field("string", "string"),
            field("integer", "integer"),
            field("number", "number"),
            field("boolean", "boolean"),
            field("mapping", "dict"),
        ];
        let text = "```json\n{\"string\": 3, \"integer\": \"4\", \"number\": \"2.5\", \
                    \"boolean\": \"yes\", \"mapping\": {\"ok\": true}}\n```";

        assert_eq!(
            Value::Object(parse_result_fields(text, &fields)),
            json!({
                "string": "3",
                "integer": 4,
                "number": 2.5,
                "boolean": true,
                "mapping": {"ok": true},
            })
        );

        // A scalar where a list was declared is wrapped, not discarded.
        assert_eq!(
            Value::Object(parse_result_fields(
                "```json\n{\"points\": \"just one\", \"consensus\": \"yes\"}\n```",
                &shape()
            )),
            json!({"points": ["just one"], "consensus": true, "summary": "", "score": 0})
        );
    }

    // ---- Summary text -----------------------------------------------------

    #[test]
    fn the_json_block_is_stripped_from_the_summary() {
        let text = "They agreed.\n\n```json\n{\"consensus\": true}\n```";
        assert_eq!(strip_result_block(text), "They agreed.");
        assert_eq!(strip_result_block("Just prose."), "Just prose.");
    }

    #[test]
    fn the_loop_records_the_stripped_text() {
        let mut host = StubHost::new(&[
            "END_SESSION",
            "They agreed.\n\n```json\n{\"consensus\": true}\n```",
        ]);
        let state = OrchestratorLoop::new(LoopSpec::default(), "Mod", vec!["Alice".to_string()])
            .with_result_fields(vec![field("consensus", "bool")])
            .run(&mut host).unwrap();

        assert_eq!(state.final_summary, "They agreed.");
        assert_eq!(Value::Object(state.fields), json!({"consensus": true}));
        assert_eq!(host.summary.as_deref(), Some("They agreed."));
    }

    // ---- No orchestrator --------------------------------------------------

    #[test]
    fn a_headless_loop_takes_no_turns() {
        let mut host = StubHost::new(&["unused"]);
        let state = OrchestratorLoop::new(LoopSpec::default(), "", vec!["Alice".to_string()])
            .with_max_turns(0)
            .run(&mut host).unwrap();

        assert_eq!(state.turn_count, 0);
        assert!(host.summary.is_none());
    }

    // ---- Phases reach participants ----------------------------------------

    #[test]
    fn the_phase_rides_every_routed_turn_without_displacing_the_ask() {
        let mut replies = routing(2);
        replies.push("END_SESSION".to_string());
        let mut every = StubHost::new(&replies).closing("Summary.");
        run(&mut every, phased(vec![think(), argue()]));

        assert_eq!(every.routed.len(), 2);
        for (_, instruction) in &every.routed {
            assert!(instruction.contains("[Phase: think]"), "{instruction}");
            assert!(instruction.contains("State your own view."), "{instruction}");
        }

        let mut asked =
            StubHost::new(&["@Alice, answer the cost question.", "END_SESSION", "S."]);
        run(&mut asked, phased(vec![think()]));

        let (name, instruction) = &asked.routed[0];
        assert_eq!(name, "Alice");
        assert!(instruction.contains("answer the cost question."), "{instruction}");
        assert!(instruction.contains("State your own view."), "{instruction}");
    }

    #[test]
    fn phases_arrive_in_declared_order() {
        let mut replies = routing(8);
        replies.push("END_SESSION".to_string());
        let mut host = StubHost::new(&replies).closing("Summary.");
        run(&mut host, phased(vec![think(), argue(), rethink()]));

        let mut seen: Vec<&str> = Vec::new();
        for (_, instruction) in &host.routed {
            for phase in ["think", "argue", "rethink"] {
                if instruction.contains(&format!("[Phase: {phase}]")) && !seen.contains(&phase) {
                    seen.push(phase);
                }
            }
        }
        assert_eq!(seen, ["think", "argue", "rethink"]);
    }

    #[test]
    fn a_rethink_phase_says_so_in_the_turn_itself() {
        let mut replies = routing(2);
        replies.push("END_SESSION".to_string());
        let mut host = StubHost::new(&replies).closing("Summary.");
        run(&mut host, phased(vec![rethink()]));

        assert!(host.routed[0].1.contains("rethink phase"), "{:?}", host.routed[0]);
        assert!(host.routed[0].1.contains("whether it changed"));
    }

    // ---- Rounds close -----------------------------------------------------

    #[test]
    fn it_takes_the_last_straggler_and_not_a_repeat_to_close_one() {
        let mut repeat =
            StubHost::new(&["@Alice, go.", "@Alice, again.", "END_SESSION", "Summary."]);
        assert_eq!(run(&mut repeat, phased(vec![think()])).rounds_run, 0);

        let mut straggler = StubHost::new(&[
            "@Alice, go.",
            "@Alice, again.",
            "@Bob, go.",
            "END_SESSION",
            "Summary.",
        ]);
        assert_eq!(
            run(&mut straggler, phased(vec![think(), argue()])).rounds_run,
            1
        );
    }

    #[test]
    fn the_briefing_names_who_still_owes_a_turn_and_is_reissued() {
        let mut opening = StubHost::new(&["@Alice, go.", "END_SESSION", "Summary."]);
        run(&mut opening, phased(vec![think()]));

        assert!(!opening.directives.is_empty());
        assert!(
            opening.directives[0].contains("Yet to speak this round: Alice, Bob."),
            "{}",
            opening.directives[0]
        );

        let mut turnover =
            StubHost::new(&["@Alice, go.", "@Bob, go.", "END_SESSION", "Summary."]);
        run(&mut turnover, phased(vec![think(), argue()]));

        assert!(turnover.directives.len() >= 2, "{:?}", turnover.directives);
        assert!(
            turnover.directives.last().unwrap().contains("argue"),
            "{:?}",
            turnover.directives
        );
    }

    // ---- Phases end the run -----------------------------------------------

    #[test]
    fn the_last_phase_running_out_stops_the_loop_and_still_closes() {
        let mut host = StubHost::new(&routing(20)).closing("Summary.");
        let state = run(&mut host, phased(vec![think()]));

        assert_eq!(state.end_reason, EndReason::PhasesComplete);
        assert_eq!(host.summary.as_deref(), Some("Summary."));
        assert_eq!(state.final_summary, "Summary.");
    }

    #[test]
    fn it_runs_the_declared_number_of_rounds_and_stops() {
        // think(1) + argue(2) + rethink(1) = 4 rounds, 2 participants each.
        let mut host = StubHost::new(&routing(40)).closing("Summary.");
        let state = run(&mut host, phased(vec![think(), argue(), rethink()]));

        assert_eq!(state.rounds_run, 4);
        assert_eq!(host.routed.len(), 8);
        assert_eq!(state.phase_reached, "rethink");
    }

    #[test]
    fn a_terminator_still_exits_early() {
        let mut host = StubHost::new(&["@Alice, go.", "END_SESSION", "Summary."]);
        let state = run(&mut host, phased(vec![think(), argue(), rethink()]));

        assert_eq!(state.end_reason, EndReason::Keyword);
        assert_eq!(state.rounds_run, 0);
    }

    // ---- advance_on is read back ------------------------------------------

    #[test]
    fn the_keyword_advances_the_phase_whether_or_not_it_routes() {
        let mut alone =
            StubHost::new(&["NEXT_PHASE", "@Alice, go.", "END_SESSION", "Summary."]);
        run(&mut alone, phased(vec![think(), argue()]));

        assert!(alone.routed[0].1.contains("[Phase: argue]"), "{:?}", alone.routed);
        assert!(!alone.notes.contains(&FORCED_END_NOTE.to_string()));
        assert!(!alone
            .directives
            .iter()
            .any(|d| d.contains("didn't contain an @Name")));

        let mut combined =
            StubHost::new(&["NEXT_PHASE. @Alice, go.", "END_SESSION", "Summary."]);
        run(&mut combined, phased(vec![think(), argue()]));

        assert!(combined.routed[0].1.contains("[Phase: argue]"));
    }

    #[test]
    fn advancing_past_the_last_phase_ends_the_run() {
        let mut host = StubHost::new(&["NEXT_PHASE", "Summary."]);
        let state = run(&mut host, phased(vec![think()]));

        assert_eq!(state.end_reason, EndReason::PhasesComplete);
        assert_eq!(host.summary.as_deref(), Some("Summary."));
    }

    #[test]
    fn a_harness_that_declares_no_keyword_ignores_the_word() {
        let mut host = StubHost::new(&["NEXT_PHASE", "@Alice, go.", "END_SESSION", "S."]);
        run(
            &mut host,
            LoopSpec {
                advance_on: String::new(),
                ..phased(vec![think(), argue()])
            },
        );

        assert!(host.routed[0].1.contains("[Phase: think]"), "{:?}", host.routed);
    }

    // ---- max_rounds is real -----------------------------------------------

    #[test]
    fn max_rounds_caps_a_single_phase() {
        let greedy = PhaseSpec {
            name: "argue".to_string(),
            instruction: "Argue.".to_string(),
            rounds: 99,
            rethink: false,
        };
        let mut host = StubHost::new(&routing(40)).closing("Summary.");
        let state = run(
            &mut host,
            LoopSpec {
                max_rounds: 2,
                ..phased(vec![greedy])
            },
        );

        assert_eq!(state.rounds_run, 2);
    }

    #[test]
    fn max_rounds_is_not_a_total_across_phases() {
        let mut host = StubHost::new(&routing(40)).closing("Summary.");
        let state = run(
            &mut host,
            LoopSpec {
                max_rounds: 3,
                ..phased(vec![think(), argue(), rethink()])
            },
        );

        assert_eq!(state.rounds_run, 4);
        assert_eq!(state.phase_reached, "rethink");
    }

    #[test]
    fn max_rounds_bounds_a_phase_less_harness() {
        let mut host = StubHost::new(&routing(40)).closing("Summary.");
        let state = run(
            &mut host,
            LoopSpec {
                max_turns: 200,
                max_rounds: 3,
                ..LoopSpec::default()
            },
        );

        assert_eq!(state.rounds_run, 3);
        assert_eq!(state.end_reason, EndReason::MaxRounds);
        assert_eq!(host.routed.len(), 6);
    }

    #[test]
    fn max_turns_still_outranks_max_rounds() {
        let mut host = StubHost::new(&routing(40)).closing("Summary.");
        let state = run(
            &mut host,
            LoopSpec {
                max_turns: 3,
                max_rounds: 99,
                ..LoopSpec::default()
            },
        );

        assert!(state.turn_count <= 3, "{state:?}");
        assert_eq!(state.end_reason, EndReason::MaxTurns);
    }

    #[test]
    fn a_phase_less_harness_is_briefed_nothing_and_reports_no_phase() {
        let mut host = StubHost::new(&["@Alice, make the case.", "END_SESSION", "Summary."]);
        let state = run(&mut host, LoopSpec::default());

        assert!(host.directives.is_empty());
        assert_eq!(
            host.routed,
            [("Alice".to_string(), "make the case.".to_string())]
        );
        assert_eq!(state.phase_reached, "");
    }

    // ---- The judge rethinks its verdict -----------------------------------

    const DRAFT: &str = "Alice won on the merits.";
    const FINAL: &str = "Neither won outright; Bob conceded a point Alice never answered.";

    #[test]
    fn the_draft_is_revised_and_only_the_revision_is_kept() {
        let mut host = StubHost::new(&["END_SESSION"]).closing_script(&[DRAFT, FINAL]);
        let state = run(&mut host, LoopSpec::default());

        assert_eq!(host.closing_prompts.len(), 2);
        assert!(host.closing_prompts[1].contains(DRAFT));
        // No fields were declared.
        assert!(!host.closing_prompts[1].contains("JSON"));

        assert_eq!(state.final_summary, FINAL);
        assert_eq!(host.summary.as_deref(), Some(FINAL));
        assert!(host.delivered.iter().all(|(_, text, _)| !text.contains(DRAFT)));
    }

    #[test]
    fn result_fields_are_asked_for_again_and_taken_from_the_second_pass() {
        let mut host = StubHost::new(&["END_SESSION"]).closing_script(&[
            "Alice won.\n\n```json\n{\"winner\": \"Alice\"}\n```",
            "On reflection, nobody did.\n\n```json\n{\"winner\": \"nobody\"}\n```",
        ]);
        let state = driver(LoopSpec::default())
            .with_result_fields(vec![field("winner", "str")])
            .run(&mut host).unwrap();

        assert!(host.closing_prompts[1].contains("JSON"));
        assert_eq!(Value::Object(state.fields), json!({"winner": "nobody"}));
    }

    #[test]
    fn the_verdict_rethink_is_on_by_default_and_a_harness_can_turn_it_off() {
        assert!(LoopSpec::default().verdict_rethink);

        let mut host = StubHost::new(&["END_SESSION"]).closing_script(&[DRAFT, FINAL]);
        let state = run(
            &mut host,
            LoopSpec {
                verdict_rethink: false,
                ..LoopSpec::default()
            },
        );

        assert_eq!(host.closing_prompts.len(), 1);
        assert_eq!(state.final_summary, DRAFT);
    }

    #[test]
    fn the_rethink_prompt_carries_the_draft_and_asks_for_exactly_what_is_declared() {
        let prompt = verdict_rethink_prompt("They agreed on nothing.", &[]);

        assert!(prompt.contains("They agreed on nothing."), "{prompt}");
        assert!(prompt.contains("not both versions"), "{prompt}");
        assert!(prompt.contains("change their mind"), "{prompt}");
        assert!(!prompt.contains("JSON"), "{prompt}");

        assert!(
            verdict_rethink_prompt("Draft.", &[field("verdict", "str")]).contains("JSON")
        );
    }

    // ---- Resuming ---------------------------------------------------------

    #[test]
    fn a_resumed_run_carries_its_predecessors_turn_count_and_phase() {
        let mut first = StubHost::new(&routing(3));
        let mut driver = driver(phased(vec![think(), argue(), rethink()]));
        driver.run(&mut first).unwrap();
        let saved = driver.snapshot();

        let mut resumed = StubHost::new(&["END_SESSION", "Summary."]);
        let state = OrchestratorLoop::new(phased(vec![think(), argue(), rethink()]), "Mod", names())
            .with_resume_state(saved.clone())
            .run(&mut resumed).unwrap();

        assert_eq!(
            state.turn_count,
            saved["turn_count"].as_i64().unwrap() + 1,
            "the second run starts where the first stopped"
        );
        assert!(
            resumed.directives[0].contains("argue"),
            "{:?}",
            resumed.directives
        );
    }

    #[test]
    fn a_saved_index_past_the_end_of_an_edited_gameplan_is_clamped() {
        let mut host = StubHost::new(&["END_SESSION", "Summary."]);
        let saved = json!({
            "turn_count": 4,
            "phases": {
                "index": 9,
                "round_in_phase": 0,
                "pending": ["Alice", "Carol"],
                "rounds_run": 2,
                "exhausted": false,
            },
        });
        let state = OrchestratorLoop::new(phased(vec![think(), argue()]), "Mod", names())
            .with_resume_state(saved.as_object().unwrap().clone())
            .run(&mut host).unwrap();

        assert_eq!(state.phase_reached, "argue");
        assert_eq!(state.rounds_run, 2);
        // Carol left the roster between runs and owes nothing now.
        assert!(
            host.directives[0].contains("Yet to speak this round: Alice."),
            "{:?}",
            host.directives
        );
    }
}
