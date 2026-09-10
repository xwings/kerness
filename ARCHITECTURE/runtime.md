---
eatmycode_version: "1.2.0"
---

# Runtime and data flow detail

Supporting page for [ARCHITECTURE.md](../ARCHITECTURE.md#runtime-and-data-flow):
the entry points on both surfaces, the preparation and execution path, every
data path with the functions it passes through, the configuration contracts
and where each is enforced, failure and recovery, the concurrency and resource
invariants, and the well-known constants both test suites assert.

## Entry points

**From Python.** `import kerness` runs `bindings/python/kerness/__init__.py`.
Line 12 calls `_core.bootstrap(exceptions, _enums.ToolDialect, <package dir>)`,
handing down the three things the extension cannot declare — the exception
classes, the `ToolDialect` enum, and the assets root — and `bootstrap`
(`bindings/python/src/lib.rs:36`) installs all four seams. The remaining imports
pull public names out of the per-subsystem shims; `__all__` at
`bindings/python/kerness/__init__.py:80` is the public surface.

**From Rust.** The caller sets `kerness::assets::set_root(...)` if the built-in
gameplans are wanted from outside the crate directory; otherwise
`$KERNESS_ASSETS` or `$CARGO_MANIFEST_DIR/assets` resolves it
(`crates/kerness/src/assets.rs:38`). Then `SessionConfig { .. }` →
`Session::new` → `add_agent` / `add_tool` / `add_skill` / `add_context` →
`start` or `run`. `crates/kerness/src/lib.rs:52` onward is the re-export list a
dependent reaches for.

## Preparation and execution

`Session::new` (`crates/kerness/src/session.rs:549`) loads and validates the
gameplan and confines the session's own write paths — memory file, session
file, channel logs — against the workspace before any turn. `Session::start`
(`crates/kerness/src/session.rs:914`) consumes the configuration, resolves the
roster, inherited defaults, permitted tools and context, personas, skills and
prompts, opens every memory scope, and returns an owned `SessionRun`.
`Session::run` (`crates/kerness/src/session.rs:900`) drives the same engine
with the legacy defaults.

`SessionRun::step` (`crates/kerness/src/session/run.rs:382`) applies host input,
then advances one engine-selected unit of work and returns `Progress`,
`Waiting` (host input, approval, or indeterminate tool intent) or `Finished`.
Automatic mode follows the gameplan through `OrchestratorLoop`; host-driven
mode accepts `select_agent`, `user_message`, `approve`, `reconcile` and
`finish` inputs. One agent turn is an `AgentRunner` advancing an `AgentTurn`:
assemble messages through `PromptAssembler`, call `Provider::chat_with_retries`
once, dispatch each pending tool call through `ToolDispatcher` under
`AccessManager`, feed results back, repeat until the turn's reason is known.

## Data paths

| Path | From | Through | To |
| --- | --- | --- | --- |
| Contract | `assets/gameplans/*.md` or a caller path | `gameplan::load_gameplan` → `yaml::parse` → `harness::parse_harness` → `validate_harness` | `HarnessSpec` + `Permitted` |
| Prompt | role file, persona, context sources, skills index, memory block, tool prompt | `agent::build_system_prompt` → `decorate_system_prompt` → `PromptAssembler` | `Vec<ChatMessage>` |
| Provider request | `ChatMessage`s + tool schemas | backend payload builder → `HttpTransport::post_json` | `ProviderResponse` + `NormalizedUsage` |
| Tool call | reply text or native tool block | `parse_tool_calls` / `parse_*_tool_calls` → `validate_arguments` → `ToolDispatcher::execute` | `ToolResult` rendered per dialect |
| Command | `run_command` arguments | `AccessManager::check_command` → `exec::run_command` in a process group with a deadline | captured output |
| Memory | `write_memory` tool or `@MEMORY:` marker | `remember` → `MemoryFilter` → `MemoryStore::append` | the scope; read back into the next prompt |
| Delivery | every turn and system note | `Channel::send` / `send_system`, `MultiChannel` fan-out | console, file, JSONL log, or a caller's channel |
| Checkpoint | scheduler + runtime + conversation + usage | `SessionRun::checkpoint` → `sessionfile::save_snapshot` | `session_file` (schema 2), atomically replaced |

## Configuration contracts

| Contract | Type | Where enforced |
| --- | --- | --- |
| Gameplan frontmatter | `HarnessSpec` (`crates/kerness/src/harness.rs:199`) | `parse_harness` (`:312`), `validate_harness` (`:353`) |
| Session options | `SessionConfig` (`crates/kerness/src/session.rs:111`) | `Session::new`, `prepare` (`:953`) |
| Run options | `RunOptions` (`crates/kerness/src/session/run.rs:70`): mode, approvals, budget, pricing, sink, result validation, `binding_version` | `Session::start` |
| Access policy | `AccessPolicy` (`crates/kerness/src/access.rs:186`; Python dataclass `bindings/python/kerness/access.py:22`) | `AccessManager` |
| Agent options | `Agent` (`crates/kerness/src/agent.rs:22`), inherited from `AgentDefaults` (`:313`) | `inherit` (`:274`) at preparation |
| Skill frontmatter | `SkillConfig` (`crates/kerness/src/skill/loader.rs:27`) | `load_skill` (`:78`) |
| Role and persona frontmatter | `RoleConfig` (`crates/kerness/src/role.rs:70`), `PersonaConfig` (`crates/kerness/src/persona.rs:17`) | their loaders |

## Failure and recovery

- Provider errors are retried with backoff (`DEFAULT_RETRIES`, `DEFAULT_BACKOFF_SEC`);
  a rejected native-tools or reasoning-effort body flips a one-way degrade latch
  and retries ([provider.md](provider.md)).
- A context-overflow refusal (`Error::is_context_overflow`) schedules one
  compaction to `OVERFLOW_RETRY_FRACTION` and retries once
  ([compaction.md](compaction.md)).
- A failing tool, an unknown tool, a schema violation and an unparseable call
  are all answered as text; `MAX_INVALID_CALLS` and `MAX_REPEATED_FAILURES`
  end the turn ([agent-runtime.md](agent-runtime.md)).
- A memory read failure mid-run yields an empty block and a logged warning;
  an append failure keeps the paid reply in committed history and returns the
  error ([memory.md](memory.md)).
- Strict results report `InvalidResult` diagnostics rather than coercing;
  budgets stop the next admitted operation with `BudgetExceeded`; a sink
  failure is terminal without replaying the completed action
  ([run.md](run.md)).
- A restored run validates counters, identities and the configuration contract
  before executing; a captured tool intent with no completion waits for
  `Reconcile` or cancellation ([sessionfile.md](sessionfile.md)).

## Concurrency and resource invariants

- On supported POSIX platforms execution stays on the calling thread. The
  binding releases the GIL around `step`, contextual
  `run_command` and the HTTP transport (`bindings/python/src/run.rs:209`,
  `:166`, `provider.rs:768`) so another Python thread can call
  `RunControl.cancel()`. The unsupported non-Unix command fallback uses pipe
  reader threads (`crates/kerness/src/exec.rs:214`).
- Process-wide slots are `RwLock`ed; the unit suite runs concurrently in one
  process, so a test that reads back its own double takes a static mutex
  (`crates/kerness/src/access.rs:1125`) and a test that only checks reachability
  asserts with `contains` (`crates/kerness/src/channel.rs:440`).
- Every command runs in its own process group with a deadline; output is drained
  nonblocking in 8 KiB passes; timeout kills the group, reaps the child and
  closes the pipes ([access.md](access.md)).
- Memory scopes open before the first turn and close exactly once on
  completion, failure, cancellation or drop; observed provider calls are refused
  during cleanup ([session.md](session.md)).
- Snapshot temporary files are created with `create_new`, synced, renamed, and
  removed on failure ([sessionfile.md](sessionfile.md)).

## Well-known constants

Values callers see in output or depend on in tests. Each is exported from the
Python package as well as the crate, and `crates/kerness/tests/public_api.rs:43`
and `bindings/python/tests/test_provider.py` assert them.

| Constant | Value | Owner |
| --- | --- | --- |
| `SCHEMA_VERSION` | `2` | `crates/kerness/src/sessionfile.rs:36` |
| `DEFAULT_MAX_CONTEXT_TOKENS` | `256_000` | `crates/kerness/src/session.rs:66` |
| `CHARS_PER_TOKEN` | `4` | `crates/kerness/src/compaction.rs:33` |
| `COMPACT_TO_FRACTION` | `0.5` | `crates/kerness/src/compaction.rs:40` |
| `OVERFLOW_RETRY_FRACTION` | `0.5` | `crates/kerness/src/session.rs:77` |
| `MAX_INVALID_CALLS` | `3` | `crates/kerness/src/agent_runtime.rs:30` |
| `MAX_REPEATED_FAILURES` | `3` | `crates/kerness/src/agent_runtime.rs:34` |
| `MEMORY_STALE_AFTER_DAYS` | `1` | `crates/kerness/src/prompting.rs:50` |
| `DEFAULT_KEEP_ENTRIES` | `20` | `crates/kerness/src/memory.rs:440` |
| `DEFAULT_MEMORY_BUDGET` | `2_200` | `crates/kerness/src/memory.rs:716` |
| `ENTRY_SEPARATOR` | `§` | `crates/kerness/src/memory.rs:725` |
| `DEFAULT_ROLE_FILE` | `participant.md` | `crates/kerness/src/role.rs:66` |
| `DEFAULT_TERMINATORS` | `CONSENSUS_REACHED`, `END_SESSION` | `crates/kerness/src/utils.rs:12` |
| `RESERVED_TOOL_NAMES` | `["Skill"]` | `crates/kerness/src/harness.rs:25` |
| `DEFAULT_TIMEOUT` | 60s | `crates/kerness/src/exec.rs:21` |
| `ReasoningEffort::default()` | `high` | `crates/kerness/src/provider/mod.rs:64` |
| `DEFAULT_REQUEST_TIMEOUT_SEC` | `60` | `crates/kerness/src/provider/mod.rs:40` |
| `DEFAULT_RETRIES` | `2` | `crates/kerness/src/provider/mod.rs:42` |
| `DEFAULT_BACKOFF_SEC` | `2.0` | `crates/kerness/src/provider/mod.rs:44` |
| `DEFAULT_TEMPERATURE` | `1.0` | `crates/kerness/src/provider/mod.rs:46` |
| `DEFAULT_TOP_P` | `1.0` | `crates/kerness/src/provider/mod.rs:48` |
| `DEFAULT_CLAUDE_MAX_TOKENS` | `4096` | `crates/kerness/src/provider/claude.rs:26` |
| `OPENAI_BASE_URL` | `https://api.openai.com/v1` | `crates/kerness/src/provider/openai.rs:18` |
| `OPENROUTER_BASE_URL` | `https://openrouter.ai/api/v1` | `crates/kerness/src/provider/openrouter.rs:15` |
| `CLAUDE_BASE_URL` | `https://api.anthropic.com/v1` | `crates/kerness/src/provider/claude.rs:16` |

The request defaults below `ReasoningEffort` are declared once and named twice:
the crate's four backends build their `Default` impls from them, and the Python
constructors write the same constants into their own signatures. A value spelled
out in both languages drifts silently; one both sides import cannot.
