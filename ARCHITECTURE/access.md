---
eatmycode_version: "1.1.0"
---

# Access

## Goal

The permission boundary. Every command a tool runs and every path a tool reads
is checked here first, against a policy the caller declares up front. Nothing
else in the framework decides whether an action is allowed — the tools in
`exec.rs` take an `&AccessManager` and cannot act without one.

There are three dimensions, and the module's organising idea is that they differ
in who answers.

**A command is asked about.** `auto_approve_prefixes`, `allowed_commands`, and
`allowed_command_patterns` each name something the caller opened; a command
matching none of them goes to the approver, and a session with no approver
refuses it.

**A path is settled.** The workspace grants its own contents — every path under
it is reachable with no allowlist entry — and `allowed_files` and `allowed_dirs`
reach past it, which is how a session confined to one project still reads
`/tmp`. The union of the two is the whole of what a session can touch: a path
outside it is refused outright, with no approver consulted, because *may I* has
no answer out there. Unset, the workspace is the process's own current
directory rather than the whole filesystem. It is also the working directory a
command starts in, so a confined session's commands are *in* the confinement
rather than merely unable to name their way out of it.

**A host only narrows.** `allowed_hosts` is the one allowlist here that is
empty-means-open. The framework ships no tool that reaches the network, so a
caller who registered one — or who activated the `agent-browser` skill — has
already made that decision through the command dimension; the host list takes
URLs back off a command that was otherwise allowed, and adds nothing when it is
empty.

This module does not own *when* a check runs, who the actor is, or what a
refusal becomes: [session.md](session.md) decides the first, [run.md](run.md)
assigns the second, and [toolkit.md](toolkit.md) turns the third into a tool
result. No milestone is attached; the boundary is infrastructure every
milestone consumes.

## Status

`done`. `cargo test -p kerness access` passes 34 tests and
`cargo test -p kerness exec::` passes 11; `cargo test -p kerness --test
access_e2e` passes 17 sessions through the boundary;
`.venv/bin/python -m pytest bindings/python/tests/test_access.py -q` passes 40.
Traversal, symlink escape, host spoofing, and the console approver are each
tested directly at this layer rather than only through a session.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/access.rs` | `AccessPolicy`, `AccessManager`, the three checks, the console-prompt seam, and path resolution |
| `crates/kerness/src/exec.rs` | the three tools the boundary exists for — `run_command`, `read_file`, `list_dir` — and the POSIX process-group deadline |
| `bindings/python/src/access.rs` | `PyAccessRequest`, `PyAccessManager`, the console the approver reads through, and `policy_from_py` |
| `bindings/python/kerness/access.py` | `AccessPolicy` as a dataclass, the `ApprovePrompt` alias, and the re-exports |

## Language and Conventions

Rust crate modules (`access.rs`, `exec.rs`), one PyO3 binding module, and one
Python file that is deliberately not a shim. The root's
[Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply. Local facts:

- **`AccessPolicy` is a Python dataclass** (`bindings/python/kerness/access.py:21`)
  rather than a pyclass, because its contract is written in Python list
  semantics: a caller builds one with keyword arguments, hands it to a manager,
  and may append to a list afterwards — and the manager must *not* see that,
  because `policy_from_py` (`bindings/python/src/access.rs:188`) snapshots the
  object at construction. `test_mutating_allowed_commands_after_construction_has_no_effect`
  (`bindings/python/tests/test_access.py:280`) pins the snapshot. Every
  decision stays in Rust.
- **The only `unsafe` in the crate** is here: `crates/kerness/src/exec.rs:159`,
  `:163` (`fcntl` descriptor flags) and `:200` (`kill` on a negative PID), each
  under a `// SAFETY:` comment and a `#[cfg(unix)]` guard. `libc` supplies only
  those two operations; spawning and reads use the standard library.
- **Platform split by `cfg`.** `capture_output` has a POSIX body at
  `crates/kerness/src/exec.rs:102` and a thread-per-pipe fallback at `:214`;
  `process_group(0)` at `:74` is unix-only. The unit test at `:364` carries its
  own `#[cfg(unix)]` arms.
