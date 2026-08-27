# Session

## Goal

The top-level object. A `Session` loads the gameplan, validates the harness
against what was registered, builds the access manager, the skill registry, the
tool registry, the memory store, and the conversation, then runs — either the
round-robin participant loop or the orchestrator loop — and returns a
`SessionResult`. Serves **M2**; it is the module every other one is reached
through.

It is also the largest module in the crate, and deliberately so: assembling all
of that in one place is what keeps every other module free of knowledge about
the rest.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/session.rs` | `SessionConfig`, `Session`, `Memories`, `SessionResult`, the `LoopHost` impl |
| `crates/kerness-py/src/session.rs` | `PySession`, `PySessionResult`, the parked-channel re-raise |
| `python/kerness/session.py` | re-export shim (`Message`, `Session`, `SessionResult`) |

## Key Types and Entry Points

- `crates/kerness/src/session.rs:97` — `SessionConfig` — everything a session
  needs, as data; `Default` at `:147`.
- `crates/kerness/src/session.rs:311` — `Session` — the assembled run.
- `crates/kerness/src/session.rs:345` — `new(config)` — where the gameplan is
  loaded and the harness validated, so an impossible contract fails before any
  provider call.
- `crates/kerness/src/session.rs:566` — `run()` — the whole harness; returns
  `SessionResult`.
- `crates/kerness/src/session.rs:1208` — `impl LoopHost for Session` — how the
  orchestrator loop drives it.
- `crates/kerness/src/session.rs:463` — `add_participant(agent)` / `:475`
  `add_orchestrator` / `:493` `add_skill` / `:506` `add_tool` — the registration
  chain; each returns `&mut Self` so registration composes.
- `crates/kerness/src/session.rs:176` — `Memories` — the per-agent memory store,
  shared as `Arc<Mutex<_>>` so a `Memory` handle stays live after the run.
- `crates/kerness/src/session.rs:62` — `SessionResult` — topic, turns, consensus,
  history, summary, parsed fields, rounds, phase reached, end reason.
- `crates/kerness/src/session.rs:534` — `run_command` / `:552` `read_file` / `:557`
  `list_dir` — the built-in tools, each going through the access manager.
- `crates/kerness/src/session.rs:54` — `DEFAULT_MAX_CONTEXT_TOKENS` — the
  compaction ceiling. Not exported from the Python package: `session.__all__` is
  exactly `Message`, `Session`, `SessionResult`.

`Session::new` sets `trust_skill_bundles` explicitly (`session.rs:348`) because
`AccessPolicy`'s derived `Default` and `AccessPolicy::new()` disagree on it; the
reason is recorded at `access.rs:86`.

On the Python side, `PySession` keeps the bound channel so that an exception a
delivery raised can be re-raised out of `run()` instead of arriving as the
framework error it had to be reduced to — see [channel.md](channel.md).

## Interactions

Session is the assembly point, so it touches nearly everything:

- Loads [gameplan.md](gameplan.md) and validates through [harness.md](harness.md).
- Builds [access.md](access.md)'s manager and [skills.md](skills.md)'s registry.
- Owns [conversation.md](conversation.md) and [memory.md](memory.md).
- Registers tools with [toolkit.md](toolkit.md).
- Runs turns through [agent-runtime.md](agent-runtime.md), and is the `LoopHost`
  for [loop.md](loop.md).
- Compacts through [compaction.md](compaction.md), saves through
  [sessionfile.md](sessionfile.md), delivers through [channel.md](channel.md).

## How to Test

```sh
cargo test -p kerness session                          # pass = 0 failed
.venv/bin/python -m pytest tests/test_session.py -q    # pass = 0 failed
.venv/bin/python -m pytest tests/test_examples.py -q   # pass = 0 failed
```

- The Rust tests drive a `SequenceProvider` (`session.rs:1521`) through a fixed
  reply sequence with a `CaptureChannel` (`:1594`) recording what was delivered.
- `tests/test_session.py` is the suite's integration layer: 90 cases covering
  access refusals mid-run (`:366`), per-agent memory (`:1522`), resume across two
  runs (`:2099`), and compaction of a long run (`:2174`).
- `tests/test_examples.py:132` — `test_every_name_it_reaches_for_still_exists` —
  parses each example's AST and resolves every imported name. It does not run
  them; it proves the public surface an example depends on has not moved.

## Open Gaps / Roadmap

- `session.rs` is 2,498 lines. `run()` in particular carries both the
  participant loop and the orchestrator handoff; splitting it would need a home
  for the shared setup that is not another module knowing about sessions.
- One access manager and one skill registry per session, shared by every agent.
  Per-agent policies would be a `SessionConfig` change, not a structural one.
- Resume restores the conversation and the loop state but not tool side effects;
  a resumed session re-runs nothing it already did and assumes the world moved on.
