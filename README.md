<p align="center">
  <img src="https://raw.githubusercontent.com/xwings/kerness/main/assets/logo.svg" alt="Kerness — Kernel for Harness" width="560">
</p>

<p align="center">
  <strong>Kerness — Kernel for Harness.</strong><br>
  The framework an AI harness sits on, assembled from plug-and-play components.<br>
  A Rust crate, with Python bindings over the same kernel.
</p>

<p align="center">
  <a href="https://github.com/xwings/kerness/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/xwings/kerness/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/xwings/kerness/blob/main/LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-F59E0B"></a>
  <img alt="Rust 1.88+" src="https://img.shields.io/badge/rust-1.88%2B-B7410E">
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

The name is the design: a **ker**nel for a har**ness**.

### Two artifacts, one kernel

Kerness is a **Rust crate**. `crates/kerness/` links no Python, spawns no
threads, and runs a whole session on the calling thread — a stack trace from
inside a tool handler reaches back to `Session::run`.

`bindings/python/` is a **binding**: a thin PyO3 layer that decides nothing.
It forwards into the same kernel, so a session driven from Python takes the same
code path as one driven from `main()`. What Python adds is what Python callers
expect — a `Provider` you subclass, a lambda as a tool handler, a `pydantic`
model for structured output.

Neither surface is a wrapper around the other's use case, and neither is the
"real" one. Pick the language; the framework is the same.

That last part is a rule and not an aspiration: **a feature is written in
Rust.** The installed Python package holds the classes callers subclass, the
few the extension cannot declare, and re-exports — never behaviour. Where a
feature needs something only the interpreter has, the crate names the need as a
trait and the binding installs it at import, so `capsys` captures a console
channel, `caplog` sees a warning, and `mock.patch` intercepts a request, with
the decision still made in one place.

### The split

| The kernel owns | Your harness declares |
| --- | --- |
| Provider transport, retries, backoff | Which models each agent uses |
| Tool dialects (OpenAI / Anthropic / text-fence) | Which tools exist and what they do |
| Prompt assembly and ordering | Roles, personas, system prompts, language |
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
| **Provider** | `OpenAiProvider`, `ClaudeProvider`, `OpenRouterProvider`, `CustomProvider`; OAuth credentials where the vendor offers them | implementing the `Provider` trait — one required method, `chat` |
| **Channel** | `ConsoleChannel`, `FileChannel`, `LogChannel`, `MultiChannel` | implementing `Channel` — one required method, `send` |
| **Tools** | `cmd`, `read_file`, `list_dir`, `write_memory` | `session.add_tool(name, description, parameters, handler)` |
| **Skills** | `challenge`, `fact-check`, `summarize`, `agent-browser` | dropping a `SKILL.md` directory on disk |
| **Roles** | `participant`, `orchestrator` | a `.md` file whose frontmatter declares a `position:`, or inline prose |
| **Personas** | `pragmatic_engineer`, `devils_advocate` | a `.md` file, or inline prose |
| **Gameplans** | `debate`, `discussion`, `research` | a new Markdown file — see below |
| **Access** | closed by default; a workspace that grants its own contents, glob and regex command allow-lists, and path allow-lists that reach past the workspace | an `AccessPolicy`, plus an approval callback |
| **Memory** | `FileMemory` — a plain `.md` file per scope, read-only unless asked; `SummarizingMemory` — recent notes verbatim, the rest folded into a running summary at the end of the run | implementing `MemoryStore` — two required methods, `read` and `append` — and passing it as `memory_store` |
| **Session file** | JSON snapshot after every turn | `session_file` — absent means persist nothing |

The names are the Rust ones. Python spells the two acronym providers the way
Python callers expect — `OpenAIProvider`, plus `OpenAIOAuthProvider` and
`ClaudeOAuthProvider` — and a trait to implement becomes a class to subclass;
everything else carries the same name in both.

A `CustomProvider` pointed at any OpenAI-compatible endpoint covers most local
inference servers without implementing anything at all.

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
participant is refused before the first API call, a `tools:` entry naming a tool
nobody registered is an error rather than a silent drop, and every problem is
reported at once instead of one per run. The declared `result:` fields come back
as typed values on the session result.

