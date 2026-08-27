<p align="center">
  <img src="assets/logo.svg" alt="Kerness — Kernel for Harness" width="560">
</p>

<p align="center">
  <strong>Kerness — Kernel for Harness.</strong><br>
  The framework an AI harness sits on, assembled from plug-and-play components.
</p>

<p align="center">
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-F59E0B"></a>
  <img alt="Rust 1.80+" src="https://img.shields.io/badge/rust-1.80%2B-B7410E">
  <img alt="Python 3.10+" src="https://img.shields.io/badge/python-3.10%2B-3776AB">
</p>

---

## What Kerness is

A **harness** is everything wrapped around a language model that turns it into a
working system: who speaks and in what order, which tools are reachable, what
counts as finished, what gets remembered, and what the run finally returns. A
debate between three agents is a harness. A research pipeline is a harness. A
code-review bot, a negotiation simulator, a poker table with three seats — all
harnesses.

Building one from scratch means writing the same substrate every time. Provider
transport and retries. Tool-call parsing across three incompatible dialects.
Prompt assembly. A turn loop with phases and termination conditions. Access
control on anything that touches the filesystem. Memory. Context compaction.
Crash-resumable state. That substrate is where the weeks go, and none of it is
the harness you actually wanted to build.

**Kerness is that substrate — the kernel.** It owns every piece listed above and
exposes them as components you plug together. What is left for you is the part
that is genuinely yours: a Markdown file declaring how your harness behaves, and
whatever tools you want to hand the agents.

The name is the design: a **kern**el for a har**ness**.

### The split

| The kernel owns | Your harness declares |
| --- | --- |
| Provider transport, retries, backoff | Which models each agent uses |
| Tool dialects (OpenAI / Anthropic / text-fence) | Which tools exist and what they do |
| Prompt assembly and ordering | Personas, system prompts, language |
| The orchestrator loop, phases, turn counting | Phase names, round counts, instructions |
| Termination detection | Which tokens end a run |
| Access decisions on commands and paths | The policy those decisions are made against |
| Memory read/write, context compaction | Whether the run may write memory |
| Session files and resume | Where the state file lives |
| Skill discovery and progressive disclosure | Which skills an agent may load |

Nothing in the left column is something you should have to write again. Nothing
in the right column is something the framework should decide for you.

## Plug-and-play components

Every component is an interface with a working default. Use the default, or
swap in your own — the rest of the kernel does not notice.

| Component | Ships with | Swap it by |
| --- | --- | --- |
| **Provider** | `OpenAIProvider`, `ClaudeProvider`, `OpenRouterProvider`, `CustomProvider`, plus OAuth variants | subclassing `Provider` and overriding `chat` |
| **Channel** | `ConsoleChannel`, `FileChannel`, `LogChannel`, `MultiChannel` | subclassing `Channel` — one method, `send` |
| **Tools** | `run_command`, `read_file`, `list_dir`, `write_memory` | `session.add_tool(name, description, parameters, handler)` |
| **Skills** | `challenge`, `fact-check`, `summarize`, `agent-browser` | dropping a `SKILL.md` directory on disk |
| **Personas** | `pragmatic_engineer`, `devils_advocate` | a `.md` file, or inline prose |
| **Gameplans** | `debate`, `discussion`, `research` | a new Markdown file — see below |
| **Access** | closed by default; allow-lists for programs, patterns, directories | `AccessPolicy(...)`, plus an approval callback |
| **Memory** | a plain `.md` file, read-only unless asked | point `memory=` anywhere; per-agent scopes supported |
| **Session file** | JSON snapshot after every turn | `session_file=` — absent means persist nothing |

A `CustomProvider` pointed at any OpenAI-compatible endpoint covers most local
inference servers without writing a class at all.

Providers may speak native tool calling or fall back to text fences. The dialect
is resolved **per agent**, so one session can mix an Anthropic model, an OpenAI
model, and a local endpoint that supports no tool calling at all, and the
orchestrator sees one normalized stream of calls.

## A harness is a Markdown file

The YAML frontmatter is a machine-readable contract the framework validates and
enforces. The body below it is the orchestrator's manual, in prose.

```yaml
---
name: debate
description: Adversarial debate, then a revisit against a neutral summary.
agents:
  orchestrator: { required: true }
  participants: { min: 2, max: 6 }
loop:
  max_turns: 50
  max_rounds: 3
  terminate_on: [END_SESSION, CONSENSUS_REACHED]
  phases:
    - name: think
      rounds: 1
      instruction: Give your own independent opinion. Do not rebut anyone yet.
    - name: argue
      rounds: 2
      instruction: Choose a side and present a forceful argument for it.
    - name: rethink
      rounds: 1
      rethink: true
      instruction: Re-examine your opening position against the summary.
result:
  consensus: { type: bool, description: Whether participants converged. }
  summary:  { type: str,  description: The final neutral summary. }
---

# Debate

You are running an adversarial debate. Your job is to make the disagreement
productive, not to resolve it prematurely.
```

