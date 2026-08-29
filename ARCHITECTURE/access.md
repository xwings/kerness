# Access

## Goal

The permission boundary. Every command a tool runs and every path a tool reads
is checked here first, against a policy the caller declares up front. Nothing
else in the framework decides whether an action is allowed — the tools in
`exec.rs` take an `&AccessManager` and cannot act without one.

Two mechanisms, and the difference between them is the module's organising
idea. **An allowlist answers *may I*.** It is additive: `allowed_programs`,
`allowed_command_patterns`, and `allowed_dirs` each name something the caller
opened, an approver may open more, and `allow_dirs` opens more mid-session.
**A workspace answers *is this inside the world*.** It grants nothing, it is
checked before any allowlist, and it can only subtract — a path outside it is
refused even when an allowlist entry and a yes-saying approver would both have
admitted it. It is also the working directory a command starts in, so a
confined session's commands are *in* the confinement rather than merely unable
to name their way out of it.

## Status

`done` — implemented and tested, with direct tests for traversal and symlink
escape rather than only for the happy path, against both the allowlists and the
workspace.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/access.rs` | `AccessPolicy`, `AccessManager`, the checks |
| `crates/kerness/src/exec.rs` | the three tools the boundary exists for: run, read, list |
| `bindings/python/src/access.rs` | the pyclasses, and reading a Python `AccessPolicy` |
| `bindings/python/kerness/access.py` | `AccessPolicy` as a dataclass, and the console prompt |

`AccessPolicy` is declared in Python (`bindings/python/kerness/access.py:83`)
rather than as a pyclass because callers build one with keyword arguments and
then mutate fields on it; `policy_from_py` (`bindings/python/src/access.rs:114`)
reads it into the Rust struct at the moment it is used, so a field assigned
after construction still takes effect.

## Key Types and Entry Points

- `crates/kerness/src/access.rs:65` — `AccessPolicy` — the declaration: allowed
  programs, allowed command patterns, allowed directories, the workspace, and
  whether a skill bundle's own directory is trusted.
- `crates/kerness/src/access.rs:77` — `AccessPolicy::workspace` — the directory
  the session is confined to, `None` for the whole filesystem.
- `crates/kerness/src/access.rs:88` — `AccessPolicy::agent_workspaces` —
  per-agent workspaces, each of which narrows the session's.
- `crates/kerness/src/access.rs:138` — `AccessManager` — the policy plus the
  directories granted at runtime; owns every decision.
- `crates/kerness/src/access.rs:250` — `check_command(command, program, actor)` —
  program allow-list first, then whole-line patterns, then the prompt.
- `crates/kerness/src/access.rs:275` — `check_path(action, path, actor)` — the
  workspace first, then the allowlists; resolves the path and returns the
  canonical `PathBuf`, so the caller cannot re-resolve it differently afterwards.
- `crates/kerness/src/access.rs:227` — `check_workspace(purpose, path, actor)` —
  the workspace check alone, for a path the caller chose rather than a model
  asked for: the memory file, the session file, a channel's log.
- `crates/kerness/src/access.rs:185` — `workspace_for(actor)` — the workspace an
  actor is held to, and the directory its commands start in.
- `crates/kerness/src/access.rs:197` — `confine_agent(agent, workspace)` — narrow
  one agent, refusing a workspace outside the session's.
- `crates/kerness/src/access.rs:299` — `allow_dirs(paths)` — how a skill grants its
  bundle directory at activation time.
- `crates/kerness/src/access.rs:44` — `ApprovePrompt` — the trait a human-in-the-loop
  prompt implements; `None` means deny.
- `crates/kerness/src/exec.rs:30` — `run_command(...)` — the only place a
  subprocess is spawned, with `DEFAULT_TIMEOUT` at `exec.rs:18`.
- `bindings/python/kerness/access.py:21` — `prompt_on_console(req)` — deliberately Python:
  it calls `input()`, and tests monkeypatch the module attribute.

`AccessPolicy`'s derived `Default` and `AccessPolicy::new()` disagree on
`trust_skill_bundles`; the difference is documented at `access.rs:109`.

### Why an agent workspace narrows rather than replaces

Every other per-agent option in the framework simply overrides the session's:
an agent naming a model, a persona, or a provider gets the one it named. The
workspace is the one exception, and `confine_agent` (`access.rs:197`) is where
the exception is enforced — a workspace outside the session's is an
`AccessDenied` naming the agent, at `run()`, before a single provider call.

The reason is that override semantics turn a config file into a
privilege-escalation path. A session confined to `/srv/work` whose agent stanza
could name `/` would be confined to nothing, and the escalation would be spelled
in the same syntax as a legitimate narrowing. Intersection has no such reading:
whatever an agent stanza says, the session workspace is still an upper bound.

## Interactions

- Constructed by [session.md](session.md), which builds one manager per session
  and hands it to every tool.
- Guards [toolkit.md](toolkit.md)'s dispatch of the built-in `run_command`,
  `read_file`, and `list_dir` tools.
- Granted directories by [skills.md](skills.md) when a skill bundle activates and
  the policy trusts bundles.
- Raises `AccessDeniedError` through [errors.md](errors.md).

## How to Test

```sh
cargo test -p kerness access                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_access.py -q # pass = 0 failed
```

- `bindings/python/tests/test_access.py:150` — `test_traversal_out_of_an_allowed_dir_is_denied` —
  and `:161` `test_a_symlink_out_of_an_allowed_dir_is_denied`: the two escapes
  `check_path` exists to stop, tested at the layer that owns them.
- `:340` — `test_a_workspace_refuses_what_an_allowlist_and_an_approver_both_allow`
  — the whole point of a second mechanism: it refuses what the first one allowed.
  `:363` `test_traversal_cannot_step_out_of_the_workspace` is the same escape
  again, against the workspace this time.
- `:389` `test_the_default_policy_confines_nothing` — a workspace is opt-in;
  defaulting it to the working directory would confine every caller who never
  asked to be.
- `crates/kerness/tests/access_e2e.rs` — the workspace seen from a configured
  session: `a_workspace_confines_a_read_a_write_and_a_commands_working_directory`,
  `the_sessions_own_write_paths_are_confined_too` (a memory, session, or channel
  file outside the workspace fails at `Session::new`), and
  `an_agent_workspace_narrows_the_sessions_and_a_wider_one_names_the_agent`.
- `:259` `test_a_bare_policy_allows_nothing` and `:278`
  `test_a_manager_with_no_policy_at_all_still_denies` — the default is closed.
- `:69` `test_allowed_commands_is_exact_not_prefix` and `:119`
  `test_an_invalid_regex_is_skipped_not_raised` — how the command check refuses.
- `:308` `test_the_console_prompt_denies_when_stdin_cannot_answer` — the reason
  `prompt_on_console` stays Python.

## Open Gaps / Roadmap

- The command allow-list matches on the resolved program name and the literal
  command line. A shell metacharacter that changes which program runs is caught
  by the pattern check, not by parsing the line; callers who allow a shell should
  allow it by pattern, not by program.
- Per-actor policy is the workspace and nothing else. One manager serves every
  agent, keyed by actor for the workspace; the allowlists stay session-wide,
  because a per-agent allowlist under override semantics would let an agent widen
  its own reach — the same escalation the workspace's intersection rule exists to
  prevent. Outside the workspace, `actor` is carried through for the audit trail
  and the prompt text only.
