---
eatmycode_version: "1.2.0"
---

# Role

## Goal

A role is what an agent *is* in a session — its position in the loop and the job
it was given. It is a Markdown file with YAML frontmatter: `position:` is the one
field the framework acts on, and the body is the agent's base system prompt. This
module owns the `Position` enum, loading a role file, and deciding whether a
`role:` spec names a file or is prose.

It does not own seating — `Session::add_agent` reads the position and enforces
the one-orchestrator rule — nor the orchestrator's filled-in prompt, which
[session.md](session.md)'s `build_orchestrator_prompt` substitutes into the
role's body. `role` and [persona](persona.md) coexist and divide cleanly:

| | answers | consumed by |
| --- | --- | --- |
| `role` | *what is your position and job in this session* | the loop — dispatch, prompt base, turn order |
| `persona` | *who are you — background, expertise, voice* | the prompt only |

An agent can be the orchestrator and a devil's advocate at once. `role` unset
means participant, because the orchestrator is a privileged singleton that
conducts the run: an agent that named nothing must join the conversation, not
take it over.

## Status

`done` — `cargo test -p kerness role` passes 14 tests and
`.venv/bin/python -m pytest bindings/python/tests/test_role_loader.py -q`
passes 9. Both built-in roles load with a position, a description, and a body
through `crates/kerness/tests/public_api.rs:197` and the self-check.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/role.rs` | `Position`, `RoleConfig`, `DEFAULT_ROLE_FILE`, loading, `role_file`, path resolution |
| `crates/kerness/src/assets.rs` | `split_frontmatter`, `resolve_path`, `candidates`, and `list_markdown_stems`, shared with every asset family |
| `crates/kerness/assets/roles/*.md` | the two built-in roles |
| `bindings/python/src/types.rs:1091` | `PyRoleConfig`, a `get_all, set_all` pyclass whose constructor parses the position |
| `bindings/python/src/funcs.rs:343` | the three `#[pyfunction]` loaders |
| `bindings/python/src/funcs.rs:711` | `DEFAULT_ROLE_FILE` re-exported into `_core` |
| `bindings/python/kerness/role_loader.py` | re-export shim |
| `bindings/python/kerness/roles/*.md` | the byte-identical installed copies |

## Language and Conventions

One Rust crate module, a pyclass and three pyfunctions in the binding, and a
Python shim. The root's [Coding Style and Code
Design](../ARCHITECTURE.md#coding-style-and-code-design) rules apply. Local
facts:

- `Position` is a closed `Copy` enum with `#[default] Participant`
  (`crates/kerness/src/role.rs:31`); it crosses the boundary as the string
  `"participant"` or `"orchestrator"` (`PyRoleConfig.position`,
  `bindings/python/src/types.rs:1095`; `PyAgent.position`, `:791`), the same
  way `ReasoningEffort` does, and `PyRoleConfig.__new__` parses it so an
  unseatable value fails at construction (`:1122`).
- Frontmatter is read through `assets::split_frontmatter`
  (`crates/kerness/src/assets.rs:139`), which is line-based and YAML 1.1
  ([harness.md](harness.md)); a file with no frontmatter is a body and nothing
  else.
- Errors: a missing file is `Error::NotFound` (Python `FileNotFoundError`); a
  bad position or malformed frontmatter is `Error::Value` (Python `ValueError`)
  naming the file; a read failure is `Error::Io`.
- Unit tests use `crate::testing::TempDir` (`crates/kerness/src/role.rs:156`)
  and sentence-style names; Python tests use `tmp_path` and the discovery test
  reads the installed package directory
  (`bindings/python/tests/test_role_loader.py:88`).

## Design and Invariants

### A closed position, an open spec

`Agent::role` (`crates/kerness/src/agent.rs:43`) is the open half — any
built-in name, any `.md` path, any prose — and `Agent::position` (`:46`) is the
closed half the session writes back. `Position` is closed on purpose: an
unrecognised position satisfies neither `Agent::is_orchestrator`
(`crates/kerness/src/agent.rs:250`) nor the orchestrator lookup, so accepting
one would turn the session's conductor into an extra contributor with nothing
reported anywhere. Enforced by `a_position_that_is_nearly_right_is_still_rejected`
(`crates/kerness/src/role.rs:270`) and
`test_a_position_it_cannot_act_on_is_refused_at_construction`
(`bindings/python/tests/test_role_loader.py:78`).

### Three-way resolution, and why prose cannot conduct

`role_file` (`crates/kerness/src/role.rs:138`) decides file-or-prose, and
`Session::add_agent` (`crates/kerness/src/session.rs:737`) reads the result far
enough to learn the position:

1. it looks like a path — ends with `.md`, or holds a separator → that file, and
   not finding it is an error rather than a quiet demotion to prose;
2. it is a bare name matching a built-in stem, or a file beside the gameplan →
   that file;
3. anything else → inline prose, position `Participant`.

Case 3 takes the safe direction deliberately. `role = "orchestrator, but
sceptical"` is prose and seats a participant; conducting the session requires
naming a role file whose frontmatter declares `position: orchestrator`.
**Privilege comes from a declaration, never from a substring.** Enforced by
`a_bare_name_finds_a_file_and_prose_does_not` (`crates/kerness/src/role.rs:220`),
`a_role_seats_an_agent_by_declaration_and_never_by_prose`
(`crates/kerness/tests/session_run.rs:1197`), and
`test_prose_never_reaches_the_orchestrators_seat`
(`bindings/python/tests/test_session.py:168`).

Case 1 fails loudly because `role = "./roles/typo.md"` becoming the literal
role description `./roles/typo.md` is the same silent-success failure
[persona.md](persona.md) refuses, and it costs a whole run of provider calls to
discover. Enforced by `a_missing_path_is_an_error_rather_than_prose`
(`crates/kerness/src/role.rs:232`) and
`a_missing_role_file_is_refused_where_it_was_named`
(`crates/kerness/tests/session_run.rs:1252`).

### Resolution happens at add time and pins the path

Cases 1 and 2 rewrite `agent.role` to the absolute path that was found
(`crates/kerness/src/session.rs:752`), so a later `chdir` cannot make the file
unfindable halfway through a run, and `Agent::resolve_role`
(`crates/kerness/src/agent.rs:240`) stays a plain lookup with no search path of
its own. Unlike every option in [session.md](session.md)'s inheritance table,
which resolves at `run()`, role has no session-level default to wait for — a
session-wide role would make every agent the orchestrator at once — so a typo
is knowable the moment it is written, and the error names the agent. Enforced
by `test_a_role_file_seats_by_its_frontmatter`
(`bindings/python/tests/test_session.py:178`), which asserts the pinned path is
absolute.

### One orchestrator

`add_agent` refuses a second orchestrator by naming the first
(`crates/kerness/src/session.rs:758`). Enforced by
`a_second_orchestrator_is_refused_by_name` (`:2556`) and
`test_duplicate_orchestrator_raises` (`bindings/python/tests/test_session.py:152`).

### The default falls the safe way

A file with no frontmatter is a participant named for its stem — the smallest
useful role there is, one paragraph in a file. Defaulting the position the
other way would hand the conductor's seat to anyone who wrote a bare paragraph.
Enforced by `a_file_with_no_frontmatter_is_a_participant_named_for_itself`
(`crates/kerness/src/role.rs:174`) and its Python counterpart
(`bindings/python/tests/test_role_loader.py:31`).

### The two built-in roles

- **`participant.md`** — its body is the base system prompt every participant
  gets. `SessionConfig.system_prompt` overrides it for everyone who named no
  role, and `Agent.system_prompt` overrides both (`participant_prompt`,
  `crates/kerness/src/session.rs:1234`).
- **`orchestrator.md`** — the whole orchestrator prompt: the layout and every
  literal word. `build_orchestrator_prompt` (`:1270`) supplies only what the
  harness contract knows — the roster, the phase block, the end and flow rules
  — substituted by name through the same `{topic}`/`{bot_name}` mechanism
  `decorate_system_prompt` applies to every prompt. The frontmatter is not
  shown to any model.

Both ship twice, byte-identical, at `crates/kerness/assets/roles/` and
`bindings/python/kerness/roles/`; `bindings/python/tests/test_packaging.py:42`
is the only thing keeping the pair in step. `DEFAULT_ROLE_FILE` must name one
of them and seat a participant; `test_the_default_role_is_one_of_them`
(`bindings/python/tests/test_role_loader.py:106`) asserts both.

### Search order is the caller's

`resolve_role_path(path, search)` and `role_file` both try the path as written,
then each `search` directory, then the built-ins, through `assets::candidates`
(`crates/kerness/src/assets.rs:79`), so a gameplan can ship roles beside it.
Enforced by `a_search_directory_is_tried_before_the_built_ins`
(`crates/kerness/src/role.rs:249`).

## Key Types and Entry Points

- `crates/kerness/src/role.rs:31` — `Position` — `Participant | Orchestrator`;
  `as_str` at `:39`, `parse` at `:47` returning `Error::Value` naming both
  accepted words.
- `crates/kerness/src/role.rs:70` — `RoleConfig` — `name` (frontmatter or
  stem), `position`, `description` (printed by the self-check, never sent to a
  model), `content` (the body, trimmed).
- `crates/kerness/src/role.rs:66` — `DEFAULT_ROLE_FILE` — `participant.md`, the
  role an agent that named none reads.
- `crates/kerness/src/role.rs:96` — `load_role(path, search)` — resolve, read,
  split frontmatter, parse the position; `Error::NotFound`, `Error::Io`, or
  `Error::Value` naming the file.
- `crates/kerness/src/role.rs:138` — `role_file(spec, search)` — `Ok(Some(path))`
  for a file, `Ok(None)` for prose, `Err(NotFound)` for a path-shaped spec that
  does not resolve.
- `crates/kerness/src/role.rs:149` — `resolve_role_path(path, search)` — the
  first existing candidate, or `Error::NotFound` naming every directory tried.
- `crates/kerness/src/role.rs:123` — `list_builtin_roles()` — sorted stems of
  `assets/roles/*.md`, read from disk.
- `crates/kerness/src/agent.rs:240` — `Agent::resolve_role()` — the base
  prompt: the body of the pinned `.md`, the prose itself, or the built-in
  `participant` role when the agent named none.
- `crates/kerness/src/session.rs:737` — `Session::add_agent(agent)` — the one
  door: resolves the spec, writes `position`, pins the path, refuses a second
  orchestrator.
- `bindings/python/src/funcs.rs:343` — `load_role(path, search=None)`; `:351`
  `list_builtin_roles`; `:358` `resolve_role_path` returning a `Path`.

## Interactions

- [session.md](session.md) — `add_agent` (`crates/kerness/src/session.rs:737`)
  is the one door and enforces the one-orchestrator rule; `participant_prompt`
  (`:1234`) and `build_orchestrator_prompt` (`:1270`) consume the bodies.
  Tested at `bindings/python/tests/test_session.py:133` (`TestAddAgent`).
- [agent.md](agent.md) — `resolve_role` (`crates/kerness/src/agent.rs:240`)
  produces the base prompt `build_system_prompt` decorates;
  `is_orchestrator`/`is_participant` (`:250`, `:254`) are the position reads
  the loop branches on.
- [prompting.md](prompting.md) — `messages_for` branches on
  `Agent::is_orchestrator` to pick the prompt path.
- [loop.md](loop.md) — the orchestrator is the agent the scheduler asks to
  route; there is exactly one because this module and `add_agent` keep it so.
- [gameplan.md](gameplan.md) — the gameplan's `directory()` is the search path
  `add_agent` passes; shares `assets::resolve_path` and `split_frontmatter`.
- [selfcheck.md](selfcheck.md) — loads every built-in and requires a body
  (`bindings/python/kerness/selfcheck.py:92`).
- [errors.md](errors.md) — `Error::NotFound` and `Error::Value` cross as
  `FileNotFoundError` and `ValueError`, which
  `bindings/python/tests/test_role_loader.py:45` and `:69` assert.

## How to Test

```sh
cargo test -p kerness role                                              # pass = 14 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_role_loader.py -q  # pass = 9 passed
cargo test -p kerness --test session_run -- a_role_seats a_missing_role # pass = 2 passed
cargo test -p kerness --test public_api every_bundled_role              # pass = 1 passed
```

- `crates/kerness/src/role.rs:174` —
  `a_file_with_no_frontmatter_is_a_participant_named_for_itself` — the default
  falls the safe way.
- `crates/kerness/src/role.rs:188` —
  `an_unknown_position_names_the_file_it_came_from` — and `:270`
  `a_position_that_is_nearly_right_is_still_rejected`: the closed set, with
  the file named in the error.
- `crates/kerness/src/role.rs:220` — `a_bare_name_finds_a_file_and_prose_does_not`
  — and `:232` `a_missing_path_is_an_error_rather_than_prose`: the three-way
  resolution.
- `crates/kerness/tests/session_run.rs:1197` —
  `a_role_seats_an_agent_by_declaration_and_never_by_prose` — the four spec
  kinds against the chairs they select, through a whole session; `:1252`
  `a_missing_role_file_is_refused_where_it_was_named` names the agent.
- `bindings/python/tests/test_session.py:168` —
  `test_prose_never_reaches_the_orchestrators_seat` — the same property from
  Python; `:178` `test_a_role_file_seats_by_its_frontmatter` asserts the
  pinned absolute path; `:152` `test_duplicate_orchestrator_raises`.
- `bindings/python/tests/test_role_loader.py:88` —
  `test_it_reads_the_directory_and_every_file_in_it_loads` — the discovery
  assertion over the installed built-ins, and `:106`
  `test_the_default_role_is_one_of_them`.
- Gap: no test drives a spec holding a backslash separator
  (`crates/kerness/src/role.rs:139`), which is treated as a path on every
  platform.

## Review and Refactor Guide

- Adding a position → `Position` (`crates/kerness/src/role.rs:31`), `as_str`
  and `parse`, `Agent::is_orchestrator`/`is_participant`
  (`crates/kerness/src/agent.rs:250`), the loop branch that would consume it
  ([loop.md](loop.md)), `PyAgent.position`'s docstring
  (`bindings/python/src/types.rs:786`), and
  `a_position_round_trips_through_its_written_name`
  (`crates/kerness/src/role.rs:262`). The enum being closed is what makes this
  a compile-time list rather than a search.
- Changing resolution order → `role_file` (`:138`) and `assets::candidates`
  (`crates/kerness/src/assets.rs:79`), which [persona.md](persona.md) shares;
  update both docs and both search tests.
- Changing a built-in role's body → the orchestrator template's placeholders
  are filled by `build_orchestrator_prompt`
  (`crates/kerness/src/session.rs:1270`); a renamed placeholder is a silent
  literal in every prompt. Edit both copies of the file, or
  `bindings/python/tests/test_packaging.py:42` fails.
- Changing what `add_agent` reads → `crates/kerness/src/session.rs:737`,
  `TestAddAgent` (`bindings/python/tests/test_session.py:133`), and the two
  `session_run` tests above.
- Safe extension point: frontmatter keys other than `name`, `position`, and
  `description` are ignored by `load_role`; a new key needs a `RoleConfig`
  field, a `PyRoleConfig` field and constructor keyword, and a consumer, or it
  is dead configuration by project rule.
- Forbidden coupling: this module must not import `agent`, `session`, or
  `orchestrator`; it is a leaf that [agent.md](agent.md) and
  [session.md](session.md) call.
- Compatibility: `PyRoleConfig` is constructed by keyword and compared by
  `position == "orchestrator"` in tests, so both the field names and the two
  position strings are public.

Improvement candidates (proposals, not accepted work):

- Cache the loaded body on the agent. Benefit: the file is read at add time to
  learn the position and again at prompt assembly to read the content. Check:
  one read per agent per run, with `test_a_role_file_seats_by_its_frontmatter`
  still asserting the pinned absolute path. Declined so far because it would
  put a hidden field on a struct callers construct with a literal.

## Open Gaps / Roadmap

- The body is loaded at add time to read the position and again at prompt
  assembly to read the content; see the improvement candidate above.
- `Position` has two members, and a third — a silent observer, say — would need
  a loop branch to go with it.
- A backslash in a spec is a path separator on every platform
  (`crates/kerness/src/role.rs:139`); the access boundary already assumes POSIX
  paths, so this is consistent but untested.
