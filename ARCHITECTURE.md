---
eatmycode_version: "1.1.0"
---

# Kerness

## Mission and Constraints

Kerness is a framework for building **multi-agent harnesses**: sessions in which
several language models hold a structured conversation, call tools, consult a
shared memory, and produce a result with named fields.

The organising idea is that a **Markdown gameplan is the program**. Its YAML
frontmatter is a machine-readable contract — who the agents are, how many
rounds, which phases, which tools, what the result must contain — and its body
is the prose manual the orchestrator reads. A harness author writes Markdown;
the framework validates it, assembles the prompts, drives the loop, dispatches
the tools, and enforces the access boundary.

Two properties follow from that and shape every decision below:

- **The contract is total.** Every key the frontmatter parser accepts is
  validated, rendered into a prompt, or enforced at runtime. A key that parses
  and then does nothing is a bug, not a reserved word
  ([harness.md](ARCHITECTURE/harness.md)).
- **Everything is synchronous.** There is no executor, no async runtime, and no
  hidden concurrency. A session runs on the calling thread, and a stack trace
  from inside a tool handler reaches the calling `SessionRun::step` or
  `Session::run` ([run.md](ARCHITECTURE/run.md)).

### Two artifacts, one implementation

Kerness ships as two artifacts from one repository: a **Rust crate** for callers
who want the framework in a Rust program, and a **Python extension** for callers
who want to subclass `Provider`, pass a lambda as a tool handler, and hand a
`pydantic` model in for structured output. Both are supported surfaces.

**A feature is written in Rust.** The crate implements it, the extension exposes
it, and the installed Python package does one of five things and nothing else:
declares a class callers subclass (`Provider`, `Channel`, `MemoryStore`),
declares one the extension cannot (the exception hierarchy's structured
constructors, `ToolDialect` as a real `enum.Enum`, `AccessPolicy` as a dataclass
whose contract is written in Python list semantics), reads a signature with
`inspect`, validates with `pydantic`, or re-exports a name. Every other `.py` in
the package is a shim. A capability that exists only in Python is a defect
([bindings.md](ARCHITECTURE/bindings.md)).

Where a feature needs something only the interpreter has — `sys.stdout`, a
logger, `input`, an HTTP client under a caller's `mock.patch` — the crate names
the need as a trait, ships a default that works from Rust alone, and the
binding installs a replacement at `bootstrap`. The behaviour stays in one place
and only its delivery crosses:

| Seam | Crate default | What the binding installs |
| --- | --- | --- |
| `HttpTransport` (`crates/kerness/src/http.rs:24`) | `ureq` + `rustls` | `kerness.provider.http_post_json`, resolved per call so `mock.patch` reaches it |
| `Logger` (`crates/kerness/src/logging.rs:28`) | warnings and errors to stderr | `logging.getLogger("kerness")`, so `caplog` sees them |
| `ConsoleWriter` (`crates/kerness/src/channel.rs:54`) | this process's stdout | `builtins.print`, so `capsys` and a `StringIO` see it |
| `ConsolePrompt` (`crates/kerness/src/access.rs:69`) | `std::io::stdin` | `sys.stdin` / `builtins.input` |

### Supported platforms and observable limits

| | |
| --- | --- |
| Platform | Linux and macOS; developed on Linux x86-64. `ureq` + `rustls` reach further, but path confinement resolves every path from `/` — `crates/kerness/src/access.rs:713` — so the access boundary assumes POSIX paths. Command process groups and deadlines are `#[cfg(unix)]` (`crates/kerness/src/exec.rs:14`). |
| Network | Outbound HTTPS only, to provider endpoints the caller names. The framework ships no fetch tool; `allowed_hosts` narrows URLs on commands the caller allowed ([access.md](ARCHITECTURE/access.md)). |
| Process | No daemon, no database, no listening socket, no background thread. |
| Filesystem | Writes are confined to paths the caller opts into: whatever the memory store names for a scope, the session file, channel logs, and directories added to the access policy. |
| Runtime deps | None beyond the crate's Cargo dependencies; `pydantic` is optional and only for structured output. |

### Code-scope non-goals

These are decisions, each recorded with its reason in the owning module doc:

