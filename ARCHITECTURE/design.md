---
eatmycode_version: "1.2.0"
---

# System design detail

Supporting page for [ARCHITECTURE.md](../ARCHITECTURE.md#system-design): the
subsystem dependencies, who owns which state and
for how long, the four process-wide seams, and the decisions the root cites.

## Layers and dependency direction

These groups locate responsibilities; they are not an enforced acyclic layer
graph. `session` composes the runtime; lower-level modules do not import
`crate::session`. Providers, tool schemas and usage accounting have peer edges
(`crates/kerness/src/provider/mod.rs:21`, `crates/kerness/src/toolschema.rs:23`,
`crates/kerness/src/usage.rs:17`). Check imports when moving a responsibility;
there is no architecture linter.

| Group | Modules | Dependencies and boundaries |
| --- | --- | --- |
| Foundation | `error`, `pyfmt`, `utils`, `logging`, `conversation`, `yaml`, `http`, `testing` (`cfg(test)`) | Small shared primitives; HTTP reports crate errors and Python-style body rendering. |
| Contract and assets | `assets`, `gameplan`, `harness`, `role`, `persona`, `skill/loader` | Loaders share asset resolution, YAML, validation and rendering; gameplan resolves the harness contract. |
| Model I/O | `provider/*`, `toolschema`, `tooling`, `toolkit`, `jsonschema`, `usage` | Providers consume tool schemas and usage tracking; schemas/usage refer to provider responses; dispatch validates arguments. |
| Agent and prompt | `agent`, `prompting`, `context`, `memory`, `compaction`, `agent_runtime`, `skill/runtime` | Compose contracts, provider/tool I/O and peer types; skill activation also uses access checks. |
| Access boundary | `access`, `exec` | `exec` checks `AccessManager` before spawning; both use foundation utilities. |
| Orchestration and delivery | `orchestrator`, `sessionfile`, `channel` | Scheduler consumes harness types; persistence stores serialized state; channels use logging and rendering. |
| Session | `session`, `session/run`, `session/capabilities`, `session/outcome` | Own preparation, step execution and integration of the above subsystems. |
| Binding | `bindings/python/src/*.rs` | Depends on the Rust crate and PyO3; the crate has no Python dependency. |
| Package | `bindings/python/kerness/*.py` | Imports `_core`, sibling modules and interpreter support; runtime policy stays in Rust. |

## State ownership

| State | Owner | Lifetime |
| --- | --- | --- |
| Configuration, roster, registrations | `Session` (`crates/kerness/src/session.rs:111` `SessionConfig`) | until `start` consumes it or `run` returns |
| Live run: scheduler, active turn, approvals, usage, terminal outcome | `SessionRun` (`crates/kerness/src/session/run.rs:264`) | owned value; dropped or finished |
| Resources `'static` callbacks need: access, memory, channels, context cache, skills | `Arc<Shared>` (`crates/kerness/src/session.rs:338`) with per-resource mutexes; callback handles are cloned out before host code runs (`store_for`, `:323`; `lock`, `:373`) | one run |
| Conversation turns and transcript | `Conversation` ([conversation.md](conversation.md)) | one run; checkpointed |
| Loop counters, phase, pending callbacks | `OrchestratorLoop` ([loop.md](loop.md)) | one run; checkpointed |
| One agent turn's private scratch and tool cursor | `AgentTurn` ([agent-runtime.md](agent-runtime.md)) | one turn; checkpointed mid-turn |
| Notes across runs | the installed `MemoryStore` ([memory.md](memory.md)) | outlives the run |
| Process-wide seams: transport, logger, console writer, console prompt, assets root | `OnceLock<RwLock<...>>` slots in `http.rs`, `logging.rs`, `channel.rs`, `access.rs`, `assets.rs` | process |

## Process-wide seams

Where a feature needs something only the interpreter has — `sys.stdout`, a
logger, `input`, an HTTP client under a caller's `mock.patch` — the crate names
the need as a trait, ships a default that works from Rust alone, and the
binding installs a replacement at `bootstrap` (`bindings/python/src/lib.rs:36`).
The behaviour stays in one place and only its delivery crosses. Every seam
follows one shape: a private `*slot()` accessor returning `&'static RwLock<...>`
over a `OnceLock`, a `set_*` installer, and the Rust default; `assets.rs`
uses the same shape for the assets root.

| Seam | Crate default | What the binding installs |
| --- | --- | --- |
| `HttpTransport` (`crates/kerness/src/http.rs:24`) | `ureq` + `rustls` | `kerness.provider.http_post_json`, resolved per call so `mock.patch` reaches it |
| `Logger` (`crates/kerness/src/logging.rs:28`) | warnings and errors to stderr | `logging.getLogger("kerness")`, so `caplog` sees them |
| `ConsoleWriter` (`crates/kerness/src/channel.rs:54`) | this process's stdout | `builtins.print`, so `capsys` and a `StringIO` see it |
| `ConsolePrompt` (`crates/kerness/src/access.rs:69`) | `std::io::stdin` | `sys.stdin` / `builtins.input` |

## Design decisions with evidence

- Supplied provider behaviour lives in free functions generic over
  `P: Provider + ?Sized` so a Python subclass override wins and an unoverridden
  method still gets the framework body ([provider.md](provider.md)).
- The four bundled channels and three bundled stores are crate types
  registered against Python ABCs, so `isinstance` holds without a pyclass
  inheriting from a Python class ([channel.md](channel.md),
  [memory.md](memory.md)).
- Tool narrowing is subtractive at four places and binds the dispatcher as well
  as the prompt; there is no lazy catalog ([toolkit.md](toolkit.md)).
- Two provider degrade latches never reset, because one conversation must not
  mix two payload shapes ([provider.md](provider.md)).
