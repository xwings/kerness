---
eatmycode_version: "2.0.0"
---

# Tools, skills and access

Owner: [Project architecture](../../ARCHITECTURE.md)

Read when: changing tool parsing/dispatch, JSON argument validation, command/file
policy, skill activation, trusted bundle grants or permission boundary tests.

## Responsibility and Status

Implemented: model calls become validated handler invocations; model-requested
file/command operations pass explicit policy checks. Skills disclose their body
per turn and affect the available tools. This is a library capability boundary,
not process isolation: arbitrary installed host handlers/providers are trusted.
Verification exercises direct denials and successful allowed paths.

## Code Map

| Path / symbol | Role |
| --- | --- |
| [access.rs](../../crates/kerness/src/access.rs), `AccessManager` | Command decisions, workspace/path resolution, host filtering and approval prompts. |
| [exec.rs](../../crates/kerness/src/exec.rs), `run_command` | Policy-checked argv execution, deadlines and file/list operations. |
| [tooling.rs](../../crates/kerness/src/tooling.rs), `ToolSpec`; [toolkit.rs](../../crates/kerness/src/toolkit.rs), `ToolDispatcher` | Text-call normalization, registered handlers and dynamic tool selection. |
| [jsonschema.rs](../../crates/kerness/src/jsonschema.rs); [skill/runtime.rs](../../crates/kerness/src/skill/runtime.rs), `SkillActivation` | Supported schema validation, turn-local disclosure and tool gates. |
| [access_e2e.rs](../../crates/kerness/tests/access_e2e.rs), [tools_e2e.rs](../../crates/kerness/tests/tools_e2e.rs), [skills_e2e.rs](../../crates/kerness/tests/skills_e2e.rs) | Policy, dispatcher and progressive-disclosure behavior through public sessions. |

## Local Conventions

[Root conventions](../../ARCHITECTURE.md#code-conventions) apply. Keep policy
decisions in `AccessManager` and execution in `exec`; built-in tools use the
same registered-handler path as custom tools. Unknown tools, invalid arguments
and handler errors become explicit error `ToolResult`s rather than disappearing.
Security changes require direct negative boundary tests, including traversal,
symlinks, approval scope and expired handles. Unix-only unsafe process operations
in `exec.rs` require preserving their documented size/lifetime assumptions.

## Contracts and Invariants

- A workspace grants its resolved contents; unset uses the process working
  directory. Explicit allowed files/directories extend that grant. An agent's
  workspace only narrows the session's; symlink/traversal resolution occurs
  before checking confinement. Path denials cannot be overridden by approval
  (`access.rs`, `access_e2e.rs`).
- Unlisted commands are denied unless an installed approver allows them.
  Anchored globs, regex patterns and prefixes have distinct matching semantics;
  do not silently interchange them. An allowed command still faces configured
  host restrictions. `allowed_hosts` inspects explicit command URLs; it is not
  a firewall and does not govern provider HTTP.
- `AccessPolicy::new()` enables trusted skill-bundle grants while derived
  `Default` does not; `Session` intentionally uses `new()` when no policy is
  supplied (`access.rs`, `session.rs`). Automatic grants apply only to built-in
  skill bundles; loading an arbitrary path skill does not grant its files
  (`SkillActivation` in `skill/runtime.rs`).
- Commands are split with `shell_words` and launched via `Command` without an
  implicit shell, with null stdin and captured stdout/stderr. Unix process groups
  and pipe draining support deadlines/cancellation. Nonzero exit includes stderr;
  `DEFAULT_TIMEOUT` is the default, not a promise about arbitrary host callbacks.
- Dispatch validates arguments against the offered schema and resolves allowed
  specs on each call. JSON-schema support is the implemented subset, not a claim
  of complete standards support (`jsonschema.rs`, `ToolDispatcher::execute`).
- Skill bodies enter the current agent's tool-result scratch, not the system
  prompt. Repeat loads within a turn do not repeat the body; new turns start fresh.
  Active skills union their allowed-tool gates; registered `requires-tools` may
  add tools past harness/other skill restrictions. This never invents a handler
  or bypasses the handler's access checks (`skill/runtime.rs`, `skills_e2e.rs`).
- Contextual handler identity/leases are [runtime-owned](runtime.md): capability
  use after return fails, and command preflight grants one frozen command/cwd
  once. Session memory/snapshot/channel file destinations are also checked at
  setup, before model tools can run (`session.rs`, `access_e2e.rs`).

## Dependencies and Boundaries

Read [runtime](runtime.md) for tool registration, approval/checkpoint state,
contextual leases or any change to `session/capabilities.rs`. Read
[providers](providers.md) for native schemas, response parsing or dialect fallback;
it owns `toolschema.rs`. Read [harness/assets](harness-assets.md) for skill metadata
or lookup changes and [memory/persistence](memory-persistence.md) for memory
filter/write permissions. Read [Python bindings](python-bindings.md) for callback,
policy dataclass or schema API changes. Access cannot confine IO a custom handler
performs outside supplied capabilities.

## Change Guide

| Change trigger | Inspect / extend | Required docs / checks |
| --- | --- | --- |
| Path, command or host allowance | Access resolver/matcher and exec; `access_e2e.rs` | Update this owner; read runtime for preflight/approval semantics; negative and permitted cases. |
| Tool parse, schema or dispatch | `tooling.rs`, `jsonschema.rs`, `toolkit.rs`; `tools_e2e.rs` | Read providers if wire shapes change; preserve error feedback and matching offered/executable tools. |
| Disclosure or active gate | `skill/runtime.rs`; `skills_e2e.rs` | Read harness/assets for metadata and runtime for turn reset; check same-turn repeat and later-turn isolation. |

## Verification

Use the [root Rust suite](../../ARCHITECTURE.md#verification): inline access/exec,
tool/schema/skill tests plus `access_e2e`, `tools_e2e`, `skills_e2e` cover the
invariants above. `session_run` covers contextual identity and lease expiry.
Run the [Python suite](python-bindings.md#verification) for `test_access.py`,
`test_tooling.py`, `test_toolkit.py`, `test_jsonschema.py`, `test_skill_runtime.py`
and contextual cases in `test_session.py`. These are local tests, not live
provider tests. See [baseline evidence](../topics/build-checks.md#evidence-and-gaps).

## Known Gaps

POSIX path/process assumptions exclude a demonstrated Windows security contract.
Host filtering only examines command text. Arbitrary handlers and spawned
programs can perform their own IO; library path checks do not provide an OS
sandbox. Cancellation/timeout behavior of custom callbacks remains host-owned.