- No streaming; a response is one request and one reply ([provider.md](ARCHITECTURE/provider.md)).
- No parallel agent execution; the synchronous invariant is load-bearing ([run.md](ARCHITECTURE/run.md)).
- No embedded table of model context windows or prices; the caller supplies both ([provider.md](ARCHITECTURE/provider.md)).
- No per-model tokenizer; `CHARS_PER_TOKEN` plus a reactive retry ([compaction.md](ARCHITECTURE/compaction.md)).
- No MCP client, workflow adapter, or subagent scheduler yet (M4, [Roadmap](#roadmap)).
- No domain-specific bundled assets; `assets/` stays framework-generic ([gameplan.md](ARCHITECTURE/gameplan.md)).
- No hard token or cost budget; only measured thresholds ([provider.md](ARCHITECTURE/provider.md)).

### Compatibility

Compatibility is additive. Existing `ToolSpec`, `ToolHandler`, `Provider` and
`SessionSnapshot` public shapes remain usable; `Session::run` keeps legacy
result coercion, provider-error placeholders and synchronous approval callbacks,
while `Session::start` is strict by default. Session files are written at
`SCHEMA_VERSION` 2 and valid version-1 turn boundaries still load
([sessionfile.md](ARCHITECTURE/sessionfile.md)). The version is declared once, as
`[workspace.package] version` in the root `Cargo.toml`, and reaches Python as
`kerness.__version__` through `env!("CARGO_PKG_VERSION")`
(`bindings/python/src/funcs.rs:677`).

## Languages and Toolchain

| Area | Language | Declared support | Evidence |
| --- | --- | --- | --- |
| `crates/kerness/` | Rust, edition 2021 | MSRV **1.88**; stable toolchain | `Cargo.toml` `[workspace.package] rust-version = "1.88"`; CI `rust` job on `dtolnay/rust-toolchain@stable` |
| `bindings/python/src/` | Rust, `pyo3` 0.23 with `extension-module` + `abi3-py310` | same MSRV; `cdylib` named `_core` | `bindings/python/Cargo.toml` |
| `bindings/python/kerness/` | Python | **3.10+**, CPython, stable ABI | `pyproject.toml` `requires-python = ">=3.10"`; classifiers 3.10–3.13; CI matrix 3.10 and 3.13 |
| Build | `cargo` for the crate; `maturin` for the wheel | `maturin>=1.7,<2.0` | `pyproject.toml` `[build-system]` |
| Lint | `rustfmt` (defaults; no `rustfmt.toml`), `clippy -D warnings`, `rustdoc -D warnings`; `ruff` with `select = ["E4","E7","E9","F"]` and `target-version = "py310"` | `ruff>=0.16,<0.17` | `.github/workflows/ci.yml`; `pyproject.toml` `[tool.ruff]` |
| Test | `cargo test`; `pytest` with `pydantic` under the `dev` extra | `pytest>=7.0`, `pydantic>=2,<3` | `pyproject.toml` `[project.optional-dependencies]` |

Direct crate dependencies (`cargo tree --depth 1`): `fancy-regex` 0.14, `libc`
0.2 (unix only), `regex` 1.11, `serde` 1 (derive), `serde_json` 1
(`preserve_order`), `shell-words` 1.1, `ureq` 2.12 (`json`, `tls`, no gzip),
`yaml-rust2` 0.11 (event API, no `Value` deserializer). The reasons for the last
two are comments on the `[workspace.dependencies]` table in `Cargo.toml`. The
binding adds `pyo3` and `serde_json` only. No type checker is configured for
Python; the shims carry no annotations beyond `from __future__ import
annotations` in six files.

Locally observed (2026-09-06, not a claim of support): `cargo` 1.89 nightly
and `cargo +1.88.0`, Python 3.13.5 in `.venv`, `ruff` 0.16.5, `maturin` 1.15.0,
`pytest` 9.1.1, `pydantic` 2.13.5. `maturin` and `python` are not on `PATH` in
this workspace; invoke them from `.venv/bin/`.

Two toolchain facts are wrong in the tree today and are recorded under
[Roadmap](#roadmap): the CI MSRV job pins `dtolnay/rust-toolchain@1.120.0`, a
Rust version that does not exist (`.github/workflows/ci.yml:62`), and the
committed `Cargo.lock` still records `0.1.1-dev` for both workspace crates while
`Cargo.toml` says `0.1.2-dev`, so every `--locked` command fails from a fresh
clone.

## System Design

### Layers and dependency direction

The crate is a flat set of modules with a strict downward import rule. The graph
below is computed from non-test, non-comment `crate::` references; a module may
import anything in a lower layer and nothing in a higher one. Only `session`
and `session/run` import across the whole crate, and nothing imports `session`.

| Layer | Modules | Imports |
| --- | --- | --- |
| Foundation | `error`, `pyfmt`, `utils`, `logging`, `conversation`, `yaml`, `http`, `testing` (cfg(test)) | nothing above Foundation |
| Contract and assets | `assets`, `gameplan`, `harness`, `role`, `persona`, `skill/loader` | Foundation |
| Model I/O | `provider/*`, `toolschema`, `tooling`, `toolkit`, `jsonschema`, `usage` | Foundation |
| Agent and prompt | `agent`, `prompting`, `context`, `memory`, `compaction`, `agent_runtime`, `skill/runtime` | the two above |
| Boundary | `access`, `exec` | Foundation |
| Orchestration | `orchestrator`, `sessionfile`, `channel` | Contract, Foundation |
| Session | `session` (configuration, preparation), `session/run` (engine), `session/capabilities`, `session/outcome` | everything |
| Binding | `bindings/python/src/*.rs` | the crate; the crate has zero `pyo3` references |
| Package | `bindings/python/kerness/*.py` | `kerness._core` only |

Forbidden couplings, each checked by the graph above or a test:

- Nothing below `session` names `crate::session`; the two cross-layer edges
  that exist run upward through traits (`toolschema` and `usage` import
  `provider::ProviderResponse`, not the reverse).
- The crate never links Python. `crates/kerness/tests/public_api.rs:134`
  assembles a session from the public API alone.
- The Python package never implements behaviour. `bindings/python/tests/test_packaging.py:74`
  and `:83` hold every shim to a docstring, `_core` imports and an `__all__`.
- Bundled assets exist twice, byte-identical, and nothing in the build keeps them
  in step; `bindings/python/tests/test_packaging.py:42` is the only guard.

### State ownership

| State | Owner | Lifetime |
| --- | --- | --- |
| Configuration, roster, registrations | `Session` (`crates/kerness/src/session.rs:111` `SessionConfig`) | until `start` consumes it or `run` returns |
| Live run: scheduler, active turn, approvals, usage, terminal outcome | `SessionRun` (`crates/kerness/src/session/run.rs:264`) | owned value; dropped or finished |
| Resources `'static` callbacks need: access, memory, channels, context cache, skills | `Shared` (`crates/kerness/src/session.rs:338`) behind `Mutex`, taken through `lock` (`:373`) and released before any host callback (`store_for`, `:323`) | one run |
| Conversation turns and transcript | `Conversation` ([conversation.md](ARCHITECTURE/conversation.md)) | one run; checkpointed |
| Loop counters, phase, pending callbacks | `OrchestratorLoop` ([loop.md](ARCHITECTURE/loop.md)) | one run; checkpointed |
| One agent turn's private scratch and tool cursor | `AgentTurn` ([agent-runtime.md](ARCHITECTURE/agent-runtime.md)) | one turn; checkpointed mid-turn |
| Notes across runs | the installed `MemoryStore` ([memory.md](ARCHITECTURE/memory.md)) | outlives the run |
| Process-wide seams: transport, logger, console writer, console prompt, assets root | `OnceLock<RwLock<...>>` slots in `http.rs`, `logging.rs`, `channel.rs`, `access.rs`, `assets.rs` | process |

### Trust boundaries

- **Model output is untrusted.** Tool calls are parsed defensively and validated
  against a schema before dispatch ([toolkit.md](ARCHITECTURE/toolkit.md),
  [jsonschema.md](ARCHITECTURE/jsonschema.md)); a malformed or refused call is
  text returned to the model, never a raised error. Memory notes pass a caller
  `MemoryFilter` on the way in and are fenced with a caveat on the way out
  ([memory.md](ARCHITECTURE/memory.md), [prompting.md](ARCHITECTURE/prompting.md)).
  Orchestrator routing is a boundary scan for a registered name, and a role's
  position comes from a file's frontmatter, never from prose
  ([role.md](ARCHITECTURE/role.md)).
- **Every command and path goes through `AccessManager`**, default deny, with
  paths resolved before comparison so traversal and symlink escape are refused;
  an agent workspace can only narrow the session's
  ([access.md](ARCHITECTURE/access.md)). Tool identity is assigned by the engine
  and never read from model arguments (`crates/kerness/src/session/capabilities.rs:19`).
- **The host program is trusted.** Context sources, tool handlers, providers,
  channels and stores are the caller's code; the framework meters and scopes
  them but cannot undo their external effects ([run.md](ARCHITECTURE/run.md)).
- **Skill bundles are trusted only when the policy says so**
  (`trust_skill_bundles`, [skills.md](ARCHITECTURE/skills.md)).
- **Checkpoints are private files** written exclusively (`create_new`,
  owner-only permissions on Unix) and renamed into place
  ([sessionfile.md](ARCHITECTURE/sessionfile.md)); they carry prompts,
  transcripts and tool arguments, so storage and retention are the host's.

### Cross-cutting invariants

- Synchronous everywhere; cancellation is a cooperative `RunControl` flag
  (`crates/kerness/src/session/run.rs:45`) checked between steps and inside
  POSIX command polling.
- Anything that touches IO returns `crate::error::Result`; the enum is flat and
  the binding fans it into one exception class per variant
  ([errors.md](ARCHITECTURE/errors.md)).
- A step dispatches at most one provider operation, one tool invocation, one
  compaction, or one maintenance scope; usage is accounted per actor and
  operation and budgets are admitted before the operation
  ([run.md](ARCHITECTURE/run.md), [provider.md](ARCHITECTURE/provider.md)).
- Intent is persisted before a tool's side effect and completion after it;
  restored intent without completion waits for reconciliation and is never
  replayed ([sessionfile.md](ARCHITECTURE/sessionfile.md)).
- Frontmatter is YAML 1.1 read as events, so `no` is a boolean and `"no"` is a
  string ([harness.md](ARCHITECTURE/harness.md)).
- Values render the way CPython renders them on both sides of the boundary
  (`crates/kerness/src/pyfmt.rs`, [utils.md](ARCHITECTURE/utils.md)).

### Design decisions with evidence

- Supplied provider behaviour lives in free functions generic over
  `P: Provider + ?Sized` so a Python subclass override wins and an unoverridden
  method still gets the framework body ([provider.md](ARCHITECTURE/provider.md)).
- The four bundled channels and three bundled stores are crate types
  registered against Python ABCs, so `isinstance` holds without a pyclass
  inheriting from a Python class ([channel.md](ARCHITECTURE/channel.md),
  [memory.md](ARCHITECTURE/memory.md)).
- Tool narrowing is subtractive at four places and binds the dispatcher as well
  as the prompt; there is no lazy catalog ([toolkit.md](ARCHITECTURE/toolkit.md)).
- Two provider degrade latches never reset, because one conversation must not
  mix two payload shapes ([provider.md](ARCHITECTURE/provider.md)).

## Runtime and Data Flow

### Entry points

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

### Preparation and execution

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

### Data paths

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

### Configuration contracts

| Contract | Type | Where enforced |
| --- | --- | --- |
| Gameplan frontmatter | `HarnessSpec` (`crates/kerness/src/harness.rs:199`) | `parse_harness` (`:312`), `validate_harness` (`:353`) |
| Session options | `SessionConfig` (`crates/kerness/src/session.rs:111`) | `Session::new`, `prepare` (`:953`) |
| Run options | `RunOptions` (`crates/kerness/src/session/run.rs:70`): mode, approvals, budget, pricing, sink, result validation, `binding_version` | `Session::start` |
| Access policy | `AccessPolicy` (`crates/kerness/src/access.rs:186`; Python dataclass `bindings/python/kerness/access.py:22`) | `AccessManager` |
| Agent options | `Agent` (`crates/kerness/src/agent.rs:22`), inherited from `AgentDefaults` (`:313`) | `inherit` (`:274`) at preparation |
| Skill frontmatter | `SkillConfig` (`crates/kerness/src/skill/loader.rs:27`) | `load_skill` (`:78`) |
| Role and persona frontmatter | `RoleConfig` (`crates/kerness/src/role.rs:70`), `PersonaConfig` (`crates/kerness/src/persona.rs:17`) | their loaders |

### Failure and recovery

- Provider errors are retried with backoff (`DEFAULT_RETRIES`, `DEFAULT_BACKOFF_SEC`);
  a rejected native-tools or reasoning-effort body flips a one-way degrade latch
  and retries ([provider.md](ARCHITECTURE/provider.md)).
- A context-overflow refusal (`Error::is_context_overflow`) schedules one
  compaction to `OVERFLOW_RETRY_FRACTION` and retries once
  ([compaction.md](ARCHITECTURE/compaction.md)).
- A failing tool, an unknown tool, a schema violation and an unparseable call
  are all answered as text; `MAX_INVALID_CALLS` and `MAX_REPEATED_FAILURES`
  end the turn ([agent-runtime.md](ARCHITECTURE/agent-runtime.md)).
- A memory read failure mid-run yields an empty block and a logged warning;
  an append failure keeps the paid reply in committed history and returns the
  error ([memory.md](ARCHITECTURE/memory.md)).
- Strict results report `InvalidResult` diagnostics rather than coercing;
  budgets stop the next admitted operation with `BudgetExceeded`; a sink
  failure is terminal without replaying the completed action
  ([run.md](ARCHITECTURE/run.md)).
- A restored run validates counters, identities and the configuration contract
  before executing; a captured tool intent with no completion waits for
  `Reconcile` or cancellation ([sessionfile.md](ARCHITECTURE/sessionfile.md)).

### Concurrency and resource invariants

- One thread. The binding releases the GIL around `step`, contextual
  `run_command` and the HTTP transport (`bindings/python/src/run.rs:209`,
  `:166`, `provider.rs:768`) so another Python thread can call
  `RunControl.cancel()`; no framework thread exists.
- Process-wide slots are `RwLock`ed; the unit suite runs concurrently in one
  process, so a test that reads back its own double takes a static mutex
  (`crates/kerness/src/access.rs:1125`) and a test that only checks reachability
  asserts with `contains` (`crates/kerness/src/channel.rs:440`).
- Every command runs in its own process group with a deadline; output is drained
  nonblocking in 8 KiB passes; timeout kills the group, reaps the child and
  closes the pipes ([access.md](ARCHITECTURE/access.md)).
- Memory scopes open before the first turn and close exactly once on
  completion, failure, cancellation or drop; observed provider calls are refused
  during cleanup ([session.md](ARCHITECTURE/session.md)).
- Snapshot temporary files are created with `create_new`, synced, renamed, and
  removed on failure ([sessionfile.md](ARCHITECTURE/sessionfile.md)).

### Well-known constants

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
| `DEFAULT_MEMORY_BUDGET` | `2_200` | `crates/kerness/src/memory.rs:715` |
| `ENTRY_SEPARATOR` | `§` | `crates/kerness/src/memory.rs:724` |
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

## Workspace Map

One artifact per top-level directory: `crates/` is the crate, `bindings/` is
everything the wheel is made of. Neither reaches into the other's tests, and the
root carries one manifest — `Cargo.toml`. A Python build starts from
`bindings/python/`, not from here.

```
Cargo.toml                  workspace root, shared dependency versions, the one version number
Cargo.lock                  committed; `--locked` commands are a claim about it
crates/
  kerness/                  the framework — pure Rust, links no Python
    src/                    31 top-level modules plus provider/, skill/, and session/;
                            every file opens with a `//!` doc; unit tests inline
    assets/                 built-in gameplans, roles, personas, skills (edit with the copy below)
    tests/                  8 integration files + common/mod.rs doubles, over the public surface
    examples/               10 harnesses driven from Rust alone; support/mod.rs is shared by two
bindings/
  python/                   everything the wheel is built from
    pyproject.toml          the wheel's manifest and the ruff config
    Cargo.toml              the `kerness-py` crate, a workspace member, `publish = false`
    LICENSE  README.md      symlinks to the root copies (load-bearing, see below)
    src/                    13 PyO3 modules, one per boundary concern
    kerness/                the installed Python package
      __init__.py           bootstrap + public surface
      *.py                  per-subsystem re-export shims
      provider.py  channel.py  memory.py     the three subclassable ABCs
      access.py             the AccessPolicy dataclass
      exceptions.py         the exception hierarchy
      _enums.py             ToolDialect
      selfcheck.py          `python -m kerness.selfcheck`
      _core.abi3.so         BUILT ARTIFACT, gitignored; regenerate with `maturin develop`
      gameplans/ roles/ personas/ skills/   byte-identical copies of crates/kerness/assets/
    tests/                  26 pytest modules + conftest.py
    examples/               8 runnable scripts, walked by tests/test_examples.py
.github/workflows/          ci.yml (every push and PR); release.yml (wheels, sdist, clean sdist install)
.github/dependabot.yml      weekly cargo and github-actions bumps
assets/                     project marks only: logo.svg, logo-mark.svg
README.md                   the public introduction
ARCHITECTURE.md             this file; CLAUDE.md and AGENT.md are symlinks to it
ARCHITECTURE/               one file per subsystem
.venv/                      local virtualenv, gitignored; `.venv/bin/{python,maturin,ruff,pytest}`
target/  .ruff_cache/  .pytest_cache/  __pycache__/   generated, gitignored
```

Edit restrictions and regeneration:

- **Assets are declared twice.** Change `crates/kerness/assets/<kind>/<file>`
  and `bindings/python/kerness/<kind>/<file>` together;
  `bindings/python/tests/test_packaging.py:42` fails otherwise.
- **The two symlinks in `bindings/python/` are load-bearing.** `readme` and
  `license-files` in `pyproject.toml` resolve against that directory and
  reject a `..` path; without them the wheel builds and ships neither, with
  nothing on stderr to say so.
- **`_core.abi3.so`** is what `maturin develop` writes into the package
  directory. After any Rust change, rebuild before running the Python suite;
  `bindings/python/tests/test_packaging.py:35` catches a stale binary by version.
- **Nothing under `bindings/python/` other than the package reaches the wheel.**
  maturin packages only the directory matching `module-name`; the sdist is
  rooted at the workspace and carries `crates/` as well.
- **No generated source.** There are no build scripts, no codegen, and no
  `.pyi` stubs.

## Coding Style and Code Design

### Enforced

| Rule | Tool and configuration | Command |
| --- | --- | --- |
| Rust formatting | `rustfmt` defaults; no `rustfmt.toml` | `cargo fmt --all -- --check` |
| Rust lint | `clippy` on every target, warnings are errors | `cargo clippy --workspace --all-targets -- -D warnings` |
| Rust docs | `rustdoc` warnings are errors | `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps -p kerness` |
| MSRV | `rust-version = "1.88"` in `Cargo.toml` | `cargo +1.88.0 check --workspace --all-targets --locked` |
| Python lint | `ruff` with `E4, E7, E9, F`, `py310`; `E402` ignored in `kerness/__init__.py` because `bootstrap` must run first (`pyproject.toml`) | `.venv/bin/ruff check bindings/python` |
| Shim shape | every public module declares `__all__` and every name resolves (`bindings/python/tests/test_packaging.py:74`, `:83`); every module is in the self-check list (`bindings/python/tests/test_selfcheck.py:18`) | pytest |
| Constants | the well-known constants and request defaults agree across the boundary (`crates/kerness/tests/public_api.rs:43`, `:70`; `bindings/python/tests/test_provider.py`) | test suites |

### Observed conventions

**Rust.**

- Every `.rs` file opens with a `//!` module doc stating what the module owns
  and why it is shaped that way; public items carry `///` docs. Rationale lives
  in those comments, not in a changelog.
- Naming is `snake_case` functions, `CamelCase` types, `SCREAMING_SNAKE` constants;
  the binding's pyclasses are `Py<Name>` with `#[pyclass(name = "Name", module = "kerness._core")]`
  plus `frozen`, `get_all`, `set_all` or `subclass` as needed (`bindings/python/src/types.rs`).
- Errors: `crate::error::{Error, Result}` everywhere IO or a provider is
  touched. Lock poisoning is `expect("... lock poisoned")`; the only bare
  `.unwrap()` calls in non-test code are 13 state-machine invariants in
  `crates/kerness/src/session/run.rs`. `unreachable!` carries a sentence saying
  why.
- `#[allow]` is rare and local: `clippy::too_many_arguments` on PyO3
  constructors (`bindings/python/src/types.rs:685`, `runtime.rs:417`,
  `session.rs:60`, `provider.rs:174` and their siblings) and a commented
  `clippy::large_enum_variant` at `crates/kerness/src/session/run.rs:164`.
- `unsafe` appears only at `crates/kerness/src/exec.rs:159`, `:163`, `:200`
  and `:370`, for `libc::fcntl` and `libc::kill`, under `#[cfg(unix)]`.
- No `println!`/`eprintln!` outside the default sink in
  `crates/kerness/src/logging.rs`; diagnostics go through `logging::{debug,warning,error}`
  and console output through `ConsoleWriter`.
- Serde: checkpoint and continuation types carry
  `#[serde(deny_unknown_fields)]` (19 sites across `agent_runtime`,
  `orchestrator`, `session/*`, `usage`); tagged enums use
  `#[serde(tag = "...", rename_all = "snake_case")]`; `serde_json` is built with
  `preserve_order` so dict order survives the boundary.
- Process-wide seams follow one shape: `fn slot() -> &'static RwLock<...>` over
  a `OnceLock`, a `set_*` installer, and a default that works from Rust alone
  (`http.rs`, `logging.rs`, `channel.rs`, `access.rs`, `assets.rs`).
- Free functions generic over `P: Provider + ?Sized` carry supplied trait
  behaviour so a Python override wins ([provider.md](ARCHITECTURE/provider.md)).
- Tests are named as sentences in `snake_case`
  (`the_documented_constants_hold_their_documented_values`); unit tests sit in
  `#[cfg(test)] mod tests` inside all 31 modules and share
  `crates/kerness/src/testing.rs` for scratch directories; integration tests
  share the doubles in `crates/kerness/tests/common/mod.rs` and add no test
  dependency.

**Python.**

- A shim is a docstring, `from kerness._core import ...`, and `__all__`
  (`bindings/python/kerness/toolkit.py`). Anything more is a finding unless it
  is one of the five permitted kinds.
- Subclassable bases are `abc.ABC` with `@abstractmethod`; bundled crate types
  are registered against them with `ABC.register`
  (`bindings/python/kerness/channel.py:49`, `memory.py:137`).
- `AccessPolicy` is a `@dataclass` with list defaults
  (`bindings/python/kerness/access.py:21`); `ToolDialect` is an `enum.Enum`
  compared with `is`.
- Tests: pytest, `class Test<Behaviour>` grouping with `test_<sentence>`
  functions, shared `MockProvider` and `PurposeMockProvider` in
  `bindings/python/tests/conftest.py`, transport patched at
  `kerness.provider.http_post_json`.

### Unenforced or inconsistent

- No Python type checker runs; annotations are partial and untested.
- `auto_approve_prefixes` has no doc comment on either surface
  (`crates/kerness/src/access.rs:188`, `bindings/python/kerness/access.py:36`)
  while every sibling field does.
- Relative `:NN` references in module docs are a documentation convention only;
  nothing in CI checks them.

## Verification and Review Map

### Commands and expected evidence

Run from the repository root.

```sh
cargo fmt --all -- --check                            # pass = exit 0
cargo test --workspace -q                             # pass = 407 unit + 118 integration + 1 doctest (526)
cargo clippy --workspace --all-targets -- -D warnings # pass = exit 0
cargo build -p kerness --examples                     # pass = all 10 compile
cargo run -p kerness --example offline_debate         # pass = completes with no key, no network
cargo run -p kerness --example host_control           # pass = validated host result
cargo run -p kerness --example resume_approval        # pass = restored approval, each tool once
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps -p kerness   # pass = exit 0
cargo +1.88.0 check --workspace --all-targets --locked      # pass = exit 0 once Cargo.lock is regenerated (see Roadmap)
(cd bindings/python && ../../.venv/bin/maturin develop)     # pass = "Installed kerness-<workspace version>"
.venv/bin/python -m pytest bindings/python/tests -q   # pass = 502 passed
.venv/bin/python -m kerness.selfcheck                 # pass = "OK: all core checks passed", exit 0
.venv/bin/ruff check bindings/python                  # pass = "All checks passed!"
.venv/bin/python bindings/python/examples/host_control.py  # pass = validated result, exit 0
```

Observed on 2026-09-06 at commit `7c97dcb`: every command above passes except the `--locked` variants, which fail with `the lock file
needs to be updated` until the committed `Cargo.lock` is regenerated for
`0.1.2-dev`. Without `--locked`, `cargo +1.88.0 check --workspace --all-targets`
passes.

### CI

`.github/workflows/ci.yml` runs on every push to `main` and every pull request:
`rust` (fmt, clippy, test, build examples, `offline_debate`, rustdoc on
stable), `msrv` (`cargo check --workspace --all-targets --locked` on a pinned
toolchain), and `python` (3.10 and 3.13: `maturin develop --extras dev`,
pytest, selfcheck, ruff). The `msrv` job cannot pass while it pins
`dtolnay/rust-toolchain@1.120.0`; see [Roadmap](#roadmap).
`release.yml`'s `verify-sdist` job installs the source distribution into a
clean interpreter with no checkout and runs the self-check; it is the only
check on the `bindings/python/{LICENSE,README.md}` symlinks and on asset
packaging ([testing.md](ARCHITECTURE/testing.md)).

### Change to test map

| If you change | Run first | Then | Review constraints |
| --- | --- | --- | --- |
| Frontmatter keys or validation | `cargo test -p kerness harness yaml`; `--test harness_contract` | `pytest tests/test_harness.py` | every accepted key must be validated, rendered or enforced ([harness.md](ARCHITECTURE/harness.md)) |
| Access rules, paths, commands | `cargo test -p kerness access exec::`; `--test access_e2e` | `pytest tests/test_access.py` | direct tests at the boundary, default deny, narrowing only ([access.md](ARCHITECTURE/access.md)) |
| Provider payloads, retries, usage | `cargo test -p kerness --lib provider usage`; `--test tools_e2e` | `pytest tests/test_provider.py` | constants asserted on both sides; `chat` is one request ([provider.md](ARCHITECTURE/provider.md)) |
| Turn stepping, tool loop | `cargo test -p kerness --lib agent_runtime`; `--test tools_e2e` | `pytest tests/test_agent_runtime.py` | continuation is owned data; counters saturate ([agent-runtime.md](ARCHITECTURE/agent-runtime.md)) |
| Scheduling, phases, end reasons | `cargo test -p kerness --lib orchestrator` | `pytest tests/test_loop.py` | forward-only phases; no IO in the loop ([loop.md](ARCHITECTURE/loop.md)) |
| Run engine, approvals, budgets, outcomes | `cargo test -p kerness --test session_run --test resume --test compaction_e2e --test public_api`; the two control examples | `pytest tests/test_session.py`; `examples/host_control.py` | one operation per step; intent before effect ([run.md](ARCHITECTURE/run.md)) |
| Session file schema | `cargo test -p kerness --lib sessionfile`; `--test resume` | `pytest tests/test_sessionfile.py` | schema 2 written, valid v1 read; unknown fields rejected ([sessionfile.md](ARCHITECTURE/sessionfile.md)) |
| Memory stores or filter | `cargo test -p kerness --lib memory usage`; `--test session_run memory_maintenance` | `pytest tests/test_memory.py tests/test_session.py` | filter before store; scopes close once ([memory.md](ARCHITECTURE/memory.md)) |
| Prompt assembly, context, skills index | `cargo test -p kerness prompting context skill` | `pytest tests/test_prompting.py tests/test_skill_*.py` | fixed part order; caveat on memory only ([prompting.md](ARCHITECTURE/prompting.md)) |
| Tool specs, dialects, schemas | `cargo test -p kerness tooling toolkit toolschema jsonschema` | `pytest tests/test_tool*.py tests/test_jsonschema.py` | `Skill` reserved; wire shapes exact ([toolschema.md](ARCHITECTURE/toolschema.md)) |
| Channels, logging seams | `cargo test -p kerness channel` | `pytest tests/test_channel.py` | exact-type shortcut; parked exceptions ([channel.md](ARCHITECTURE/channel.md)) |
| Any `bindings/python/src/*.rs` | `cargo clippy --workspace --all-targets -- -D warnings`; `maturin develop` | full pytest, selfcheck, ruff | forwarding only; no policy in Python ([bindings.md](ARCHITECTURE/bindings.md)) |
| Any bundled asset | `cargo test -p kerness --test public_api` | `pytest tests/test_packaging.py tests/test_selfcheck.py` | edit both copies; stay framework-generic |
| A public constant or default | `cargo test -p kerness --test public_api` | `pytest tests/test_provider.py tests/test_packaging.py` | update the table above in the same change |
| Any public Rust API | `cargo build -p kerness --examples`; rustdoc | `pytest tests/test_examples.py` | re-exports in `lib.rs`; examples must still compile |

### Coverage gaps

- No test reaches the network; the four backends are proved down to the request
  they build ([provider.md](ARCHITECTURE/provider.md)).
- CI runs on Linux only; macOS and Windows wheels are built at release time
  without running the suites there, and Windows is not a declared platform.
- `PyStore::revise` is the one binding crossing no test drives
  ([memory.md](ARCHITECTURE/memory.md)).
- Nothing checks the assets pair from the Rust side; the guard needs the Python
  surface installed.
- Relative line references in these docs are not machine-checked.

## Roadmap

### Implementation milestones

The M1–M3 core upgrade is implemented in Rust and exposed through the binding.
Execution remains synchronous, with no hidden executor or concurrent agent
scheduling.

| Milestone | Status | Delivered behavior and evidence |
| --- | --- | --- |
| **M1 — Runtime ownership and tool capabilities** | done | Owned `SessionRun`, complete `ToolSpec` registration, contextual handlers with immutable identity and scoped capabilities. Registration, capability lifetime and resource lifecycle tests pass ([session.md](ARCHITECTURE/session.md), [run.md](ARCHITECTURE/run.md)). |
| **M2 — Host-driven execution** | done | Shared run/step engine, typed input/events/control, external approval, single-agent host mode, schema-2 continuation, v1 boundary migration and explicit reconciliation. Equivalence, suspended approval, denial, cancellation and interrupted-action tests pass ([run.md](ARCHITECTURE/run.md), [loop.md](ARCHITECTURE/loop.md), [sessionfile.md](ARCHITECTURE/sessionfile.md)). |
| **M3 — Outcomes and budgets** | done | Strict result diagnostics, typed terminal and turn reasons, retained committed history, normalized usage and operation/tool/time admission. Token/cost limits require explicit measured-threshold mode. Retry, compaction, maintenance and nested provider accounting tests pass ([provider.md](ARCHITECTURE/provider.md), [memory.md](ARCHITECTURE/memory.md)). |
| **M4 — Adapters and richer sessions** | deferred | Native streaming, workflow adapters, session-store listing/forking, typed content parts, MCP and sequential subagents each need a separately justified change. Parallel execution requires revising the synchronous invariant. |

M4 adapters must consume the M1–M3 contracts. Streaming needs a transport seam
for partial output and explicit retry semantics; richer content needs a schema
and context-accounting change. Workflow and MCP adapters reuse the existing
execution, access, approval and budget boundaries. Custom tools needing several
resumable effects require a continuation protocol; synchronous callbacks cannot
be unwound and replayed for approval. Practical limits of the current contracts
belong to [run.md](ARCHITECTURE/run.md).

### Repository defects (evidence-backed, not yet fixed)

| Defect | Evidence | Fix and success check |
| --- | --- | --- |
| The CI MSRV job installs a Rust version that does not exist | `.github/workflows/ci.yml:62` pins `dtolnay/rust-toolchain@1.120.0` (Dependabot bump `2e60af2`); the job named "Rust (MSRV 1.88)" fails at `rustup toolchain install 1.120.0` with a 404 | Pin `1.88.0` to match `rust-version`, and add a Dependabot `ignore` for that action so a semver bump cannot move the floor; success = the `msrv` job passes on `main` |
| The committed `Cargo.lock` predates the version bump | `Cargo.lock` records `kerness`/`kerness-py` at `0.1.1-dev` while `Cargo.toml` says `0.1.2-dev` (`9f6555e`); `cargo test --workspace --locked` and the MSRV job's `cargo check --locked` fail with "the lock file needs to be updated" | Run `cargo update -w` (or any cargo command) and commit the regenerated lock; success = `cargo +1.88.0 check --workspace --all-targets --locked` exits 0 |

### Improvement candidates (proposals, not accepted work)

Each is recorded in its owning module with benefit and success check:

- A Rust-side asset parity check so the duplicated `assets/` cannot drift when
  the Python surface is not installed ([testing.md](ARCHITECTURE/testing.md)).
- Doc comments for `auto_approve_prefixes`, the loosest command mechanism
  ([access.md](ARCHITECTURE/access.md)).
- `.pyi` stubs for `_core`, so editors see the binding's surface
  ([bindings.md](ARCHITECTURE/bindings.md)).
- A query parameter on `MemoryStore::read`, which is a trait contract change
  and therefore a decision rather than an omission ([memory.md](ARCHITECTURE/memory.md)).
- A summary tool catalog, justified only once a tool source the host did not
  enumerate exists ([toolkit.md](ARCHITECTURE/toolkit.md)).

## Development Loop

Coding Discipline governs writing; Review Checks govern review. This
loop connects them and defines when work is ready to release.

```text
Frame → Write → Prove → Review → Gate
          ▲          findings      │
          └────────────────────────┘
```

### The loop

**1. Frame.** Convert the request into a goal with an observable check.
Inspect the request, code, docs, and repository conventions; record the
narrowest supported assumptions. When using eatmycode, run its Version
and Freshness Gate before trusting architecture guidance. Ask one focused
question only when a required decision cannot be discovered or safely
inferred and guessing would materially change the result. Once framed,
continue without an approval pause.

**2. Write.** Make the smallest change that reaches the goal. Add no
unrequested features or abstractions, match local style, touch only
in-scope code, and remove only orphans created by the change.

**3. Prove.** Run relevant tests and retain observable evidence.

*Survey the suite before touching it.* Before adding, changing, merging,
or deleting any test, inventory the whole suite: enumerate every test
file and case name, then read in full each test whose subject, fixtures,
or assertions touch this change. Use a subagent for broad inventory when
supported. From that inventory decide the complete set of test edits at
once — what to change, what to add, what to merge, what to remove — each
backed by `file:line`, then execute only that plan. Never write a test
before the survey, and never discover existing coverage afterward.

The plan obeys four rules:

- **Reuse or extend first.** Add a case to the test that already owns
  the behavior or shares its setup, fixtures, and subject. A new test
  function or file is justified only when the survey found no existing
  test owning the behavior, or when merging would hide which case
  failed.
- **Add only what the goal needs.** A bug fix needs a reproducing
  regression test; a new capability needs a test of its claimed
  behavior. Nothing further.
- **Retire what this change made obsolete.** Delete tests whose behavior
  no longer exists, and merge tests this change turned into duplicates,
  citing the surviving test. Leave unrelated pre-existing tests alone;
  record suspected redundancy under **Open Gaps / Roadmap**.
- **Never delete to reach green.** A failing test is a finding for
  Write. Removal requires evidence that its behavior is gone or is still
  covered elsewhere, cited by `file:line`.

Coverage of claimed behavior must not decrease. A failure returns
directly to Write, never forward to Review.

**4. Review.** Walk all seven Review Checks as separate passes. Read
whole affected files, not only the diff. Every finding needs `file:line`
evidence. Use an independent agent or isolated pass for Fit,
Dependencies, and Security when available.

**5. Gate.** Apply the Definition of Done. Any unticked criterion,
`blocker`, or unresolved `major` returns its evidence to Write. All
criteria passing means the change is ready for public or production
release. There is no separate approval or reporting phase.

### Definition of Done

**Correctness**

- The framed goal and its named check pass.
- Tests cover claimed behavior and pass; a bug fix has a regression test.
- The suite was surveyed before any test was written, changed, or
  deleted; no added test duplicates coverage another test owns, and no
  removal left claimed behavior uncovered.
- The owning module's **How to Test** command passes with evidence.
- The project builds and tests from a fresh clone without local-only
  dependencies.

**Review**

- All seven Review Checks ran; none was skipped or assumed.
- No `blocker` or unresolved `major` remains.
- Nits were applied or consciously declined.

**Legibility and contract**

- An agent can locate the owning code, identify language/style/design
  constraints, select a safe change or refactor, review its impact, and
  run the right checks from `ARCHITECTURE.md` and the owning module docs.
- Every changed line serves the goal; no drive-by formatting, debugging
  remnants, commented-out code, secrets, tokens, or local paths remain.
- Public names, signatures, errors, and recovery are intelligible.
- Architecture docs and `file:line` references reflect current source;
  version stamps certify a verified contract migration, not just a
  metadata edit.
- Architecture docs contain only coding context; any encountered
  deployment guides or other non-coding material and obsolete links were
  removed from the doc set.
- Breaking changes, deprecations, dependencies, licenses, and attribution
  are handled; commit or PR text explains why.

### Iterating without thrashing

- Every pass closes a named finding and touches only what it names.
- Nits alone do not trigger another pass.
- Re-run Prove after every fix.
- Two no-change passes force Gate re-evaluation: release if Done passes;
  otherwise return the surviving evidence to Frame.
- Three passes on one finding return automatically to Frame for a new
  approach.
- Never widen scope to satisfy a finding. Record coding-related follow-up
  work under **Open Gaps / Roadmap**; keep non-coding work outside the
  architecture doc set.

## Coding Discipline

### 1. Think Before Coding

- Understand the request, code, goal, and repository conventions first.
- Record assumptions and choose the narrowest evidence-backed reading.
- Prefer the simpler approach when it reaches the same verified goal.
- Ask only during planning and only for a required answer that cannot be
  discovered or safely inferred.

### 2. Simplicity First

- Implement only what was requested.
- Do not add single-use abstractions, speculative flexibility, or checks
  for impossible conditions.
- If the implementation is materially larger than the problem, simplify
  it.

### 3. Surgical Changes

- Do not refactor, reformat, or clean up unrelated code.
- Match the surrounding style.
- Remove imports, variables, and functions made unused by this change;
  leave pre-existing dead code alone unless requested.
- Every changed line must trace to the stated goal.

### 4. Goal-Driven Execution

Turn work into verifiable outcomes, then loop until they pass:

- Add validation → invalid inputs are rejected by a named passing test.
- Fix a bug → a regression test fails before the fix and passes after.
- Refactor → behavior tests pass before and after.

Give every plan step its own check. Strengthen vague criteria from
repository evidence before implementation.

### Project-Specific Deviations

- **Dead configuration keys are defects.** Every field the harness parser
  accepts must be validated, rendered into a prompt, or enforced at runtime.
  There are no reserved harness keys held for later.
- **The docs describe the code as it is.** No changelog prose, no milestone
  labels in comments, no "used to" or "changed in": a comment or document that
  narrates history describes something a reader cannot run. Removals are
  outright, and what replaced them is documented on its own terms.
- **Bundled assets remain framework-generic.** Domain-specific gameplans,
  personas, and skills live with the project that owns their domain, not in
  `assets/`.
- **Inventory tests assert discovery, not literals.** Built-in assets and core
  modules are enumerated from disk so an addition or removal cannot silently
  escape the self-check.
- **Security boundaries receive direct tests.** Access rules, path traversal,
  symlink escape, skill bundle grants, and denied tool calls are tested at the
  layer that owns them, not only through a session that happens to exercise them.
- **Anything that touches IO returns `crate::error::Result`.** A function that
  reads a file, writes one, or calls a provider does not panic and does not
  return a bare value; the error type is the crate's own.


## Review Checks

Run every check against every change before merge. Keep checks separate.

Four rules bind all checks:

- **Evidence or no finding.** Every finding cites `file:line`.
- **The repository is authoritative.** Demand only conventions visible
  in the tree.
- **Read files, not only hunks.** Context can invalidate a finding or
  reveal unreachable code, unused parameters, and hidden duplication.
- **Review the change, never the author.** Describe code and impact, not
  how or by whom it was produced.

### 1. Style

Check indentation and local file conventions. Mixed indentation is
`major`; a consistent new file using the wrong local indent is `nit`.
Leave machine-checkable formatting to existing formatters and linters;
never demand unrelated reformatting.

### 2. Naming

Compare new names with nearby precedents before filing a finding. If the
repository is inconsistent, demand nothing. A local mismatch is `nit`;
an inconsistent public name is `major`.

### 3. Duplication

Search distinctive constants, errors, fields, and call sequences—not
only symbol names—for code performing the same job. Cite both sites and
the remedy. Cross-layer duplication is `major`; small local repetition
is `nit`. Similar code with meaningfully different branches is not
duplication.

### 4. Quality

Require followable control flow, errors handled where they occur, and
abstractions proportional to the problem. Swallowed errors,
inappropriate prints, unexplained magic values, and dead branches are
`major`. Remove unrequested configurability, one-caller wrappers, filler
comments, debugging remnants, and unrelated formatting. Missing tests
belong to Prove, not this check.

### 5. Fit

Read `ARCHITECTURE.md` and the owning module doc before the diff. Check
documented language/toolchain constraints, code-design conventions,
scope, layering, ownership, invariants, public-API growth, compatibility,
and performance claims against source evidence. A layering violation or
unjustified public API is `major`. Architectural or public-behavior changes
must update the relevant docs in the same change.

### 6. Dependencies

Check manifests and imports, maintenance, supply-chain risk, advisories,
install-time behavior, license, transitive cost, and whether the standard
library is sufficient. An unjustified top-level dependency is `major`;
a live advisory or abandoned upstream is `blocker`. Incomplete evidence
does not pass.

### 7. Security

Check both defects and widened exposure: unsafe memory access, unchecked
sizes or offsets, integer overflow, path traversal, unsafe
deserialization, command construction, committed secrets, and unbounded
untrusted input. Trace input to impact; without a reachable path there is
no finding. A real defect is `major`; a trust-boundary break is `blocker`.
Describe the fix without publishing exploit steps.

### Severity and the merge threshold

| Severity | Effect |
| -------- | ------ |
| `blocker` | Must not merge. |
| `major` | Must be resolved before merge. |
| `nit` | Apply or consciously decline. |
| `info` | Context or a question; no action implied. |

Merge only with no `blocker` and no unresolved `major`. A check that did
not run does not pass. Findings feed Write and Gate directly; they do not
create a reporting phase.


## Index

Reading path: run the freshness gate, read the cross-cutting rules above, pick
the owners below from the paths or triggers your change touches, read those
module docs and the partners they name, then inspect the cited code and tests.

| Module doc | Owns (source paths) | Responsibility | Read it when you change | Partners |
| --- | --- | --- | --- | --- |
| [access.md](ARCHITECTURE/access.md) | `crates/kerness/src/access.rs`, `crates/kerness/src/exec.rs`, `bindings/python/src/access.rs`, `bindings/python/kerness/access.py` | the permission boundary: commands, paths, hosts, workspaces, the console prompt seam, command deadlines and process groups | any allow-list, path resolution, approval flow, `run_command`, a new tool that touches the filesystem or network | toolkit, skills, session, run |
| [agent.md](ARCHITECTURE/agent.md) | `crates/kerness/src/agent.rs`, `PyAgent` in `bindings/python/src/types.rs` | the agent record, option inheritance, system prompt and message assembly | a new per-agent option, inheritance rules, what a provider is sent | role, persona, prompting, provider, session |
| [agent-runtime.md](ARCHITECTURE/agent-runtime.md) | `crates/kerness/src/agent_runtime.rs`, `PyAgentRunner` in `bindings/python/src/runtime.rs` | one resumable agent turn: provider call, pending tool calls, results, guards, typed turn reason | tool-loop bounds, turn continuation, overflow retry behaviour | provider, toolkit, toolschema, sessionfile, run |
| [bindings.md](ARCHITECTURE/bindings.md) | `bindings/python/src/*.rs`, `bindings/python/kerness/*.py`, `bindings/python/pyproject.toml` | the PyO3 boundary, value and error conversion, callback binding, the installed package and its shims | anything crossing to Python, a new pyclass, a new shim, the wheel manifest | errors, provider, channel, memory, run, selfcheck |
| [channel.md](ARCHITECTURE/channel.md) | `crates/kerness/src/channel.rs`, `crates/kerness/src/logging.rs`, `bindings/python/src/channel.rs`, `bindings/python/kerness/channel.py` | message delivery, the four bundled channels, the console-writer and logger seams, parked Python exceptions | output destinations, diagnostics, a caller's channel subclass | session, bindings, access |
| [compaction.md](ARCHITECTURE/compaction.md) | `crates/kerness/src/compaction.rs`; `fit_conversation` in `crates/kerness/src/session.rs` | token estimate, the context ceiling per agent, the summarize-the-prefix rewrite and the one overflow retry | context limits, summary prompts, the estimate | conversation, provider, run, errors |
| [context.md](ARCHITECTURE/context.md) | `crates/kerness/src/context.rs`; `add_context`/`resolve_context` in `crates/kerness/src/session.rs` | host-supplied standing facts rendered once per agent | a new context source, the `context:` key, caching | harness, prompting, session |
| [conversation.md](ARCHITECTURE/conversation.md) | `crates/kerness/src/conversation.rs`, `PyConversation` in `bindings/python/src/runtime.rs` | turns, transcript, and the rendered `ChatMessage` list | message shapes, what a provider sees, restore | compaction, sessionfile, agent-runtime |
| [errors.md](ARCHITECTURE/errors.md) | `crates/kerness/src/error.rs`, `bindings/python/src/errors.rs`, `bindings/python/kerness/exceptions.py` | the error enum, the exception hierarchy, and the total map between them | a new failure kind, overflow detection, exception attributes | every module; bindings |
| [gameplan.md](ARCHITECTURE/gameplan.md) | `crates/kerness/src/gameplan.rs`, `crates/kerness/src/assets.rs`, `crates/kerness/assets/gameplans/` | loading a Markdown gameplan, splitting frontmatter from body, the assets root and enumeration | a bundled gameplan, asset resolution, `set_root` | harness, persona, selfcheck |
| [harness.md](ARCHITECTURE/harness.md) | `crates/kerness/src/harness.rs`, `crates/kerness/src/yaml.rs`, spec pyclasses in `bindings/python/src/types.rs` | the frontmatter contract: parse, validate against registrations, narrow tools and context, widen skills | any frontmatter key, validation message, YAML scalar rule | gameplan, session, loop, toolkit, skills, context |
| [jsonschema.md](ARCHITECTURE/jsonschema.md) | `crates/kerness/src/jsonschema.rs` | strict-mode schema rewriting and argument validation messages | tool parameter schemas, structured output, validation wording | toolschema, toolkit, provider |
| [loop.md](ARCHITECTURE/loop.md) | `crates/kerness/src/orchestrator.rs`, loop adapters in `bindings/python/src/runtime.rs` | the orchestrator action machine: routing, phases, retries, closing verdict, end reasons | phase rules, turn limits, host-driven scheduling, loop snapshot | harness, agent-runtime, sessionfile, utils, run |
| [memory.md](ARCHITECTURE/memory.md) | `crates/kerness/src/memory.rs`, `Memories`/`remember` in `crates/kerness/src/session.rs`, `bindings/python/src/memory.rs`, `bindings/python/kerness/memory.py` | the store trait, three bundled stores, the filter, scopes, metered maintenance | a store, the filter, memory tools, maintenance or cleanup | prompting, access, toolkit, run, provider |
| [persona.md](ARCHITECTURE/persona.md) | `crates/kerness/src/persona.rs`, `crates/kerness/assets/personas/` | loading a persona and rendering it for a prompt | persona search paths or rendering | agent, gameplan, selfcheck |
| [prompting.md](ARCHITECTURE/prompting.md) | `crates/kerness/src/prompting.rs`, `PyPromptAssembler` in `bindings/python/src/runtime.rs` | system-prompt assembly, part order, memory and context framing | any prompt block, caveat text, part order, dialect branch | agent, memory, context, skills, toolschema |
| [provider.md](ARCHITECTURE/provider.md) | `crates/kerness/src/provider/`, `crates/kerness/src/http.rs`, `crates/kerness/src/usage.rs`, `bindings/python/src/provider.rs`, `bindings/python/kerness/provider.py` | the `Provider` trait, four backends, retries and degrade latches, the transport seam, usage accounting and budgets | a backend, a request field, retry policy, metering, pricing | agent-runtime, toolschema, jsonschema, compaction, run |
| [role.md](ARCHITECTURE/role.md) | `crates/kerness/src/role.rs`, `crates/kerness/assets/roles/` | the two positions, role files, three-way spec resolution | a role file, the orchestrator prompt, position rules | agent, session, prompting |
| [run.md](ARCHITECTURE/run.md) | `crates/kerness/src/session/run.rs`, `crates/kerness/src/session/capabilities.rs`, `crates/kerness/src/session/outcome.rs`, `bindings/python/src/run.rs` | owned execution: step engine, inputs, events, approvals, scoped tool capabilities, budgets, outcomes, checkpoints and recovery | any step transition, approval, budget, event, result validation, resume behaviour | session, loop, agent-runtime, access, sessionfile, provider |
| [selfcheck.md](ARCHITECTURE/selfcheck.md) | `bindings/python/kerness/selfcheck.py` | `python -m kerness.selfcheck`, the installed-package health check | a new package module or asset kind | gameplan, role, persona, skills, bindings |
| [session.md](ARCHITECTURE/session.md) | `crates/kerness/src/session.rs`, `bindings/python/src/session.rs`, `bindings/python/kerness/session.py` | configuration, registration, preparation, shared resources, the blocking `run` adapter | a session option, registration rule, preparation order, resource lifecycle | run, harness, agent, access, memory, prompting |
| [sessionfile.md](ARCHITECTURE/sessionfile.md) | `crates/kerness/src/sessionfile.rs`, snapshot functions in `bindings/python/src/funcs.rs` | the snapshot schema, identity check, atomic save and validated load | the schema, a new checkpointed field, identity rules, file safety | run, loop, conversation, agent-runtime |
| [skills.md](ARCHITECTURE/skills.md) | `crates/kerness/src/skill/`, `crates/kerness/assets/skills/`, `bindings/python/src/skill.rs` | skill bundles, the `Skill` tool, the tool gate and required tools, bundle grants | a bundled skill, `allowed-tools`/`requires-tools`, activation | harness, toolkit, access, prompting |
| [testing.md](ARCHITECTURE/testing.md) | `crates/kerness/tests/`, `crates/kerness/src/testing.rs`, `bindings/python/tests/`, `crates/kerness/examples/`, `bindings/python/examples/`, `.github/workflows/` | the three suites, their doubles, the examples, CI, and the whole-workspace gate | adding or moving a test, a double, an example, a CI step | every module |
| [toolkit.md](ARCHITECTURE/toolkit.md) | `crates/kerness/src/tooling.rs`, `crates/kerness/src/toolkit.rs`, tool pyclasses in `bindings/python/src/types.rs` | tool specs, text-fence call parsing, the tools prompt, dispatch and result shaping, allow-list narrowing | a tool, the call parser, dispatch errors, narrowing order | jsonschema, toolschema, access, skills, agent-runtime |
| [toolschema.md](ARCHITECTURE/toolschema.md) | `crates/kerness/src/toolschema.rs`, `register_dialect` in `bindings/python/src/types.rs`, `bindings/python/kerness/_enums.py` | native tool dialects and their exact wire shapes | a dialect, a provider's tool format, result rendering | provider, toolkit, jsonschema, prompting |
| [utils.md](ARCHITECTURE/utils.md) | `crates/kerness/src/utils.rs`, `crates/kerness/src/pyfmt.rs` | keyword and mention scanning, memory markers, retry, CPython-compatible formatting | a terminator, routing syntax, retry backoff, value rendering | loop, memory, provider, toolkit |