Everything in that contract is enforced, not advisory: a session with one
participant is refused before the first API call, an unknown key is an error
rather than a silent no-op, and every problem in the file is reported at once
instead of one per run. The declared `result:` fields come back as typed values
on the session result.

A new harness is a new Markdown file. It is not a new runtime, not a subclass,
and not a fork.

## Install

```sh
pip install kerness                  # runtime
pip install 'kerness[structured]'    # plus pydantic, for OpenAIProvider(output_type=...)
```

From a checkout:

```sh
maturin develop --release
python -m kerness.selfcheck          # pass = "OK: all core checks passed"
```

Wheels are `abi3` from CPython 3.10 up, so one build covers every supported
interpreter.

## A run, in Python

```python
from kerness import ConsoleChannel, OpenAIProvider, Session

session = Session(
    gameplan="debate",
    topic="Should the cache be write-through?",
    provider=OpenAIProvider(api_key="..."),
    channel=ConsoleChannel(),
    session_file="run.json",     # resumable; omit to persist nothing
)
session.add_participant("Alice", model="gpt-4o", persona="pragmatic_engineer.md")
session.add_participant("Bob", model="gpt-4o", persona="devils_advocate.md")
session.add_orchestrator("Mod", model="gpt-4o")

result = session.run()
print(result.summary)        # the gameplan's declared `summary` field
print(result.consensus_reached, result.rounds_run, result.end_reason)
```

Handing the agents a tool of your own is one call:

```python
session.add_tool(
    "lookup_price",
    "Look up the current price of a ticker.",
    {"type": "object",
     "properties": {"ticker": {"type": "string"}},
     "required": ["ticker"]},
    lambda args: str(prices[args["ticker"]]),
)
```

The handler is a plain callable. The kernel handles schema translation into
whichever dialect the agent's provider speaks, parsing the call back out,
feeding the result in, and stopping a model that loops on malformed calls.

Skills use progressive disclosure: prompts carry only names and descriptions,
and the full instructions load on demand through a turn-local `Skill` tool. A
skill may also narrow the tools available for that turn, and — when the policy
trusts bundles — grant read access to its own directory.

There is no daemon and no server. A run given a `session_file` writes its state
to disk after every turn and continues from that file the next time the same
script runs. Resume checks identity first: a snapshot from a different gameplan
or a different agent roster is refused, not half-applied.

## A run, in Rust

The core crate links no Python at all. The same session, with no interpreter in
the process:

```rust
let mut session = Session::new(SessionConfig {
    gameplan: "debate".to_string(),
    topic: "Should the cache be write-through?".to_string(),
    provider: Some(provider),
    channel: Some(Arc::new(ConsoleChannel::default())),
    ..Default::default()
})?;

session.add_participant(Agent::new("Alice", "gpt-4o"));
session.add_participant(Agent::new("Bob", "gpt-4o"));
session.add_orchestrator(Agent { role: Role::Orchestrator, ..Agent::new("Mod", "gpt-4o") })?;

let result = session.run()?;
```

Full file: [`crates/kerness/examples/debate.rs`](crates/kerness/examples/debate.rs) —
`cargo run -p kerness --example debate`.

## Layout

```text
crates/kerness/      # the kernel, pure Rust — no PyO3, no Python
crates/kerness-py/   # the PyO3 extension module, kerness._core
python/kerness/      # the Python package: shims, deliberate Python, assets
tests/               # pytest suite
examples/            # runnable integrations
ARCHITECTURE/        # one document per subsystem
```

The bundled `debate`, `discussion`, and `research` gameplans are worked examples
of the contract, not the product.

## Testing

```sh
cargo test --workspace                     # 305 tests
cargo clippy --workspace --all-targets -- -D warnings
maturin develop
python -m pytest tests/ -q                 # 394 tests
python -m kerness.selfcheck                # exit 0
```

## Documentation

[`ARCHITECTURE.md`](ARCHITECTURE.md) is the entry point: mission, workspace
layout, boot flow, well-known constants, and an index of one document per
subsystem under [`ARCHITECTURE/`](ARCHITECTURE/). Each carries live `file:line`
references and the commands that prove it works.

## License

MIT. See [LICENSE](LICENSE).