A new harness is a new Markdown file. It is not a new runtime, not a subclass,
and not a fork.

## Install

**Rust** — MSRV 1.88, no build script, no system libraries:

```toml
[dependencies]
kerness = { git = "https://github.com/xwings/kerness" }
```

**Python** — `abi3` wheels from CPython 3.10 up, so one build covers every
supported interpreter:

```sh
pip install kerness                  # runtime
pip install 'kerness[structured]'    # plus pydantic, for OpenAIProvider(output_type=...)
```

From a checkout, either side. The root is a Cargo workspace, so a Python build
starts from the binding's own directory:

```sh
cargo test --workspace                           # the kernel

cd bindings/python && maturin develop            # the binding
python -m kerness.selfcheck                      # pass = "OK: all core checks passed"
```

## Set it once on the session, override it per agent

The session is configured before any agent exists, and everything it carries —
provider, model, reasoning effort, persona, language, system prompt, memory
scope, workspace — is a **default**. An agent that names none of them inherits
every one; an agent that names one overrides it, for itself alone. That is the
whole rule, and it has two deliberate exceptions.

**Provider and model inherit as a pair.** A model name only means something on
the backend it was written for, so an agent that brings its own provider must
name its own model; inheriting the session's would silently ask one vendor for
another's model. It is an error at `run()`, naming the agent.

**The workspace only ever narrows.** A session's workspace grants its own
contents — every path under it is readable without an allow-list entry — and it
is the working directory a command starts in. Unset, it is the directory the
program was launched from. `allowed_dirs` and `allowed_files` reach *past* it,
which is how a session confined to one project still reads `/tmp`; the two
together are the whole of what a session can touch, and an approval callback
cannot add to them. An agent may set a workspace of its own, and it is
intersected with the session's rather than replacing it — otherwise an agent
stanza would be a way to hand itself more of the filesystem than the session was
given.

One further asymmetry, and it is not about inheritance: **`role` has no session
default at all.** A session-wide role would make every agent the orchestrator at
once. `role` is what an agent *is* in the session — its position and its job —
and it is a built-in name, a path to a `.md` role file, or that job written out
as prose. `persona` is a different question, *who* the agent is, and it reaches
the prompt and nothing else. Unset, `role` seats a participant, and prose seats a
participant too: only a role file declaring `position: orchestrator` in its
frontmatter can seat the chair, so privilege comes from a declaration and never
from a substring somebody wrote.

## A run, in Rust

Two providers, four agents, a tool, and two skills — the whole configuration
contract in one file.