- **Global seam.** `ConsolePrompt` (`crates/kerness/src/access.rs:69`) lives in
  an `OnceLock<RwLock<Arc<dyn ConsolePrompt>>>` slot (`:114`), the same shape as
  the other three seams the root table lists. Lock poisoning is
  `expect("console prompt lock poisoned")`, the crate-wide idiom.
- **Regex engine.** Command patterns compile with `fancy-regex`
  (`crates/kerness/src/access.rs:14`), not `regex`, because a caller's pattern
  may use lookaround; an uncompilable pattern is dropped, not raised (`:656`).
- **Tests.** Unit tests sit inline under `#[cfg(test)]` and use
  `crate::testing::TempDir` (`crates/kerness/src/access.rs:757`,
  `crates/kerness/src/exec.rs:298`). Tests that install a console take turns on
  a static mutex through `with_console` (`crates/kerness/src/access.rs:1125`),
  because the slot is process-wide and the unit suite runs in one process. The
  Python suite builds managers through `denying(**kwargs)`
  (`bindings/python/tests/test_access.py:27`), an approver that always says no,
  so only allowlists can admit. Both suites use sentence-style names.

## Design and Invariants

### Decomposition and dependency direction

`access.rs` depends on `error` and `pyfmt` only; `exec.rs` depends on `access`
and `error`. Nothing below the session references either. Callers are
[session.md](session.md) (`Session::new` builds the manager and checks the
session's own write paths, `crates/kerness/src/session.rs:563`), the owned run
(`command_approval` at `crates/kerness/src/session/run.rs:990`), scoped tool
contexts (`crates/kerness/src/session/capabilities.rs:209`), and the skill
runtime through the `grant_paths` closure (`crates/kerness/src/session.rs:609`).

`AccessPolicy` is the declaration and `AccessManager` is the evaluator.
`AccessManager::new` (`crates/kerness/src/access.rs:336`) resolves every path,
normalises every list, lowercases hosts, and compiles patterns once; a manager
is therefore a snapshot, and `Session::set_exec`
(`crates/kerness/src/session.rs:685`) applies a change by rebuilding one.
`allow_dirs` (`crates/kerness/src/access.rs:506`) is the one live widening
path, and it writes back to the policy so a rebuilt manager keeps the grant
(`allow_dirs_widens_a_live_manager_and_the_policy_behind_it`, `:1210`).

### Invariants a change must preserve

- **Default deny, and the refusal names the way back in.** A bare policy
  allows nothing; an unlisted command with no approver is an `AccessDenied`
  that names `approve_prompt=prompt_on_console` (`prompt_or_deny`,
  `crates/kerness/src/access.rs:517`). Tests:
  `no_prompt_at_all_still_denies_and_names_the_way_back_in` (`:793`),
  `a_bare_policy_allows_nothing_but_trusts_skill_bundles` (`:1410`),
  `test_a_manager_with_no_policy_at_all_still_denies`
  (`bindings/python/tests/test_access.py:309`).
- **Hosts before mechanisms.** `command_approval`
  (`crates/kerness/src/access.rs:417`) runs `check_host` over every URL on the
  line *before* the prefixes, globs, or patterns, so the host list narrows even
  a command a prefix would have waved through — what confines
  `agent-browser open <url>` to the sites it was given. Tests:
  `a_host_list_narrows_an_auto_approved_prefix_too` (`:925`),
  `every_url_on_the_line_is_checked_not_only_the_first` (`:982`).
- **A host is parsed, not pattern-matched.** `host_of` (`crates/kerness/src/access.rs:623`) reads userinfo
  to the *last* `@`, strips a numeric port at the last colon (leaving a bracketed
  IPv6 literal intact), and ignores the scheme; `urls_in` (`:616`) cuts the line
  at the characters a shell word cannot carry and calls anything holding `://` a
  URL. `https://good.example@evil.test/` is a request to `evil.test`. Tests:
  `the_host_is_read_past_userinfo_a_port_and_a_path` (`:960`),
  `a_host_pattern_is_an_anchored_glob_over_the_hostname` (`:948`).
- **Paths are settled, never asked about.** `check_path` (`crates/kerness/src/access.rs:487`) consults the
  workspace and the two path allowlists and no approver; an approver that would
  say yes is never reached. Test:
  `a_yes_saying_approver_cannot_widen_the_path_boundary` (`:1355`) and its
  Python twin (`bindings/python/tests/test_access.py:205`).
- **Resolution before judgement, and the resolved path is what is returned.**
  `resolve_path` (`crates/kerness/src/access.rs:675`) expands `~`, absolutises, then `realpath` (`:713`)
  walks components from `/`, follows symlinks within a 40-link budget, and
  collapses `..` — non-strictly, so a file the agent wants to *create* is judged
  too. `check_path` returns that `PathBuf`, so the caller cannot re-resolve
  differently afterwards; `exec::read_file` (`crates/kerness/src/exec.rs:264`)
  opens exactly what was returned. Tests:
  `traversal_out_of_an_allowed_dir_is_denied` (`crates/kerness/src/access.rs:1031`),
  `a_symlink_out_of_an_allowed_dir_is_denied` (`:1045`),
  `traversal_and_symlinks_cannot_step_out_of_the_root` (`:1259`).
- **An agent workspace narrows and never widens.** Every other per-agent option
  overrides the session's; `confine_agent` (`crates/kerness/src/access.rs:382`) refuses a workspace outside
  the session's with an `AccessDenied` naming the agent, applied at
  `crates/kerness/src/session.rs:1540` before the first provider call.
  Override semantics would turn a config stanza into a privilege-escalation
  path spelled in the same syntax as a legitimate narrowing. Tests:
  `an_agent_workspace_narrows_the_sessions_and_cannot_widen_it`
  (`crates/kerness/src/access.rs:1373`),
  `an_agent_workspace_narrows_the_sessions_and_a_wider_one_names_the_agent`
  (`crates/kerness/tests/access_e2e.rs:495`).
- **The session's own files go through the same check.** The memory file, the
  session file, and every channel path are checked at `Session::new`
  (`crates/kerness/src/session.rs:577`, `:580`, `:583`) with the framework's
  description as *purpose*, so a misplaced path fails at construction rather
  than mid-turn. Tests: `the_sessions_own_files_are_confined_by_the_same_check`
  (`crates/kerness/src/access.rs:1281`),
  `the_sessions_own_write_paths_are_confined_too`
  (`crates/kerness/tests/access_e2e.rs:448`).
- **Globs are anchored; patterns search.** `glob_matches` (`crates/kerness/src/access.rs:579`) pins both
  ends and offers only `*`, so `git *` cannot admit `sudo git push`;
  `matches_regex` (`:642`) is a search, the looser mechanism, and a literal `"*"`
  is special-cased to allow all (`:656`). Tests:
  `a_command_glob_is_anchored_at_both_ends` (`:836`),
  `a_command_pattern_searches_rather_than_matches` (`:871`).
- **No shell.** `run_command` splits the line with `shell-words` and hands the
  argv to the OS (`crates/kerness/src/exec.rs:43`); metacharacters are
  arguments. Unbalanced quoting and a line that splits to no program are refused
  before the policy is consulted. Tests:
  `there_is_no_shell_so_metacharacters_are_arguments` (`:331`),
  `unbalanced_quoting_is_refused_before_the_policy_check` (`:340`),
  `a_command_that_splits_to_no_program_is_refused_rather_than_panicking` (`:350`).
- **A deadline holds against a full pipe and a lingering descendant.** On POSIX
  each command starts in its own process group; `capture_output` (`crates/kerness/src/exec.rs:103`) drains
  both pipes with bounded nonblocking reads and checks the deadline on every
  pass. Success needs the child exited *and* both pipes at EOF. Timeout,
  cancellation, or a pipe error runs `stop_group` (`:197`): `SIGKILL` to the
  group, then the direct child, then a reap. The leader stays unreaped while its
  pipes are open so its PID cannot be reused as another group's ID before
  cleanup signals it. Tests: `a_command_that_overruns_its_deadline_is_killed`
  (`:364`), `output_larger_than_a_pipe_buffer_still_completes` (`:435`).
- **`AccessPolicy::new()` and `Default` disagree on `trust_skill_bundles`.**
  `new()` (`crates/kerness/src/access.rs:286`) sets it true; the derived
  `Default` leaves it false. `Session::new` uses `new()`
  (`crates/kerness/src/session.rs:557`) so a session that named no policy still
  grants bundle paths. No test asserts the `Default` side directly; the
  Python default is pinned by `test_skill_bundles_are_trusted_by_default`
  (`bindings/python/tests/test_access.py:306`).
- **Approval is one grant for one exact request.** Under external approval a
  scoped context replaces the approver with one that answers yes once, for the
  frozen command and directory only
  (`crates/kerness/src/session/capabilities.rs:209`); `command_approval`
  returns the request without prompting so [run.md](run.md) can suspend before
  any side effect. Tests:
  `command_preflight_checks_hard_denials_and_grants_only_the_frozen_command_once`
  (`crates/kerness/tests/access_e2e.rs:752`),
  `an_approval_authorizes_one_request_and_not_the_next`
  (`crates/kerness/src/access.rs:1060`).

### The console approver and the console under it

`prompt_on_console` (`crates/kerness/src/access.rs:146`) is the framework's, not
the binding's: the wording of the question, the rule that an empty answer means
yes, and the refusal to ask at all when there is no terminal are decisions a
Rust-only harness needs too. What differs across the boundary is the console,
and `ConsolePrompt` (`:69`) is the seam — three methods because they are three
questions: `is_interactive` is about input, `renders_colour` about output, and
`ask` is the round trip. Anything going wrong in the first two is `false`: a
stream that cannot say whether it is a terminal is not one, and a console
question is not worth failing a run over. The binding's `PyConsole`
(`bindings/python/src/access.rs:114`) answers from `sys.stdin`, `sys.stdout`, and
`builtins.input` rather than descriptors 0 and 1, because a caller who
redirected a stream meant something by it.

### Extension points

- A custom approver is any `Fn(&AccessRequest) -> bool` (`ApprovePrompt`,
  `crates/kerness/src/access.rs:48`) or, from Python, any callable; a raising
  Python approver is a denial (`PyApprove`, `bindings/python/src/access.rs:166`).
- A different console is a `ConsolePrompt` installed through
  `set_console_prompt` (`crates/kerness/src/access.rs:120`); the binding installs
  its own at import (`install_console`, `bindings/python/src/access.rs:159`).
- A new tool that touches the filesystem or spawns a process takes an
  `&AccessManager` and calls `check_path` or `run_command`; nothing else grants.

## Key Types and Entry Points

- `crates/kerness/src/access.rs:186` — `AccessPolicy` — the declaration: the
  approver, `auto_approve_prefixes`, `workspace`, `agent_workspaces`, the two
  command allowlists, the two path allowlists, `allowed_hosts`, and
  `trust_skill_bundles`. `new()` at `:286` is the constructor sessions use.
- `crates/kerness/src/access.rs:313` — `AccessManager` — the evaluator: a
  resolved, normalised snapshot of a policy plus the directories granted at
  runtime; owns every decision. `new(policy)` at `:336`, `policy()` at `:358`.
- `crates/kerness/src/access.rs:417` — `command_approval(command, actor)` —
  empty-command denial, then every URL against the host list, then the three
  mechanisms; returns `Ok(None)` for an admitted command, `Ok(Some(request))`
  for one that needs an approver, `Err(AccessDenied)` for a hard refusal. Never
  calls a callback. `check_command` at `:408` is the synchronous form that then
  prompts or denies.
- `crates/kerness/src/access.rs:452` — `check_host(target, actor)` — a URL or
  bare hostname against `allowed_hosts`; empty list means allowed; refuses
  outright naming the host, the list, and the actor.
- `crates/kerness/src/access.rs:487` — `check_path(purpose, path, actor)` —
  resolves, then admits inside the actor's workspace or any path allowlist;
  returns the resolved `PathBuf` or an `AccessDenied` naming where the path
  landed and the workspace it fell outside. *purpose* is a tool's action or the
  framework's own description of a file it writes.
- `crates/kerness/src/access.rs:370` — `workspace_for(actor)` / `:382`
  `confine_agent(agent, workspace)` — the workspace an actor is held to and its
  commands start in; narrowing refused outside the session's.
- `crates/kerness/src/access.rs:506` — `allow_dirs(paths)` — the mid-session
  read grant a trusted skill bundle receives; updates the policy too.
- `crates/kerness/src/access.rs:146` — `prompt_on_console(request)` — the one
  shipped approver; denies off a terminal, treats an empty answer as yes, reads
  through the `ConsolePrompt` slot (`:69`, `:120`).
- `crates/kerness/src/exec.rs:33` — `run_command(access, command, cwd, timeout,
  actor)` — argv split, policy check, spawn in its own group, bounded capture;
  returns stdout, or a session error carrying the exit code and stderr, or
  `Command timed out`. `DEFAULT_TIMEOUT` at `:21` is 60 s. `read_file` at `:264`
  and `list_dir` at `:271` are the two path tools.
- `bindings/python/src/access.rs:247` — `PyAccessManager` — `check_command`
  (`:279`), `check_host` (`:288`), `check_path` (`:294`), `allow_dirs` (`:310`),
  each forwarding and raising; holds the caller's policy object so `allow_dirs`
  appends to the list a rebuilt manager will read. `policy_from_py` at `:188` is
  the snapshot.

## Interactions

- [session.md](session.md) builds one manager per session
  (`crates/kerness/src/session.rs:563`), checks its own write paths through it,
  rebuilds it in `set_exec`, confines agents at run start, and hands it to every
  built-in tool. Shared state: `Shared.access: Arc<Mutex<AccessManager>>`.
  Tested by `crates/kerness/tests/access_e2e.rs` end to end.
- [run.md](run.md) calls `command_approval` in preflight
  (`crates/kerness/src/session/run.rs:990`) and a scoped `ToolContext` runs
  commands through `run_command_cancellable` with a one-shot approver
  (`crates/kerness/src/session/capabilities.rs:209`). Contract: a returned
  `AccessRequest` must be answered before execution. Tested by
  `crates/kerness/tests/access_e2e.rs:588` and `:752`.
- [toolkit.md](toolkit.md)'s dispatcher turns an `AccessDenied` from
  `run_command`, `read_file`, or `list_dir` into an error tool result rather
  than a failed session. Tested by
  `a_denied_tool_call_becomes_a_tool_result_and_a_channel_notice`
  (`crates/kerness/tests/access_e2e.rs:345`) and
  `TestADenialCostsATurnNotTheSession`
  (`bindings/python/tests/test_session.py:511`).
- [skills.md](skills.md) grants bundle directories through `allow_dirs` only
  when `trust_skill_bundles` holds (`crates/kerness/src/session.rs:606`); a
  skill that shells out is narrowed by `allowed_hosts` like any other command.
- [errors.md](errors.md) carries the refusal as `Error::AccessDenied`, which
  crosses to Python as `AccessDeniedError`.
- [bindings.md](bindings.md) installs the console seam at `bootstrap`.

## How to Test

```sh
cargo test -p kerness access                                       # pass = 34 passed, 0 failed
cargo test -p kerness exec::                                       # pass = 11 passed, 0 failed
cargo test -p kerness --test access_e2e                            # pass = 17 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_access.py -q # pass = 40 passed
```

- `crates/kerness/src/exec.rs:364` —
  `a_command_that_overruns_its_deadline_is_killed` covers a direct child, a
  parent waiting for its child, and an exited parent whose child retains both
  pipes, plus a leader that moved groups; each must report timeout within a
  bounded elapsed time. `:435` `output_larger_than_a_pipe_buffer_still_completes`
  checks exact stdout while large stdout and stderr streams are produced together.
- `bindings/python/tests/test_access.py:179` — `test_traversal_out_of_an_allowed_dir_is_denied` —
  and `:190` `test_a_symlink_out_of_an_allowed_dir_is_denied`: the two escapes
  `check_path` exists to stop, tested at the layer that owns them.
- `bindings/python/tests/test_access.py:395` — `test_a_workspace_grants_its_contents_and_names_itself_when_it_refuses`
  — a read admitted with nothing allowlisted, and a refusal that names the
  workspace it fell outside. `:414`
  `test_an_allowlist_reaches_outside_the_workspace` is the other half: an entry
  past the workspace widens it rather than being refused by it. `:428`
  `test_traversal_cannot_step_out_of_the_workspace` is the escape above again,
  against the workspace this time.
- `bindings/python/tests/test_access.py:205` `test_a_yes_saying_approver_cannot_widen_the_path_boundary` — what
  makes the boundary hard: an approver that would say yes is never asked.
- `bindings/python/tests/test_access.py:450` `test_an_unset_workspace_is_the_current_directory` — not the whole
  filesystem; a policy that says nothing about paths confines to where the
  program was launched.
- `bindings/python/tests/test_access.py:138` `test_a_named_host_passes_and_an_unnamed_one_does_not` — the network
  dimension seen from Python. `allowed_hosts` is plain data the crate validates,
  so the binding is proven by one case: the list crossed and it is consulted.
- `crates/kerness/src/access.rs:940` —
  `a_command_naming_no_url_is_not_narrowed_by_the_host_list` — and `:982`
  `every_url_on_the_line_is_checked_not_only_the_first`: what `urls_in` does and
  does not see. Pattern anchoring (`:948`), userinfo spoofing and case (`:960`)
  are decided here and tested here only — restating them through a pass-through
  binding would be two suites drifting over one rule.
- `crates/kerness/tests/access_e2e.rs:405` —
  `a_workspace_confines_a_read_a_write_and_a_commands_working_directory`, `:448`
  `the_sessions_own_write_paths_are_confined_too` (a memory, session, or channel
  file outside the workspace fails at `Session::new`), and `:495`
  `an_agent_workspace_narrows_the_sessions_and_a_wider_one_names_the_agent`: the
  workspace seen from a configured session. `:180`
  `set_exec_replaces_the_patterns_and_takes_effect_at_once` proves the rebuild.
- `bindings/python/tests/test_access.py:292` `test_a_bare_policy_allows_nothing` and `:309`
  `test_a_manager_with_no_policy_at_all_still_denies` — the default is closed.
- `bindings/python/tests/test_access.py:76` `test_a_command_glob_is_anchored_at_both_ends`, `:87`
  `test_a_bare_star_allows_every_command`, and `:123`
  `test_an_invalid_regex_is_skipped_not_raised` — how the command check admits
  and refuses.
- `crates/kerness/src/access.rs:1136` —
  `the_console_prompt_takes_yes_an_empty_line_and_nothing_else` — and `:1156`
  `the_console_prompt_names_the_agent_and_denies_without_a_terminal`: what
  counts as approval, and what the question says, decided against a scripted
  `ConsolePrompt` with no terminal in sight.
- `bindings/python/tests/test_access.py:339` `test_the_console_prompt_denies_when_stdin_cannot_answer` and `:356`
  `test_the_console_prompt_reads_sys_stdin_and_not_the_descriptor` — the pair
  that proves the installed console: under pytest `sys.stdin` genuinely is not a
  terminal and the answer is no, and a replaced `sys.stdin` that says it is one
  is believed. Reading file descriptor 0 would get both backwards.
- Gap: the `#[cfg(not(unix))]` `capture_output` body
  (`crates/kerness/src/exec.rs:214`) is never compiled by CI, which runs on Linux
  only; its direct-child timeout semantics are untested.

## Review and Refactor Guide

- **Changing a check's order or a refusal message** → inspect
  `command_approval`, `check_host`, `check_path`, `prompt_or_deny`
  (`crates/kerness/src/access.rs:417`, `:452`, `:487`, `:517`); dependent
  modules [run.md](run.md) (preflight reads the returned request) and
  [toolkit.md](toolkit.md) (the message reaches the model as a tool result);
  tests `no_prompt_at_all_still_denies_and_names_the_way_back_in` (`:793`) and
  `test_the_denial_names_the_way_back_in`
  (`bindings/python/tests/test_access.py:330`) assert message content.
- **Changing path resolution** → `resolve_path`, `expanduser`, `realpath`,
  `components` (`crates/kerness/src/access.rs:675`, `:692`, `:713`, `:744`);
  the traversal and symlink tests at `:1031`, `:1045`, `:1259` and their
  `crates/kerness/tests/access_e2e.rs:223`, `:245` twins must still pass;
  `MAX_LINK_DEPTH` (`crates/kerness/src/access.rs:711`) is the loop budget.
- **Adding a policy field** → `AccessPolicy` (`crates/kerness/src/access.rs:186`), its hand-written `Debug`
  (`:294`), `AccessManager::new` (`:336`), the Python dataclass
  (`bindings/python/kerness/access.py:22`), and `policy_from_py`
  (`bindings/python/src/access.rs:188`) — five places, all in the same change.
  Keep the field's doc comment on both surfaces.
- **Changing command execution** → `run_command_cancellable`, `capture_output`,
  `stop_group` (`crates/kerness/src/exec.rs:43`, `:103`, `:197`); both `cfg`
  arms; the scoped-context caller at
  `crates/kerness/src/session/capabilities.rs:240`; the deadline tests at
  `crates/kerness/src/exec.rs:364` and `:435`.
- **Safe extension points**: a custom `ApprovePrompt`, a custom `ConsolePrompt`,
  and a new `exec`-style tool that takes `&AccessManager`. Reuse `check_path`'s
  returned path rather than re-resolving.
- **Forbidden coupling**: nothing below `session` may hold or construct an
  `AccessManager`; no tool may open a path it did not receive from `check_path`;
  no per-agent allowlist — the workspace is the only per-actor dimension (see
  Open Gaps for why).
- **Compatibility checks**: `AccessPolicy`'s Python constructor is keyword-only
  with the field order at `bindings/python/kerness/access.py:35`; `AccessRequest`
  is `frozen, get_all` with four string fields (`bindings/python/src/access.rs:27`);
  `DEFAULT_TIMEOUT` is asserted by `crates/kerness/tests/public_api.rs` and the
  root's constants table.

### Improvement candidates (proposals, not accepted work)

- Give `auto_approve_prefixes` a doc comment on both surfaces; success: the
  field carries a `///` at `crates/kerness/src/access.rs:188` and a `#:` at
  `bindings/python/kerness/access.py:36`, and `cargo doc` renders it.
- Confine the session's own write paths by the *resolved* path `check_path`
  returned rather than the configured string kept at
  `crates/kerness/src/session.rs:576`; success: a parent directory replaced by a
  symlink after `Session::new` is refused at write time by a new
  `access_e2e.rs` case.

## Open Gaps / Roadmap

- Process-group cleanup covers descendants that remain in the command's group.
  A daemon that deliberately creates another session or group can survive that
  signal; its inherited pipes still cannot delay the caller past the deadline.
  Non-POSIX builds retain direct-child timeout behavior and do not provide the
  Linux/macOS process-group guarantee.
- Both command allow-lists match the literal command line. A shell
  metacharacter that changes which program runs is not caught by parsing the
  line, so a glob like `sh *` grants whatever that shell is handed; callers who
  allow a shell are allowing everything it can reach.
- `allowed_hosts` sees the URLs written on the command line and nothing else.
  `urls_in` (`crates/kerness/src/access.rs:616`) recognises a `scheme://host`
  token, so `curl example.com` and a command that reads its destination from a
  file or an environment variable are not narrowed — whether they run at all
  remains `allowed_commands`' decision. Narrowing those would mean knowing each
  program's argument grammar, which is per-program knowledge the framework does
  not have.
- Per-actor policy is the workspace and nothing else. One manager serves every
  agent, keyed by actor for the workspace; the allowlists stay session-wide,
  because a per-agent allowlist under override semantics would let an agent widen
  its own reach — the same escalation the workspace's intersection rule exists to
  prevent. Outside the workspace, `actor` is carried through for the audit trail
  and the prompt text only.
- `auto_approve_prefixes` carries no doc comment on either surface
  (`crates/kerness/src/access.rs:188`, `bindings/python/kerness/access.py:36`),
  while every field beside it carries several lines. It is the loosest of the
  three command mechanisms — an unanchored `starts_with` at
  `crates/kerness/src/access.rs:565`, where `allowed_commands` is an anchored
  glob — and `command_approval` consults it first among them, at `:427`. The
  field a caller is least warned about is the one that admits the most.
- `check_path` returns a resolved path, but `Session::new` keeps the original
  configured write paths (`crates/kerness/src/session.rs:576`) and `save`
  opens its path later (`crates/kerness/src/session.rs:1680`). Changes to parent
  directories after validation are not confined by an OS sandbox. Snapshot
  temporary-file creation is guarded separately by
  [sessionfile.md](sessionfile.md).
- `realpath` (`crates/kerness/src/access.rs:713`) builds every resolved path
  from `/`, so path confinement assumes POSIX paths and the boundary is
  Linux/macOS. Noted in the root's
  [Mission and Constraints](../ARCHITECTURE.md#mission-and-constraints).
