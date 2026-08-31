# Role

## Goal

A role is what an agent *is* in a session — its position in the loop and the job
it was given. It is a Markdown file with YAML frontmatter: `position:` is the one
field the framework acts on, and the body is the agent's base system prompt. This
module loads one and resolves the path it was named by.

`role` and [persona](persona.md) coexist and divide cleanly:

| | answers | consumed by |
| --- | --- | --- |
| `role` | *what is your position and job in this session* | the loop — dispatch, prompt base, turn order |
| `persona` | *who are you — background, expertise, voice* | the prompt only |

An agent can be the orchestrator and a devil's advocate at once. `role` unset
means participant, because the orchestrator is a privileged singleton that
conducts the run: an agent that named nothing must join the conversation, not
take it over.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/role.rs` | the `Position` enum, loading, and path resolution |
| `crates/kerness/assets/roles/*.md` | the two built-in roles |
| `bindings/python/src/types.rs:1093` | `PyRoleConfig` |
| `bindings/python/src/funcs.rs:341` | the three loader functions |
| `bindings/python/kerness/role_loader.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/role.rs:31` — `Position` — `Participant | Orchestrator`,
  `#[default] Participant`. Closed on purpose: an unrecognised position satisfies
  neither `Agent::is_orchestrator` nor the orchestrator lookup, so accepting one
  would turn the session's conductor into an extra contributor with nothing
  reported anywhere.
- `crates/kerness/src/agent.rs:43` — `Agent.role: Option<String>` — the *spec* as
  the caller wrote it. `crates/kerness/src/agent.rs:46` — `Agent.position` — the
  chair it selected. The open half and the closed half of the same pair.
- `crates/kerness/src/role.rs:70` — `RoleConfig` — `name`, `position`,
  `description`, `content`. A file with no frontmatter at all is a body and
  nothing else: a participant named for its own stem, which is the smallest
  useful role there is.
- `crates/kerness/src/role.rs:138` — `role_file(spec, search)` — the resolution
  that decides file-or-prose, described below.
- `crates/kerness/src/role.rs:96` — `load_role(path, search)` and `:149` —
  `resolve_role_path(path, search)` — same shape, same search order, and the same
  names-every-directory-tried error as [persona.md](persona.md).
- `crates/kerness/src/role.rs:123` — `list_builtin_roles()` — enumerated from
  disk, like the other asset lists.
- `crates/kerness/src/role.rs:66` — `DEFAULT_ROLE_FILE` — `participant.md`, the
  role an agent that named none of its own reads.

### Three-way resolution, and why prose cannot conduct

`Session::add_agent` (`crates/kerness/src/session.rs:723`) reads the spec far
enough to learn the position:

1. it looks like a path — ends with `.md`, or holds a separator → that file, and
   not finding it is an error rather than a quiet demotion to prose;
2. it is a bare name matching a built-in stem → that built-in;
3. anything else → inline prose, position `Participant`.

Case 3 takes the safe direction deliberately. `role = "orchestrator, but
sceptical"` is prose and seats a participant; conducting the session requires
naming a role file whose frontmatter declares `position: orchestrator`.
**Privilege comes from a declaration, never from a substring.**

Cases 1 and 2 rewrite `agent.role` to the absolute path that was found, so a
later `chdir` cannot make the file unfindable halfway through a run, and
`Agent::resolve_role` (`crates/kerness/src/agent.rs:240`) stays a plain lookup
with no search path of its own.

Resolution happens at *add* time, unlike every option in
[session.md](session.md)'s inheritance table, which resolves at `run()`. Role has
no session-level default to wait for — a session-wide role would make every agent
the orchestrator at once — so a typo is knowable the moment it is written, and
the error names the agent that wrote it.

### The two built-in roles

- **`participant.md`** — its body is the base system prompt every participant
  gets. `SessionConfig.system_prompt` overrides it for everyone who named no
  role, and `Agent.system_prompt` overrides both
  (`crates/kerness/src/session.rs:1148`).
- **`orchestrator.md`** — the whole orchestrator prompt: the layout and every
  literal word. `build_orchestrator_prompt`
  (`crates/kerness/src/session.rs:1181`) supplies only what the harness contract
  knows — the roster, the phase block, the end and flow rules — substituted by
  name through the same mechanism `decorate_system_prompt` already applies to
  `{topic}` and `{bot_name}`. The frontmatter is not shown to any model.

Both ship twice, byte-identical, at `crates/kerness/assets/roles/` and
`bindings/python/kerness/roles/`; `bindings/python/tests/test_packaging.py:42` is
the only thing keeping the pair in step.

## Interactions

- [session.md](session.md) — `add_agent` is the one door, and the
  one-orchestrator rule is enforced there.
- [agent.md](agent.md) — `resolve_role` produces the base prompt
  `build_system_prompt` decorates.
- [prompting.md](prompting.md) — the `is_orchestrator()` branch reads `position`.
- [selfcheck.md](selfcheck.md) — loads every built-in role.

## How to Test

```sh
cargo test -p kerness role                                              # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_role_loader.py -q  # pass = 0 failed
```

- `crates/kerness/src/role.rs:174` —
  `a_file_with_no_frontmatter_is_a_participant_named_for_itself` — the default
  falls the safe way.
- `:232` — `a_missing_path_is_an_error_rather_than_prose` — the distinction that
  keeps a typo'd path from becoming an agent's role description.
- `crates/kerness/tests/session_run.rs` —
  `a_role_seats_an_agent_by_declaration_and_never_by_prose` — the four spec kinds
  against the four chairs they select.
- `bindings/python/tests/test_session.py` —
  `test_prose_never_reaches_the_orchestrators_seat` — the same property from
  Python.

## Open Gaps / Roadmap

- The body is loaded at add time to read the position and again at prompt
  assembly to read the content. Caching it would put a hidden field on a struct
  callers construct with a literal, which costs more than the second read.
- `Position` has two members, and a third — a silent observer, say — would need a
  loop branch to go with it. The enum being closed is what makes that a
  compile-time list rather than a search.
