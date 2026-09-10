---
eatmycode_version: "1.2.0"
---

# Module index

Supporting page for [ARCHITECTURE.md](../ARCHITECTURE.md#index): one row per
subsystem owner with its source paths, responsibility, the changes that should
send you there, and the partners it names. Read the root's cross-cutting rules
first, then the owner, then the partners, then the cited code and tests.
Domain owners describe their Python-facing behavior; [bindings.md](bindings.md)
owns the shared conversion and packaging mechanisms across those files.

## Owners

| Module doc | Owns (source paths) | Responsibility | Read it when you change | Partners |
| --- | --- | --- | --- | --- |
| [access.md](access.md) | `crates/kerness/src/access.rs`, `crates/kerness/src/exec.rs`, `bindings/python/src/access.rs`, `bindings/python/kerness/access.py` | the permission boundary: commands, paths, hosts, workspaces, the console prompt seam, command deadlines and process groups | any allow-list, path resolution, approval flow, `run_command`, a new tool that touches the filesystem or network | toolkit, skills, session, run |
| [agent.md](agent.md) | `crates/kerness/src/agent.rs`, `PyAgent` in `bindings/python/src/types.rs` | the agent record, option inheritance, system prompt and message assembly | a new per-agent option, inheritance rules, what a provider is sent | role, persona, prompting, provider, session |
| [agent-runtime.md](agent-runtime.md) | `crates/kerness/src/agent_runtime.rs`, `PyAgentRunner` in `bindings/python/src/runtime.rs` | one resumable agent turn: provider call, pending tool calls, results, guards, typed turn reason | tool-loop bounds, turn continuation, overflow retry behaviour | provider, toolkit, toolschema, sessionfile, run |
| [bindings.md](bindings.md) | `bindings/python/src/*.rs`, `bindings/python/kerness/*.py`, `bindings/python/pyproject.toml` | the PyO3 boundary, value and error conversion, callback binding, the installed package and its shims | anything crossing to Python, a new pyclass, a new shim, the wheel manifest | errors, provider, channel, memory, run, selfcheck |
| [channel.md](channel.md) | `crates/kerness/src/channel.rs`, `crates/kerness/src/logging.rs`, `bindings/python/src/channel.rs`, `bindings/python/kerness/channel.py` | message delivery, the four bundled channels, the console-writer and logger seams, parked Python exceptions | output destinations, diagnostics, a caller's channel subclass | session, bindings, access |
| [compaction.md](compaction.md) | `crates/kerness/src/compaction.rs`; `fit_conversation` in `crates/kerness/src/session.rs` | token estimate, the context ceiling per agent, the summarize-the-prefix rewrite and the one overflow retry | context limits, summary prompts, the estimate | conversation, provider, run, errors |
| [context.md](context.md) | `crates/kerness/src/context.rs`; `add_context`/`resolve_context` in `crates/kerness/src/session.rs` | host-supplied standing facts rendered once per agent | a new context source, the `context:` key, caching | harness, prompting, session |
| [conversation.md](conversation.md) | `crates/kerness/src/conversation.rs`, `PyConversation` in `bindings/python/src/runtime.rs` | turns, transcript, and the rendered `ChatMessage` list | message shapes, what a provider sees, restore | compaction, sessionfile, agent-runtime |
| [errors.md](errors.md) | `crates/kerness/src/error.rs`, `bindings/python/src/errors.rs`, `bindings/python/kerness/exceptions.py` | the error enum, the exception hierarchy, and the total map between them | a new failure kind, overflow detection, exception attributes | every module; bindings |
| [gameplan.md](gameplan.md) | `crates/kerness/src/gameplan.rs`, `crates/kerness/src/assets.rs`, `crates/kerness/assets/gameplans/` | loading a Markdown gameplan, splitting frontmatter from body, the assets root and enumeration | a bundled gameplan, asset resolution, `set_root` | harness, persona, selfcheck |
| [harness.md](harness.md) | `crates/kerness/src/harness.rs`, `crates/kerness/src/yaml.rs`, spec pyclasses in `bindings/python/src/types.rs` | the frontmatter contract: parse, validate against registrations, narrow tools and context, widen skills | any frontmatter key, validation message, YAML scalar rule | gameplan, session, loop, toolkit, skills, context |
| [jsonschema.md](jsonschema.md) | `crates/kerness/src/jsonschema.rs` | strict-mode schema rewriting and argument validation messages | tool parameter schemas, structured output, validation wording | toolschema, toolkit, provider |
| [loop.md](loop.md) | `crates/kerness/src/orchestrator.rs`, loop adapters in `bindings/python/src/runtime.rs` | the orchestrator action machine: routing, phases, retries, closing verdict, end reasons | phase rules, turn limits, host-driven scheduling, loop snapshot | harness, agent-runtime, sessionfile, utils, run |
| [memory.md](memory.md) | `crates/kerness/src/memory.rs`, `Memories`/`remember` in `crates/kerness/src/session.rs`, `bindings/python/src/memory.rs`, `bindings/python/kerness/memory.py` | the store trait, three bundled stores, the filter, scopes, metered maintenance | a store, the filter, memory tools, maintenance or cleanup | prompting, access, toolkit, run, provider |
| [persona.md](persona.md) | `crates/kerness/src/persona.rs`, `crates/kerness/assets/personas/` | loading a persona and rendering it for a prompt | persona search paths or rendering | agent, gameplan, selfcheck |
| [prompting.md](prompting.md) | `crates/kerness/src/prompting.rs`, `PyPromptAssembler` in `bindings/python/src/runtime.rs` | system-prompt assembly, part order, memory and context framing | any prompt block, caveat text, part order, dialect branch | agent, memory, context, skills, toolschema |
| [provider.md](provider.md) | `crates/kerness/src/provider/`, `crates/kerness/src/http.rs`, `crates/kerness/src/usage.rs`, `bindings/python/src/provider.rs`, `bindings/python/kerness/provider.py` | the `Provider` trait, four backends, retries and degrade latches, the transport seam, usage accounting and budgets | a backend, a request field, retry policy, metering, pricing | agent-runtime, toolschema, jsonschema, compaction, run |
| [role.md](role.md) | `crates/kerness/src/role.rs`, `crates/kerness/assets/roles/` | the two positions, role files, three-way spec resolution | a role file, the orchestrator prompt, position rules | agent, session, prompting |
| [run.md](run.md) | `crates/kerness/src/session/run.rs`, `crates/kerness/src/session/capabilities.rs`, `crates/kerness/src/session/outcome.rs`, `bindings/python/src/run.rs` | owned execution: step engine, inputs, events, approvals, scoped tool capabilities, budgets, outcomes, checkpoints and recovery | any step transition, approval, budget, event, result validation, resume behaviour | session, loop, agent-runtime, access, sessionfile, provider |
| [selfcheck.md](selfcheck.md) | `bindings/python/kerness/selfcheck.py` | `python -m kerness.selfcheck`, the installed-package health check | a new package module or asset kind | gameplan, role, persona, skills, bindings |
| [session.md](session.md) | `crates/kerness/src/session.rs`, `bindings/python/src/session.rs`, `bindings/python/kerness/session.py` | configuration, registration, preparation, shared resources, the blocking `run` adapter | a session option, registration rule, preparation order, resource lifecycle | run, harness, agent, access, memory, prompting |
| [sessionfile.md](sessionfile.md) | `crates/kerness/src/sessionfile.rs`, snapshot functions in `bindings/python/src/funcs.rs` | the snapshot schema, identity check, atomic save and validated load | the schema, a new checkpointed field, identity rules, file safety | run, loop, conversation, agent-runtime |
| [skills.md](skills.md) | `crates/kerness/src/skill/`, `crates/kerness/assets/skills/`, `bindings/python/src/skill.rs` | skill bundles, the `Skill` tool, the tool gate and required tools, bundle grants | a bundled skill, `allowed-tools`/`requires-tools`, activation | harness, toolkit, access, prompting |
| [testing.md](testing.md) | `crates/kerness/tests/`, `crates/kerness/src/testing.rs`, `bindings/python/tests/`, `crates/kerness/examples/`, `bindings/python/examples/`, `.github/workflows/` | the three suites, their doubles, the examples, CI, and the whole-workspace gate | adding or moving a test, a double, an example, a CI step | every module |
| [toolkit.md](toolkit.md) | `crates/kerness/src/tooling.rs`, `crates/kerness/src/toolkit.rs`, tool pyclasses in `bindings/python/src/types.rs` | tool specs, text-fence call parsing, the tools prompt, dispatch and result shaping, allow-list narrowing | a tool, the call parser, dispatch errors, narrowing order | jsonschema, toolschema, access, skills, agent-runtime |
| [toolschema.md](toolschema.md) | `crates/kerness/src/toolschema.rs`, `register_dialect` in `bindings/python/src/types.rs`, `bindings/python/kerness/_enums.py` | native tool dialects and their exact wire shapes | a dialect, a provider's tool format, result rendering | provider, toolkit, jsonschema, prompting |
| [utils.md](utils.md) | `crates/kerness/src/utils.rs`, `crates/kerness/src/pyfmt.rs` | keyword and mention scanning, memory markers, retry, CPython-compatible formatting | a terminator, routing syntax, retry backoff, value rendering | loop, memory, provider, toolkit |

## Supporting pages

Each is reachable from its owner and holds detail the owner summarises.

| Page | Owner | Holds |
| --- | --- | --- |
| [design.md](design.md) | the root's System Design | subsystem dependencies, state ownership, the four process-wide seams, design decisions |
| [runtime.md](runtime.md) | the root's Runtime and Data Flow | entry points, preparation and execution, data paths, configuration contracts, failure and recovery, concurrency, well-known constants |
| [conventions.md](conventions.md) | the root's Coding Style and Code Design | enforced rules with commands, observed Rust and Python conventions, what nothing enforces |
| [verification.md](verification.md) | the root's Verification and Review Map | CI jobs, the change-to-test map, coverage gaps |
| [workspace.md](workspace.md) | the root's Workspace Map | the full tree and the edit restrictions with their checks |
| [roadmap.md](roadmap.md) | the root's Roadmap | the milestone table with evidence, the CI configuration gap, the improvement candidates |
| [memory-stores.md](memory-stores.md) | [memory.md](memory.md) | `SummarizingMemory` and `CuratedMemory` |
