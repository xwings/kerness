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

## Status

`done` — implemented and tested, with direct tests for traversal, symlink
escape, and host spoofing rather than only for the happy path, against both the
workspace and the allowlists that reach past it.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/access.rs` | `AccessPolicy`, `AccessManager`, the checks |
| `crates/kerness/src/exec.rs` | the three tools the boundary exists for: run, read, list |
| `bindings/python/src/access.rs` | the pyclasses, the console the approver reads through, and reading a Python `AccessPolicy` |
| `bindings/python/kerness/access.py` | `AccessPolicy` as a dataclass, and the re-exports |

`AccessPolicy` is declared in Python (`bindings/python/kerness/access.py:22`)
rather than as a pyclass because callers build one with keyword arguments and
then mutate fields on it; `policy_from_py` (`bindings/python/src/access.rs:188`)
reads it into the Rust struct at the moment it is used, so a field assigned
after construction still takes effect.

## Key Types and Entry Points

- `crates/kerness/src/access.rs:186` — `AccessPolicy` — the declaration: the
  workspace, the two command allow-lists, the path allow-lists, the host list,
  and whether a skill bundle's own directory is trusted.
- `crates/kerness/src/access.rs:206` — `AccessPolicy::workspace` — the directory
  the session works in, `None` for the process's own current directory. It
  grants its contents rather than only bounding them.
- `crates/kerness/src/access.rs:217` — `AccessPolicy::agent_workspaces` —
  per-agent workspaces, each of which narrows the session's.
- `crates/kerness/src/access.rs:231` — `AccessPolicy::allowed_commands` —
  anchored globs over the whole command line; `"*"` allows every command, and a
  pattern with no `*` is exact.
- `crates/kerness/src/access.rs:271` — `AccessPolicy::allowed_hosts` — anchored
  globs over the hostname: `"example.com"` exactly, `"*.example.com"` for its
  subdomains but not itself, `"*"` for any.
- `crates/kerness/src/access.rs:312` — `AccessManager` — the policy plus the
  directories granted at runtime; owns every decision.
- `crates/kerness/src/access.rs:402` — `check_command(command, actor)` — the host
  list, then auto-approve prefixes, then globs, then whole-line patterns, then
  the prompt.
- `crates/kerness/src/access.rs:437` — `check_host(target, actor)` — one
  destination against `allowed_hosts`. Refuses outright rather than prompting,
  as `check_path` does.
- `crates/kerness/src/access.rs:472` — `check_path(purpose, path, actor)` — the
  workspace and the path allow-lists, and nothing else; resolves the path and
  returns the canonical `PathBuf`, so the caller cannot re-resolve it
  differently afterwards. *purpose* is a tool's action (`"read"`, `"list"`) or
  the framework's description of a file it writes for its own reasons
  (`"The memory file"`) — the same check either way.
- `crates/kerness/src/access.rs:364` — `workspace_for(actor)` — the workspace an
  actor is held to, and the directory its commands start in.
- `crates/kerness/src/access.rs:376` — `confine_agent(agent, workspace)` — narrow
  one agent, refusing a workspace outside the session's.
- `crates/kerness/src/access.rs:491` — `allow_dirs(paths)` — how a skill grants its
  bundle directory at activation time.
- `crates/kerness/src/access.rs:48` — `ApprovePrompt` — the trait a human-in-the-loop
  prompt implements; `None` means deny. Only ever asked about a command.
- `crates/kerness/src/access.rs:146` — `prompt_on_console(request)` — the one
  approver that ships; `:69` `ConsolePrompt` and `:120` `set_console_prompt` are
  the console it reads and writes through.
- `crates/kerness/src/exec.rs:30` — `run_command(...)` — the only place a
  subprocess is spawned, with `DEFAULT_TIMEOUT` at `exec.rs:18`. A command the
  splitter cannot turn into an argv is refused before the policy is consulted:
  unbalanced quoting, and — at `exec.rs:43` — a line that splits to no program
  at all, which a bare comment does.
- `bindings/python/src/access.rs:100` — `prompt_on_console(request)` — the same
  approver as a `#[pyfunction]`; `:114` `PyConsole` and `:159` `install_console`
  point it at `sys.stdin` and `sys.stdout`.

`AccessPolicy`'s derived `Default` and `AccessPolicy::new()` disagree on
`trust_skill_bundles`; the difference is documented at `access.rs:284`.

