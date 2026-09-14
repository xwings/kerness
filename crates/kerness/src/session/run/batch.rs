//! Bounded provider batches. Only immutable requests cross worker threads;
//! tools, activation selection, conversation commits and observers stay owned
//! by the calling thread.

use super::*;
use crate::conversation::{Conversation, Turn};
use crate::orchestrator::AgentAssignment;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BatchState {
    agents: Vec<String>,
    turn_count_before: i64,
    pending: Vec<ActiveTurn>,
    completed: Vec<ActiveTurn>,
    history: Option<Vec<Turn>>,
    committing: bool,
    interruption: Option<BatchInterruption>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchInterruption {
    reason: RunReason,
    error: Option<Error>,
}

impl ActiveTurn {
    pub(super) fn captured(&self) -> Self {
        let mut saved = self.clone();
        if let Some(activation) = &self.activation {
            saved.loaded_skills = activation.loaded();
        }
        saved
    }

    fn activate(&mut self, shared: &super::super::Shared) -> Result<()> {
        if self.activation.is_none() {
            let activation = shared.skills_registry.activation_for(&self.agent);
            for skill in &self.loaded_skills {
                activation.load(skill)?;
            }
            self.activation = Some(activation);
        }
        *lock(&shared.turn_agent) = Some(self.agent.clone());
        *lock(&shared.activation) = self.activation.clone();
        Ok(())
    }
}

impl BatchState {
    pub(super) fn is_interrupted(&self) -> bool {
        self.interruption.is_some()
    }

    pub(super) fn captured(&self) -> Self {
        Self {
            pending: self.pending.iter().map(ActiveTurn::captured).collect(),
            completed: self.completed.iter().map(ActiveTurn::captured).collect(),
            ..self.clone()
        }
    }

    pub(super) fn turn_id(&self, actor: &str) -> Option<u64> {
        self.completed
            .iter()
            .find(|turn| turn.agent == actor)
            .map(|turn| turn.id)
    }
}

impl SessionRun {
    pub(super) fn begin_batch(&mut self, assignments: Vec<AgentAssignment>) -> Result<()> {
        self.usage.check_next()?;
        let mut pending = Vec::with_capacity(assignments.len());
        let mut next_id = self.turn_id;
        for assignment in &assignments {
            let agent = self
                .session
                .agents
                .iter()
                .find(|agent| agent.name == assignment.agent && agent.is_participant())
                .ok_or_else(|| Error::session("Batch names an unknown participant."))?;
            next_id = next_id
                .checked_add(1)
                .ok_or_else(|| Error::session("Run turn identity exhausted."))?;
            pending.push(ActiveTurn {
                id: next_id,
                agent: agent.name.clone(),
                purpose: format!("turn from {}", agent.name),
                instruction: Some(assignment.instruction.clone()),
                base_prompt: self.session.participant_prompt(agent)?,
                state: None,
                needs_fit: true,
                overflow_retry: false,
                loaded_skills: Vec::new(),
                exchanges: Vec::new(),
                activation: Some(
                    self.session
                        .shared
                        .skills_registry
                        .activation_for(&agent.name),
                ),
            });
        }
        self.turn_id = next_id;
        self.batch = Some(BatchState {
            agents: assignments
                .into_iter()
                .map(|assignment| assignment.agent)
                .collect(),
            turn_count_before: self.scheduler.state().turn_count,
            pending,
            completed: Vec::new(),
            history: None,
            committing: false,
            interruption: None,
        });
        self.checkpoint()
    }

    /// Compact once for the tightest participant window, then freeze the same
    /// history for every assignment, including those queued behind the cap.
    fn prepare_batch(&mut self) -> Result<()> {
        self.usage.check_next()?;
        let mut available = usize::MAX;
        for turn in &mut self.batch.as_mut().unwrap().pending {
            turn.activate(&self.session.shared)?;
            let agent = self
                .session
                .agents
                .iter()
                .find(|agent| agent.name == turn.agent)
                .unwrap();
            let ceiling = self.session.context_ceiling(agent);
            let overhead = self.usage.with_scope(&agent.name, "compaction", || {
                self.session.prompt_overhead(agent, &turn.base_prompt)
            })?;
            if overhead >= ceiling {
                return Err(Error::session(format!(
                    "The prompt for {} meets or exceeds its context ceiling of {ceiling}.",
                    agent.name
                )));
            }
            available = available.min(ceiling - overhead);
        }
        let history = self.session.conversation.turns().to_vec();
        let history = self.compact_batch_history(&history, available)?;
        self.session.conversation.replace_turns(history.clone());
        let messages = render(&history);
        let batch = self.batch.as_mut().unwrap();
        for active in &mut batch.pending {
            let agent = self
                .session
                .agents
                .iter()
                .find(|agent| agent.name == active.agent)
                .unwrap();
            active.state = Some(AgentTurn::new(
                &messages,
                &active.purpose,
                active.instruction.as_deref(),
                self.session.shared.dialect_for(agent),
                self.session.max_tool_iterations,
            ));
            active.needs_fit = false;
        }
        batch.history = Some(history);
        self.checkpoint()
    }

    fn compact_batch_history(&mut self, history: &[Turn], available: usize) -> Result<Vec<Turn>> {
        let summarizer = self
            .session
            .orchestrator_agent()
            .ok()
            .or_else(|| self.session.agents.first().cloned())
            .unwrap();
        let mut failure = None;
        let compacted = self.usage.with_scope(&summarizer.name, "compaction", || {
            crate::compaction::compact(history, available, |turns| {
                match self.session.summarize(turns) {
                    Ok(text) => text,
                    Err(error) => {
                        failure = Some(error);
                        String::new()
                    }
                }
            })
        });
        if let Some(error) = failure {
            return Err(error);
        }
        if let Some(compacted) = compacted {
            self.session.compactions += 1;
            Ok(compacted)
        } else {
            Ok(history.to_vec())
        }
    }

    pub(super) fn advance_batch(&mut self) -> Result<StepOutcome> {
        if let Some(interruption) = self.batch.as_ref().unwrap().interruption.clone() {
            return self.finish(interruption.reason, interruption.error, None);
        }
        if self.batch.as_ref().unwrap().committing {
            self.drain_batch_commits()?;
            return Ok(StepOutcome::Progress);
        }
        if self.batch.as_ref().unwrap().history.is_none() {
            self.prepare_batch()?;
            return Ok(StepOutcome::Progress);
        }
        // A tool wait keeps the active turn selected across host calls. All
        // other continuations return to the batch before choosing the next wave.
        if let Some(mut active) = self.active.take() {
            active.activate(&self.session.shared)?;
            if active.state.as_ref().unwrap().pending_call().is_some() {
                self.active = Some(active);
                return self.advance_turn();
            }
            self.batch.as_mut().unwrap().pending.push(active);
        }
        self.collect_batch_results();
        if self.batch.as_ref().unwrap().pending.is_empty() {
            self.settle_batch(false)?;
            return Ok(StepOutcome::Progress);
        }
        self.batch
            .as_mut()
            .unwrap()
            .pending
            .sort_by_key(|turn| turn.id);
        // Tool effects are serialized in dispatch order. A turn's activation is
        // selected only on this thread, and provider requests capture its tools.
        if let Some(index) = self
            .batch
            .as_ref()
            .unwrap()
            .pending
            .iter()
            .position(|turn| turn.state.as_ref().unwrap().pending_call().is_some())
        {
            let mut active = self.batch.as_mut().unwrap().pending.remove(index);
            active.activate(&self.session.shared)?;
            self.active = Some(active);
            return self.advance_turn();
        }
        if let Some(index) = self
            .batch
            .as_ref()
            .unwrap()
            .pending
            .iter()
            .position(|turn| turn.needs_fit)
        {
            self.refit_batch_turn(index)?;
            return Ok(StepOutcome::Progress);
        }
        self.advance_batch_providers()?;
        Ok(StepOutcome::Progress)
    }

    fn refit_batch_turn(&mut self, index: usize) -> Result<()> {
        self.usage.check_next()?;
        let mut active = self.batch.as_ref().unwrap().pending[index].clone();
        active.activate(&self.session.shared)?;
        let agent = self
            .session
            .agents
            .iter()
            .find(|agent| agent.name == active.agent)
            .unwrap();
        let ceiling = self.session.context_ceiling(agent);
        let overhead = self.usage.with_scope(&agent.name, "compaction", || {
            self.session.prompt_overhead(agent, &active.base_prompt)
        })?;
        if overhead >= ceiling {
            return Err(Error::session("Batch prompt exceeds its context ceiling."));
        }
        let available =
            ((ceiling - overhead) as f64 * super::super::OVERFLOW_RETRY_FRACTION) as usize;
        let history = self.batch.as_ref().unwrap().history.clone().unwrap();
        let fitted = self.compact_batch_history(&history, available)?;
        active
            .state
            .as_mut()
            .unwrap()
            .replace_history(&render(&fitted))?;
        active.needs_fit = false;
        self.batch.as_mut().unwrap().pending[index] = active;
        self.checkpoint()
    }

    fn advance_batch_providers(&mut self) -> Result<()> {
        self.usage.check_next()?;
        let count = self.batch.as_ref().unwrap().pending.len().min(
            self.session
                .gameplan
                .harness
                .loop_spec
                .max_concurrent_agents,
        );
        let mut requests = Vec::with_capacity(count);
        for index in 0..count {
            let mut active = self.batch.as_ref().unwrap().pending[index].clone();
            active.activate(&self.session.shared)?;
            let agent = self
                .session
                .agents
                .iter()
                .find(|agent| agent.name == active.agent)
                .unwrap()
                .clone();
            let provider = self
                .session
                .shared
                .provider_for(&agent)
                .ok_or_else(|| Error::session("Provider missing."))?;
            let messages = self.usage.with_scope(&agent.name, &active.purpose, || {
                self.session.shared.prompts().messages_for(
                    &agent,
                    active.state.as_ref().unwrap().scratch(),
                    &active.base_prompt,
                )
            })?;
            let tools = self.session.shared.active_tools();
            self.emit_for(
                active.id,
                RunEventKind::ProviderStarted {
                    actor: active.agent.clone(),
                    purpose: active.purpose.clone(),
                },
            )?;
            requests.push((index, active, agent, provider, messages, tools));
        }
        if self.control.is_cancelled() {
            return Ok(());
        }
        let dispatcher = &self.session.dispatcher;
        let usage = &self.usage;
        let control = &self.control;
        let results = std::thread::scope(|scope| {
            let mut workers = Vec::new();
            let mut failure = None;
            for (index, mut active, agent, provider, messages, tools) in requests {
                let launched = std::thread::Builder::new().spawn_scoped(scope, move || {
                    if control.is_cancelled() {
                        return (index, active, Ok(()));
                    }
                    let mut runner = AgentRunner::new(
                        &agent,
                        provider.as_ref(),
                        move |_, _, _| Ok(messages.clone()),
                        dispatcher,
                        &active.base_prompt,
                    )
                    .with_tools(move || tools.clone())
                    .with_strict_errors();
                    let result = usage.with_scope(&agent.name, &active.purpose, || {
                        runner.advance(active.state.as_mut().unwrap()).map(|_| ())
                    });
                    (index, active, result)
                });
                match launched {
                    Ok(worker) => workers.push(worker),
                    Err(error) => {
                        failure = Some(Error::session(format!(
                            "Could not start batch worker: {error}"
                        )));
                        break;
                    }
                }
            }
            let mut results = Vec::new();
            for worker in workers {
                match worker.join() {
                    Ok(result) => results.push(result),
                    Err(_) => {
                        failure = Some(Error::session("Batch provider panicked."));
                    }
                }
            }
            (results, failure)
        });
        let (results, mut failure) = results;
        let mut finished = Vec::new();
        // Store every joined result before invoking any observer or saving a
        // checkpoint. Observer failure cannot discard already-paid siblings.
        for (index, mut active, result) in results {
            active
                .exchanges
                .extend(active.state.as_mut().unwrap().take_recorded());
            match result {
                Err(error) if error.is_context_overflow() && !active.overflow_retry => {
                    active.overflow_retry = true;
                    active.needs_fit = true;
                }
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
                Ok(()) => {}
            }
            finished.push((active.id, active.agent.clone(), active.purpose.clone()));
            self.batch.as_mut().unwrap().pending[index] = active;
        }
        self.checkpoint()?;
        for (id, actor, purpose) in finished {
            self.emit_for(id, RunEventKind::ProviderFinished { actor, purpose })?;
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    }

    fn collect_batch_results(&mut self) {
        let batch = self.batch.as_mut().unwrap();
        let mut pending = Vec::new();
        for turn in batch.pending.drain(..) {
            if turn.state.as_ref().is_some_and(AgentTurn::is_complete) {
                batch.completed.push(turn);
            } else {
                pending.push(turn);
            }
        }
        batch.pending = pending;
        batch.completed.sort_by_key(|turn| turn.id);
    }

    pub(super) fn settle_batch(&mut self, partial: bool) -> Result<()> {
        let mut failure = None;
        if !self.batch.as_ref().unwrap().committing {
            if self
                .active
                .as_ref()
                .is_some_and(|turn| turn.state.as_ref().is_some_and(AgentTurn::is_complete))
            {
                self.batch
                    .as_mut()
                    .unwrap()
                    .pending
                    .push(self.active.take().unwrap());
            }
            self.collect_batch_results();
            let batch = self.batch.as_ref().unwrap();
            let replies: Vec<_> = batch
                .completed
                .iter()
                .map(|turn| {
                    (
                        turn.agent.clone(),
                        turn.state.as_ref().unwrap().text().unwrap().to_string(),
                    )
                })
                .collect();
            if self.options.mode == RunMode::HostDriven {
                self.scheduler.submit_host_batch(replies)?;
            } else if partial {
                self.scheduler.submit_batch_partial(replies)?;
            } else {
                self.scheduler
                    .submit_batch(replies.into_iter().map(|(_, text)| text).collect())?;
            }
            self.batch.as_mut().unwrap().committing = true;
            if let Err(error) = self.checkpoint() {
                self.interrupt_batch(RunReason::Failed, Some(error.clone()));
                failure = Some(error);
            }
        }
        let drained = self.drain_batch_commits();
        failure.map_or(drained, Err)
    }

    pub(super) fn interrupt_batch(&mut self, reason: RunReason, error: Option<Error>) {
        if let Some(batch) = &mut self.batch {
            if batch.interruption.is_none() {
                batch.interruption = Some(BatchInterruption { reason, error });
            }
        }
    }

    fn drain_batch_commits(&mut self) -> Result<()> {
        let mut failure = None;
        loop {
            match self.scheduler.next_action()? {
                LoopAction::Turn { .. }
                | LoopAction::Batch { .. }
                | LoopAction::Complete { .. } => break,
                effect => {
                    // Each queued effect is consumed before delivery. Keep
                    // committing paid siblings even if observers fail again.
                    if let Err(error) = self.apply_effect(effect) {
                        self.interrupt_batch(RunReason::Failed, Some(error.clone()));
                        if failure.is_none() {
                            failure = Some(error);
                        }
                    }
                }
            }
        }
        let interrupted = self.batch.take().unwrap().interruption.is_some();
        if interrupted {
            // The last durable state carries the interruption and any queued
            // replies. The next checkpoint is the terminal outcome; never save
            // abandoned tools as a runnable standalone continuation.
            self.active = None;
            self.approval = None;
            self.approved = false;
            self.intent = None;
            return failure.map_or(Ok(()), Err);
        }
        if let Err(error) = self.checkpoint() {
            if failure.is_none() {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub(super) fn commit_batch_exchanges(&mut self, actor: &str) {
        if !self.session.tool_results_in_history {
            return;
        }
        if let Some(turn) = self
            .batch
            .as_ref()
            .and_then(|batch| batch.completed.iter().find(|turn| turn.agent == actor))
        {
            for message in &turn.exchanges {
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

    pub(super) fn validate_batch_snapshot(&mut self, saved: &RuntimeSnapshot) -> Result<()> {
        let Some(batch) = &saved.batch else {
            return Ok(());
        };
        let invalid = || Error::session("Run checkpoint has inconsistent batch state.");
        if batch.agents.is_empty() || batch.agents.len() > self.session.agents.len() {
            return Err(invalid());
        }
        if saved.active.is_some() && batch.history.is_none() {
            return Err(invalid());
        }
        if batch.turn_count_before < 0
            || batch.turn_count_before.checked_add(if batch.committing {
                batch.completed.len() as i64
            } else {
                0
            }) != Some(self.scheduler.state().turn_count)
            || batch.interruption.as_ref().is_some_and(|interruption| {
                matches!(
                    interruption.reason,
                    RunReason::Completed | RunReason::InvalidResult
                )
            })
            || (batch.committing
                && batch.interruption.is_none()
                && (!batch.pending.is_empty() || saved.active.is_some()))
        {
            return Err(invalid());
        }
        let mut names = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        let first_id = saved
            .turn_id
            .checked_sub(batch.agents.len() as u64)
            .and_then(|id| id.checked_add(1))
            .ok_or_else(invalid)?;
        for turn in batch
            .pending
            .iter()
            .chain(&batch.completed)
            .chain(saved.active.iter())
        {
            if !batch.agents.contains(&turn.agent)
                || !names.insert(turn.agent.clone())
                || turn.id == 0
                || turn.id > saved.turn_id
                || !ids.insert(turn.id)
                || !self
                    .session
                    .agents
                    .iter()
                    .any(|agent| agent.name == turn.agent && agent.is_participant())
                || (batch.history.is_some() != turn.state.is_some())
            {
                return Err(invalid());
            }
            let position = batch
                .agents
                .iter()
                .position(|name| name == &turn.agent)
                .unwrap();
            if turn.id != first_id + position as u64 {
                return Err(invalid());
            }
            if let Some(state) = &turn.state {
                state.validate()?;
            }
        }
        if names.len() != batch.agents.len()
            || batch
                .agents
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != batch.agents.len()
            || batch
                .completed
                .iter()
                .any(|turn| !turn.state.as_ref().is_some_and(AgentTurn::is_complete))
        {
            return Err(invalid());
        }
        if batch.committing {
            if matches!(self.scheduler.next_action()?, LoopAction::Batch { .. }) {
                return Err(invalid());
            }
        } else if self.options.mode == RunMode::Automatic {
            let LoopAction::Batch { assignments } = self.scheduler.next_action()? else {
                return Err(invalid());
            };
            if assignments.iter().map(|a| &a.agent).ne(batch.agents.iter()) {
                return Err(invalid());
            }
        } else {
            let assignments: Vec<_> = batch
                .agents
                .iter()
                .map(|agent| AgentAssignment {
                    agent: agent.clone(),
                    instruction: String::new(),
                })
                .collect();
            self.scheduler.validate_host_batch(&assignments)?;
        }
        Ok(())
    }
}

fn render(turns: &[Turn]) -> Vec<Value> {
    let mut conversation = Conversation::new();
    conversation.replace_turns(turns.to_vec());
    as_values(&conversation.render())
}
