---
eatmycode_version: "2.0.0"
---

# Concurrent participant batches

Owner: [Session runtime](../modules/runtime.md)

Read when: changing batch scheduling/execution, provider overlap, per-turn tool
selection, grouped result delivery or batch checkpoint recovery.

## Contract

`loop.max_concurrent_agents` is a positive integer, default one. An automatic
orchestrator explicitly selects independent assignments using the fenced JSON
protocol owned by [harness/assets](../modules/harness-assets.md). Hosts use
`RunInput::SelectAgents { assignments }`; Python accepts the corresponding
`select_agents` dictionary. Changing the cap alone does not select a batch.
`AgentAssignment` contains `agent` and `instruction`.

Members are distinct participants still owing a turn in the current round and
must fit its remaining turn budget. Phase instructions apply to every member.
Legacy `@Name` routing and single-agent inputs retain their behavior
([orchestrator.rs](../../crates/kerness/src/orchestrator.rs)).

The owned driver [run/batch.rs](../../crates/kerness/src/session/run/batch.rs)
fits the shared conversation to the smallest available participant window, then
freezes one history for every assignment, including queued members. Each turn
owns its scratch, identity and skill activation. Provider requests capture
messages and offered tools on the calling thread; only immutable requests and
owned continuations cross worker threads. The existing shared activation slot
selects the turn being prepared or executing a tool; workers never consult it.
Memory is still read through the store during prompt preparation, so a frozen
conversation does not promise a transactional memory or filesystem snapshot.

An explicit batch step executes up to the configured cap of blocking provider
operations on scoped threads and joins the entire wave before returning.
Provider retries remain inside those calls. All responses are retained before
checkpointing or observer callbacks. Tools, approvals, memory writes and event
sinks execute serially on the caller's thread. A pending approval pauses the
batch; provider calls and arbitrary callbacks cannot be forcibly interrupted.
Serialization prevents simultaneous tool effects but does not merge conflicting
edits or isolate host callbacks' external IO. Assign independent tasks.

Completed replies remain buffered until join, then enter shared history in
assignment order, together with optional text tool exchanges. The scheduler
queues deliveries and updates round/phase counters before callbacks; only after
the group settles does the orchestrator continue. Failure/cancellation/budget
termination preserves successful completed siblings and accounts for all calls
already admitted. In-flight measured token/cost usage can exceed the observed
threshold; operation reservations are atomic
([usage.rs](../../crates/kerness/src/usage.rs)).

Checkpoints retain membership, the frozen history, pending/completed turns,
per-turn loaded skills, the selected tool turn and scheduler delivery queue.
An approval or interrupted tool intent reuses the existing identity/reconciliation
protocol. Completed tool effects and buffered replies are not replayed. A crash
inside a provider wave can repeat calls whose responses had not been saved;
checkpoints contain no live threads. Existing sequential snapshots remain
readable through defaulted batch/turn fields
([run.rs](../../crates/kerness/src/session/run.rs)).

The lower-level `OrchestratorLoop::run` drives its synchronous `LoopHost`
callbacks serially even for an explicit batch. Concurrent execution is owned by
`SessionRun`, also used by `Session::run`. Python's blocking `run` and `step`
release the GIL; interpreter callbacks reacquire it. Python CPU bytecode is
subject to its interpreter's GIL behavior.

## Change and Verify

Read [harness/assets](../modules/harness-assets.md) for dispatch syntax and cap
validation, [providers](../modules/providers.md) for reservations/observation,
[tools/access](../modules/tools-access.md) for selected capabilities,
[memory/persistence](../modules/memory-persistence.md) for snapshot changes and
[Python bindings](../modules/python-bindings.md) for callback/GIL boundaries.

Use the [root verification](../../ARCHITECTURE.md#verification) commands and
rebuild the Python extension before testing. Relevant behavioral owners are:

- `tests/session_run.rs`: synchronized overlap/cap, frozen history, stable
  ordering, orchestrator synthesis and completed siblings on termination.
- `tests/skills_e2e.rs`: concurrent turn skill/tool isolation, workspace checks,
  serialized contextual effects and expired invocation leases.
- `tests/resume.rs`: buffered sibling plus approval, interrupted tool intent,
  and restoration during grouped delivery without replay.
- `usage.rs` tests: simultaneous observations, reservations, retry nesting and
  unwind cleanup; `orchestrator.rs` tests: selection, limits and grouped state.
- Python `test_session.py`: actual callback overlap through both owned stepping
  and blocking runs; `test_harness.py`: exposed cap and validation.

## Evidence and Gaps

Offline tests use scripted providers and bounded synchronization waits; they do
not establish live provider latency or vendor-side concurrency limits. The cap
bounds engine-selected provider operations, not nested IO created by host code.
Continuous independent agents, message inboxes, background work after a step,
parallel tool effects and forced cancellation are not implemented.