### The console approver, and the console under it

`prompt_on_console` is the framework's, not the binding's. The wording of the
question, the rule that an empty answer means yes, and the refusal to ask at all
when there is no terminal are all decisions a Rust-only harness needs too — and
`prompt_or_deny`'s denial names `prompt_on_console` as the way back in
(`access.rs:505`), and a Rust caller reading that has to be able to find it.

What differs across the boundary is the console, and `ConsolePrompt`
(`access.rs:69`) is the seam for it, alongside the two in
[channel.md](channel.md) and the one in [provider.md](provider.md). The default
is this process's stdin and stdout. The binding installs one over `sys.stdin`,
`sys.stdout`, and `builtins.input`, because those are not file descriptors 0 and
1: a caller who redirected a stream meant something by it, and a prompt reading
the descriptor would go straight past them.

Three methods rather than one, because they are three questions with three
answers. `is_interactive` is about input and `renders_colour` about output — a
run with a terminal on one and a file on the other needs both — and `ask` is the
round trip. Anything going wrong in either of the first two is a `false`: a
stream that cannot say whether it is a terminal is not one, and a console
question is not worth failing a run over.

### Why an agent workspace narrows rather than replaces

Every other per-agent option in the framework simply overrides the session's:
an agent naming a model, a persona, or a provider gets the one it named. The
workspace is the one exception, and `confine_agent` (`access.rs:376`) is where
the exception is enforced — a workspace outside the session's is an
`AccessDenied` naming the agent, at `run()`, before a single provider call.

The reason is that override semantics turn a config file into a
privilege-escalation path. A session confined to `/srv/work` whose agent stanza
could name `/` would be confined to nothing, and the escalation would be spelled
in the same syntax as a legitimate narrowing. Intersection has no such reading:
whatever an agent stanza says, the session workspace is still an upper bound.

### The host dimension, and why it parses

`check_command` (`access.rs:402`) runs `check_host` over every URL on the line
*before* consulting any of the three command mechanisms, so the host list
narrows even a command the auto-approve prefixes would have waved through. That
ordering is what confines a session running `agent-browser open <url>` to the
sites it was given: the prefix `agent-browser` still opens the tool, and the
host list decides where it may go.

`host_of` (`access.rs:608`) does its own parsing rather than matching the
pattern against the whole URL, because a URL is not the string it looks like.
`https://good.example@evil.test/` is a request to `evil.test`: userinfo runs to
the *last* `@`, so reading it any other way is exactly the bug the check exists
to stop. The port is read off too — a port is not part of the name being
allowed — by splitting at the last colon, which leaves a bracketed IPv6 literal
intact since what follows its own colons is never all digits. The scheme is not
read at all: a host is narrowed the same way over `https`, `git`, or `ws`.

`urls_in` (`access.rs:601`) cuts the line at the characters a shell word cannot
carry through and calls anything left holding a `://` a URL. Cutting on `&` and
`;` splits a query string as well as a command list, which costs nothing: the
authority is over before either can appear, and a mangled piece is judged as the
host it appears to name and refused rather than skipped.

The framework ships no fetch tool, and none is planned. Browser access is a
caller decision made through the command dimension — the bundled `agent-browser`
skill is the intended route — and this narrows that decision rather than
providing a second one.

## Interactions

- Constructed by [session.md](session.md), which builds one manager per session
  and hands it to every tool.
- Guards [toolkit.md](toolkit.md)'s dispatch of the built-in `run_command`,
  `read_file`, and `list_dir` tools.
- Granted directories by [skills.md](skills.md) when a skill bundle activates and
  the policy trusts bundles; a skill that shells out to reach the network is
  narrowed by `allowed_hosts` like any other command.
- Raises `AccessDeniedError` through [errors.md](errors.md).

## How to Test

```sh
cargo test -p kerness access                                       # pass = 0 failed
cargo test -p kerness exec::                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_access.py -q # pass = 0 failed
```

- `bindings/python/tests/test_access.py:179` — `test_traversal_out_of_an_allowed_dir_is_denied` —
  and `:190` `test_a_symlink_out_of_an_allowed_dir_is_denied`: the two escapes
  `check_path` exists to stop, tested at the layer that owns them.
