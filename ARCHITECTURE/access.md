# Access

## Goal

The permission boundary. Every command a tool runs and every path a tool reads
is checked here first, against a policy the caller declares up front. Nothing
else in the framework decides whether an action is allowed — the tools in
`exec.rs` take an `&AccessManager` and cannot act without one. Serves **M2**.

The policy answers three questions: is this program on the allow-list, does the
whole command line match an allowed pattern, and does this path resolve inside a
directory the caller opened. When the answer is no, the manager may ask a human
via a caller-supplied prompt; if there is no prompt, the answer stands.

## Status

`done` — implemented and tested, with direct tests for traversal and symlink
escape rather than only for the happy path.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/access.rs` | `AccessPolicy`, `AccessManager`, the checks |
| `crates/kerness/src/exec.rs` | the three tools the boundary exists for: run, read, list |
| `bindings/python/src/access.rs` | the pyclasses, and reading a Python `AccessPolicy` |
| `bindings/python/kerness/access.py` | `AccessPolicy` as a dataclass, and the console prompt |

`AccessPolicy` is declared in Python (`bindings/python/kerness/access.py:83`)
rather than as a pyclass because callers build one with keyword arguments and
then mutate fields on it; `policy_from_py` (`bindings/python/src/access.rs:113`)
reads it into the Rust struct at the moment it is used, so a field assigned
after construction still takes effect.

## Key Types and Entry Points

- `crates/kerness/src/access.rs:63` — `AccessPolicy` — the declaration: allowed
  programs, allowed command patterns, allowed directories, and whether a skill
  bundle's own directory is trusted.
- `crates/kerness/src/access.rs:113` — `AccessManager` — the policy plus the
  directories granted at runtime; owns every decision.
- `crates/kerness/src/access.rs:150` — `check_command(command, program, actor)` —
  program allow-list first, then whole-line patterns, then the prompt.
- `crates/kerness/src/access.rs:172` — `check_path(action, path, actor)` — resolves
  the path and returns the canonical `PathBuf`, so the caller cannot re-resolve it
  differently afterwards.
- `crates/kerness/src/access.rs:195` — `allow_dirs(paths)` — how a skill grants its
  bundle directory at activation time.
- `crates/kerness/src/access.rs:42` — `ApprovePrompt` — the trait a human-in-the-loop
  prompt implements; `None` means deny.
- `crates/kerness/src/exec.rs:30` — `run_command(...)` — the only place a
  subprocess is spawned, with `DEFAULT_TIMEOUT` at `exec.rs:18`.
- `bindings/python/kerness/access.py:21` — `prompt_on_console(req)` — deliberately Python:
  it calls `input()`, and tests monkeypatch the module attribute.

`AccessPolicy`'s derived `Default` and `AccessPolicy::new()` disagree on
`trust_skill_bundles`; the difference is documented at `access.rs:86` and relied
on at `session.rs:348`.

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
- No per-actor policies: one manager serves every agent in a session, and `actor`
  is carried through for the audit trail and the prompt text only.