```rust
use std::sync::Arc;

use kerness::access::AccessPolicy;
use kerness::provider::{
    ClaudeConfig, ClaudeCredential, ClaudeProvider, OpenAiConfig, OpenAiProvider,
};
use kerness::tooling::Arguments;
use kerness::{Agent, ConsoleChannel, Provider, ReasoningEffort, Session, SessionConfig};
use serde_json::{json, Value};

let openai: Arc<dyn Provider> = Arc::new(OpenAiProvider::new(OpenAiConfig {
    api_key: std::env::var("OPENAI_API_KEY")?,
    ..Default::default()
})?);
let claude: Arc<dyn Provider> = Arc::new(ClaudeProvider::new(ClaudeConfig {
    credential: ClaudeCredential::ApiKey(std::env::var("ANTHROPIC_API_KEY")?),
    ..Default::default()
}));

// Everything on the config is a default. Agents fill in from it at `run()`.
let mut session = Session::new(SessionConfig {
    gameplan: "research".to_string(),
    topic: "Should the cache be write-through?".to_string(),
    provider: Some(openai),
    model: Some("gpt-4o".to_string()),
    reasoning_effort: ReasoningEffort::High,
    channel: Some(Arc::new(ConsoleChannel::default())),
    memory: "/srv/work/notes.md".to_string(),     // the scope the store is asked for
    memory_write: true,                           // ...and, here, may append to
    memory_store: None,                           // None is FileMemory: a scope is a path
    session_file: Some("/srv/work/run.json".to_string()), // None persists nothing
    access_policy: Some(AccessPolicy {
        workspace: Some("/srv/work".to_string()),  // this tree, and nothing else
        allowed_dirs: vec!["/tmp".to_string()],    // ...except what is named here
        allowed_commands: vec!["rg *".to_string()],
        ..AccessPolicy::new()
    }),
    ..Default::default()
})?;

// A tool of your own. The handler is a closure; the kernel translates the
// schema into whichever dialect each agent's provider speaks, parses the call
// back out, feeds the result in, and stops a model that loops on bad calls.
session.add_tool(
    "lookup_price",
    "Look up the current price of a ticker.",
    json!({"type": "object",
           "properties": {"ticker": {"type": "string"}},
           "required": ["ticker"]}),
    Arc::new(|args: &Arguments, _actor: &str| {
        let ticker = args.get("ticker").and_then(Value::as_str).unwrap_or_default();
        Ok(format!("{ticker} is at 41.20"))
    }),
)?;
session.add_skill("fact-check")?;
session.add_skill("summarize")?;

// Alice names nothing but a persona, so she takes every default above.
session.add_agent(Agent {
    persona: Some("pragmatic_engineer.md".to_string()),
    ..Agent::new("Alice")
})?;

// Bob overrides one thing — a cheaper model on the same backend — and confines
// himself to a directory inside the session's workspace.
session.add_agent(Agent {
    persona: Some("devils_advocate.md".to_string()),
    workspace: Some("/srv/work/scratch".to_string()),
    ..Agent::new("Bob").with_model("gpt-4o-mini")
})?;

// Carol brings her own vendor, so she must name her own model: "gpt-4o" means
// nothing to Anthropic, and inheriting it silently would be the wrong answer.
// `with_provider` takes both for that reason. Effort is portable, so it
// inherits or overrides on its own.
session.add_agent(Agent {
    reasoning_effort: Some(ReasoningEffort::Medium),
    skills: Some(vec!["fact-check".to_string()]),   // Alice and Bob get both
    ..Agent::new("Carol").with_provider(claude, "claude-sonnet-4-5")
})?;

// The chair, seated by a role file that declares `position: orchestrator`.
session.add_agent(Agent::new("Mod").with_role("orchestrator"))?;

let result = session.run()?;
println!("{}", result.summary());
println!("{:?}", result.fields.get("findings"));   // the gameplan's declared fields
println!("{} rounds, ended on {}", result.rounds_run, result.end_reason);
```