- `:395` — `test_a_workspace_grants_its_contents_and_names_itself_when_it_refuses`
  — a read admitted with nothing allowlisted, and a refusal that names the
  workspace it fell outside. `:414`
  `test_an_allowlist_reaches_outside_the_workspace` is the other half: an entry
  past the workspace widens it rather than being refused by it. `:428`
  `test_traversal_cannot_step_out_of_the_workspace` is the escape above again,
  against the workspace this time.
- `:205` `test_a_yes_saying_approver_cannot_widen_the_path_boundary` — what
  makes the boundary hard: an approver that would say yes is never asked.
- `:450` `test_an_unset_workspace_is_the_current_directory` — not the whole
  filesystem; a policy that says nothing about paths confines to where the
  program was launched.
- `:138` `test_a_named_host_passes_and_an_unnamed_one_does_not` — the network
  dimension seen from Python. `allowed_hosts` is plain data the crate validates,
  so the binding is proven by one case: the list crossed and it is consulted.
- `crates/kerness/src/access.rs:925` —
  `a_command_naming_no_url_is_not_narrowed_by_the_host_list` — and `:967`
  `every_url_on_the_line_is_checked_not_only_the_first`: what `urls_in` does and
  does not see. Pattern anchoring (`:933`), userinfo spoofing and case (`:945`)
  are decided here too, and tested here only — restating them through a
  pass-through binding would be two suites drifting over one rule.
- `crates/kerness/tests/access_e2e.rs` — the workspace seen from a configured
  session: `a_workspace_confines_a_read_a_write_and_a_commands_working_directory`,
  `the_sessions_own_write_paths_are_confined_too` (a memory, session, or channel
  file outside the workspace fails at `Session::new`), and
  `an_agent_workspace_narrows_the_sessions_and_a_wider_one_names_the_agent`.
- `:292` `test_a_bare_policy_allows_nothing` and `:309`
  `test_a_manager_with_no_policy_at_all_still_denies` — the default is closed.
- `:76` `test_a_command_glob_is_anchored_at_both_ends`, `:87`
  `test_a_bare_star_allows_every_command`, and `:123`
  `test_an_invalid_regex_is_skipped_not_raised` — how the command check admits
  and refuses.
- `crates/kerness/src/access.rs:1115` —
  `the_console_prompt_takes_yes_an_empty_line_and_nothing_else` — and `:1135`
  `the_console_prompt_names_the_agent_and_denies_without_a_terminal`: what
  counts as approval, and what the question says, decided against a scripted
  `ConsolePrompt` with no terminal in sight.
- `:339` `test_the_console_prompt_denies_when_stdin_cannot_answer` and `:356`
  `test_the_console_prompt_reads_sys_stdin_and_not_the_descriptor` — the pair
  that proves the installed console: under pytest `sys.stdin` genuinely is not a
  terminal and the answer is no, and a replaced `sys.stdin` that says it is one
  is believed. Reading file descriptor 0 would get both backwards.

## Open Gaps / Roadmap

- Both command allow-lists match the literal command line. A shell
  metacharacter that changes which program runs is not caught by parsing the
  line, so a glob like `sh *` grants whatever that shell is handed; callers who
  allow a shell are allowing everything it can reach.
- `allowed_hosts` sees the URLs written on the command line and nothing else.
  `urls_in` (`access.rs:601`) recognises a `scheme://host` token, so
  `curl example.com` and a command that reads its destination from a file or an
  environment variable are not narrowed — whether they run at all remains
  `allowed_commands`' decision. Narrowing those would mean knowing each
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
  three command mechanisms — an unanchored `starts_with` at `access.rs:550`,
  where `allowed_commands` is an anchored glob — and `check_command` consults it
  first among them, at `access.rs:412`. The field a caller is least warned about
  is the one that admits the most.
- `check_path` returns the resolved path so the caller cannot re-resolve it
  differently, but `Session::new` discards all three of the ones it asks for
  (`crates/kerness/src/session.rs:564-576`) and keeps the raw string it was given
  (`session.rs:647`), which `save` later opens (`session.rs:1594`). No escape
  follows today, because the path was checked
  and a session is single-threaded, but the check's contract is documented at
  `access.rs:460-461` and the framework's own writer does not honour it.
- `realpath` (`crates/kerness/src/access.rs:698`) builds every resolved path
  from `/`, so path confinement assumes POSIX paths and the boundary is
  Linux/macOS. Noted in `ARCHITECTURE.md`'s Target Environment.