Full file: [`crates/kerness/examples/debate.rs`](https://github.com/xwings/kerness/blob/main/crates/kerness/examples/debate.rs) —
`cargo run -p kerness --example debate`.

To watch a session run without an API key, use
[`offline_debate`](https://github.com/xwings/kerness/blob/main/crates/kerness/examples/offline_debate.rs), which drives the
`debate` gameplan against a scripted provider:

```sh
cargo run -p kerness --example offline_debate    # no key, no network
```

Seven more examples sit beside it — per-agent providers, memory, structured
output, a custom tool, a custom channel, and the access boundary widened for one
program.

## The same run, in Python

The binding mirrors the crate, in the shapes Python callers expect: keyword
arguments instead of a config struct, a plain callable instead of a closure in
an `Arc`, a dataclass instead of an `AccessPolicy` literal. Same kernel, same
resolution rules, same errors.

```python
import os

from kerness import (
    AccessPolicy, ClaudeProvider, ConsoleChannel, OpenAIProvider, Session,
)

openai = OpenAIProvider(api_key=os.environ["OPENAI_API_KEY"])
claude = ClaudeProvider(api_key=os.environ["ANTHROPIC_API_KEY"])

session = Session(
    gameplan="research",
    topic="Should the cache be write-through?",
    provider=openai,
    model="gpt-4o",
    reasoning_effort="high",
    channel=ConsoleChannel(),
    memory="/srv/work/notes.md",
    memory_write=True,
    session_file="/srv/work/run.json",  # resumable; omit to persist nothing
    access_policy=AccessPolicy(
        workspace="/srv/work",      # this tree, and nothing else
        allowed_dirs=["/tmp"],      # ...except what is named here
        allowed_commands=["rg *"],
    ),
)

prices = {"KRN": "41.20"}
session.add_tool(
    "lookup_price",
    "Look up the current price of a ticker.",
    {"type": "object",
     "properties": {"ticker": {"type": "string"}},
     "required": ["ticker"]},
    lambda args: prices.get(args["ticker"], "unknown"),
)
session.add_skill("fact-check")
session.add_skill("summarize")

session.add_agent("Alice", persona="pragmatic_engineer.md")
session.add_agent("Bob", persona="devils_advocate.md",
                  model="gpt-4o-mini", workspace="/srv/work/scratch")
session.add_agent("Carol", provider=claude, model="claude-sonnet-4-5",
                  reasoning_effort="medium", skills=["fact-check"])
session.add_agent("Mod", role="orchestrator")

result = session.run()
print(result.summary)
print(result.fields["findings"])   # the gameplan's declared fields
print(result.rounds_run, result.end_reason)
```

The `add_*` calls return the session, so registration chains if you would rather
write it that way.

## What the kernel does while it runs

Neither of the above is a different runtime, so the following holds for both.

Skills use progressive disclosure: prompts carry only names and descriptions,
and the full instructions load on demand through a turn-local `Skill` tool. A
skill also says which tools the turn should hold: `allowed-tools:` narrows it,
`requires-tools:` adds back what the skill cannot work without, out of whatever
the session registered. A skill requiring a tool nobody registered is refused
before the first call rather than quietly doing nothing. When the policy trusts
bundles, loading a skill also grants read access to its own directory.

There is no daemon and no server. A run given a session file writes its state to
disk after every turn and continues from that file the next time the same
program runs. Resume checks identity first: a snapshot from a different gameplan
or a different agent roster is refused, not half-applied.

## Layout

```text
Cargo.toml       # the only manifest at the root
crates/kerness/  # the kernel, pure Rust — no PyO3, no Python
  src/           #   30 modules, 380 unit tests inline
  tests/         #   109 integration tests, over the public API only
  examples/      #   8 runnable Rust harnesses, one needing no key
  assets/        #   bundled gameplans, roles, personas, skills
bindings/python/ # everything the wheel is built from
  pyproject.toml #   the wheel's manifest — `pip install .` runs here
  src/           #   the PyO3 extension module, kerness._core
  kerness/       #   the Python package: the subclassable classes, shims, assets
  tests/         #   pytest suite, over the binding
  examples/      #   runnable Python harnesses
ARCHITECTURE/    # one document per subsystem
```

The bundled `debate`, `discussion`, and `research` gameplans are worked examples
of the contract, not the product.

## Testing

Each suite proves its own layer. The Rust integration tests compile against the
crate's public API exactly as a dependent does, so a break in that surface fails
a test here rather than a downstream build; the pytest suite proves the binding
carries the kernel's behaviour across the FFI boundary intact.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                     # 380 unit + 109 integration
cargo build -p kerness --examples          # every example still compiles
cargo run -p kerness --example offline_debate   # a whole session, no key

python -m pytest bindings/python/tests -q      # 487 tests
python -m kerness.selfcheck                    # exit 0
ruff check bindings/python
```

CI runs all of it on every push, on Rust stable and the 1.88 MSRV floor, and on
Python 3.10 and 3.13.

## Documentation

[`ARCHITECTURE.md`](https://github.com/xwings/kerness/blob/main/ARCHITECTURE.md) is the entry point: mission, workspace
layout, boot flow, well-known constants, and an index of one document per
subsystem under [`ARCHITECTURE/`](https://github.com/xwings/kerness/tree/main/ARCHITECTURE). Each carries live `file:line`
references and the commands that prove it works.

## License

MIT. See [LICENSE](https://github.com/xwings/kerness/blob/main/LICENSE).
