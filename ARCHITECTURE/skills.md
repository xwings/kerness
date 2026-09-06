---
eatmycode_version: "1.1.0"
---

# Skills

## Goal

A skill is a directory holding a `SKILL.md` and optionally `scripts/` and
`references/`. Loading one gives an agent a body of instructions it can pull in
on demand through the `Skill` tool, and — when the access policy trusts bundles —
read access to the skill's own directory. A skill also says which tools the turn
should hold: `allowed-tools:` narrows it to a subset, and `requires-tools:` adds
back what the skill cannot work without.

This module owns reading a skill off disk, the per-turn activation state, the
`Skill` tool, and the two tool-list transforms. It does not own which agent has
which skills (the session's cache), whether a bundle grant is honoured (the
access policy), or when a required tool that nobody registered is refused (the
session, before the first provider call).

## Status

`done` — `cargo test -p kerness skill` passes 45 tests,
`cargo test -p kerness --test skills_e2e` passes 13, and the two Python modules
pass 10 and 17.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/skill/loader.rs` | `SkillConfig`, loading, bundle paths |
| `crates/kerness/src/skill/runtime.rs` | activation, the `Skill` tool, the tool gate |
| `crates/kerness/src/skill/mod.rs` | the two submodules |
| `crates/kerness/assets/skills/*/SKILL.md` | the four built-in skills |
| `bindings/python/src/skill.rs` | `PySkillActivation`, `PySkillRegistry`, the grant and lookup callbacks |
| `bindings/python/src/funcs.rs:419` | `load_skill`, `list_builtin_skills`, `format_skills_index`, `apply_gate` as pyfunctions |
| `bindings/python/src/types.rs:1152` | `PySkillConfig` |
| `bindings/python/kerness/{skill_loader,skill_runtime}.py` | re-export shims |

## Language and Conventions

Rust crate modules plus a PyO3 binding module and two Python re-export shims;
the root's [Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply. Local facts:

- Both crate modules open with a `//!` doc that states the design decision
  (`crates/kerness/src/skill/runtime.rs:1` explains why a body is a tool result
  and not a prompt injection). Enforced by rustdoc under `-D warnings` only for
  syntax; the content is observed convention.
- The activation state sits behind a `Mutex<State>`
  (`crates/kerness/src/skill/runtime.rs:83`) and every lock is
  `expect("activation lock poisoned")`, matching the crate-wide lock idiom.
- Unit tests use `crate::testing::TempDir`
  (`crates/kerness/src/skill/loader.rs:255`, `crates/kerness/src/skill/runtime.rs:322`) and name tests as
  sentences. The Python tests group by behaviour (`TestGate`, `TestBundles`) and
  use `tmp_path`.
- The skill name regex is a `LazyLock<Regex>` with `expect("static pattern")`
  (`crates/kerness/src/skill/loader.rs:219`), the crate's pattern for compile-once
  regexes.
- The Python shim module is `skill_runtime`, not `skills`, because
  `kerness.skills` is the installed `SKILL.md` data directory
  ([selfcheck.md](selfcheck.md) records the constraint).

## Design and Invariants

**Two files, one direction each.** `loader.rs` turns a file into a `SkillConfig`
and depends on `assets`, `error`, and `pyfmt`; `runtime.rs` decides what an agent
may do with one and depends on `error`, `pyfmt`, `skill::loader`, and `tooling`.
Neither reaches up into `session`; the session supplies both callbacks the
registry needs (`SkillsFor`, `GrantPaths`) and applies the transforms in
`Shared::active_tools` (`crates/kerness/src/session.rs:457`).

**Progressive disclosure.** A system prompt carries one line per skill
(`format_skills_index`); the body arrives as a tool result when the agent calls
`Skill`, lands in that agent's private scratch, and lives for the current turn
only. A second load in the same turn answers "already loaded" instead of
repeating the text (`crates/kerness/src/skill/runtime.rs:146`). A fresh
`SkillActivation` per turn is what makes the lifetime true:
`Shared::start_activation` builds one at `crates/kerness/src/session.rs:481`.
Tests: `the_body_is_served_once_per_turn` (`crates/kerness/src/skill/runtime.rs:386`) and
`the_body_arrives_for_the_turn_that_asked_and_no_later_one`
(`crates/kerness/tests/skills_e2e.rs:188`).

**The gate is restrictive only, and unions across skills.** `apply_gate`
intersects the toolkit with the union of every active skill's `allowed-tools`;
a skill can never grant a tool the toolkit lacks, and loading a second skill must
not disable the first (`fold`, `crates/kerness/src/skill/runtime.rs:204`). The
`Skill` tool itself is never gated out (`crates/kerness/src/skill/runtime.rs:285`). Tests:
`apply_gate_is_restrictive_only` (`crates/kerness/src/skill/runtime.rs:460`),
`two_skills_union_rather_than_intersect`
(`crates/kerness/src/skill/runtime.rs:438`),
`the_skill_tool_is_never_gated_out`
(`crates/kerness/src/skill/runtime.rs:509`).

**`requires-tools` is the one additive step, bounded by registration.**
`admit_required` adds back only out of what the caller registered with
`add_tool`, and runs after the gameplan's `tools:` narrowing and after the gate,
so a skill can reach a session's tools and never invent one
(`crates/kerness/src/skill/runtime.rs:305`). The check that a required name was
registered at all happens once, in `check_required_tools`
(`crates/kerness/src/session.rs:1926`), before the first provider call. Tests:
`a_required_tool_comes_back_past_both_narrowings`
(`crates/kerness/src/skill/runtime.rs:475`),
`a_required_tool_nobody_registered_is_not_invented_here`
(`crates/kerness/src/skill/runtime.rs:497`), and
the two session-level owners at `crates/kerness/tests/skills_e2e.rs:405` and
`:449`.

**`None` and `[]` are different answers for `allowed-tools` only.** Absent means
*narrow nothing*; an empty list means *permit nothing*. Collapsing them would
grant every tool to a skill that asked for none, so `SkillConfig.allowed_tools`
stays `Option` all the way to Python (`crates/kerness/src/skill/loader.rs:37`).
For `requires-tools` absent and empty both say *requires nothing*, so it is a
plain `Vec` (`crates/kerness/src/skill/loader.rs:45`). Both keys go through one
parser, `parse_tool_list` (`crates/kerness/src/skill/loader.rs:156`), so inline and block YAML read the same and an error names the
key.

**Bundle grants follow the file, not the name.** Only a file named `SKILL.md`
owns its directory (`crates/kerness/src/skill/loader.rs:98`), and only a skill
resolved *inside* the package's `skills/` directory is `builtin`
(`is_builtin`, `crates/kerness/src/skill/loader.rs:136`, answered about the canonical path so a symlink
cannot widen the grant). `manifest` grants only when the skill is built-in and a
grant hook exists (`crates/kerness/src/skill/runtime.rs:177`); the session's hook additionally checks
`trust_skill_bundles` (`crates/kerness/src/session.rs:606`) before calling
`AccessManager::allow_dirs` (`crates/kerness/src/access.rs:506`). A skill from
an arbitrary path is listed but not opened. Tests:
`only_builtin_skills_are_marked_builtin`
(`crates/kerness/src/skill/loader.rs:435`),
`a_builtin_bundle_is_listed_and_granted`
(`crates/kerness/src/skill/runtime.rs:571`),
`an_untrusted_bundle_is_listed_but_not_granted`
(`crates/kerness/src/skill/runtime.rs:588`),
`a_skill_from_a_path_lists_its_bundle_without_opening_it`
(`crates/kerness/tests/skills_e2e.rs:324`).

**Naming is validated at load.** A name is a lowercase slug of at most 64
characters and must match its parent directory when the file is a `SKILL.md`
(`validate_skill_name`, `crates/kerness/src/skill/loader.rs:219`); a missing
name or description is `Error::Value`. Test:
`a_name_must_be_a_slug_and_must_match_its_directory`
(`crates/kerness/src/skill/loader.rs:409`).

**Failure directions at the boundary.** An unknown skill name from the model is
`Error::Session`, which the dispatcher turns into a readable tool result rather
than a failed run (`crates/kerness/src/skill/runtime.rs:146`). On the Python side a grant hook that
raises has merely failed to widen access and is swallowed
(`bindings/python/src/skill.rs:22`); a `skills_for` lookup that raises leaves the
agent with no skills (`bindings/python/src/skill.rs:95`). Neither swallow is asserted by a test.

## Key Types and Entry Points

- `crates/kerness/src/skill/loader.rs:27` — `SkillConfig` — name, description,
  body, `allowed_tools: Option<Vec<String>>`, `requires_tools: Vec<String>`,
  `base_dir`, `builtin`. `bundle_paths()` at `:56` returns the `scripts/` and
  `references/` directories that exist (`BUNDLE_DIRS`, `:23`).
- `crates/kerness/src/skill/loader.rs:78` — `load_skill(name_or_path)` — a bare
  name is a built-in; a separator or `.md` makes it a path tried as-is and then
  under the built-in directory. `Error::NotFound` when nothing resolves,
  `Error::Value` for bad frontmatter.
- `crates/kerness/src/skill/loader.rs:113` — `list_builtin_skills()` —
  enumerated from disk, sorted; a directory counts only if it holds a
  `SKILL.md`.
- `crates/kerness/src/skill/runtime.rs:38` — `SKILL_TOOL_NAME` — `"Skill"`, the
  one reserved tool name (`crates/kerness/src/harness.rs:25`).
- `crates/kerness/src/skill/runtime.rs:59` — `format_skills_index(skills)` — the
  prompt block; empty string for no skills.
- `crates/kerness/src/skill/runtime.rs:80` — `SkillActivation` — one agent's
  skills for one turn. `load(name)` at `:146` returns body plus manifest and
  performs the grant; `gate()` at `:123` and `required()` at `:133` are what the
  session reads after each tool call; `loaded()` feeds the run checkpoint.
- `crates/kerness/src/skill/runtime.rs:223` — `SkillRegistry` — holds the
  `SkillsFor` lookup (`:44`) and optional `GrantPaths` hook (`:41`);
  `activation_for(agent)` at `:239` starts a turn, `build_tool(activation)` at
  `:252` returns the `Skill` tool or `None` when the agent has no skills (an
  empty `enum` is not a valid schema).
- `crates/kerness/src/skill/runtime.rs:285` — `apply_gate(tools, gate)` —
  restrictive narrowing; `:305` `admit_required(tools, registered, required)` —
  the additive counterpart.
- `bindings/python/src/skill.rs:40` — `PySkillActivation` and `:81`
  `PySkillRegistry` — share the crate's `Arc<SkillActivation>` rather than
  copying it, so a caller holding one sees the narrowing a tool call performed.
- `bindings/python/src/funcs.rs:441` — `apply_gate` as a pyfunction, and
  `:419` `load_skill`; `bindings/python/src/types.rs:1152` — `PySkillConfig`.

## Interactions

- [session.md](session.md) — registers skills through `add_skill`, caches the
  per-agent list the `SkillsFor` callback reads, starts an activation per turn,
  composes `build_tool` → `apply_gate` → `admit_required` in
  `Shared::active_tools` (`crates/kerness/src/session.rs:457`), and refuses an
  unregistered required tool before the run. Tested end to end in
  `crates/kerness/tests/skills_e2e.rs`.
- [harness.md](harness.md) — `resolve_skills` unions the gameplan's `skills:`
  with the session's (skills widen; tools narrow), and `RESERVED_TOOL_NAMES`
  keeps `Skill` off the tool registry.
- [access.md](access.md) — the grant is `AccessManager::allow_dirs`, and only
  when the policy's `trust_skill_bundles` is set; a skill that shells out is
  narrowed by `allowed_hosts` like any other command.
- [prompting.md](prompting.md) — `format_skills_index` output is the
  `skills_for` callback's return value, placed after context and before memory.
- [toolkit.md](toolkit.md) — the `Skill` tool is an ordinary `ToolSpec`; its
  handler is a Rust closure that `PyToolHandler` makes callable from Python.
- [run.md](run.md) — the checkpoint records `loaded_skills` and replays
  `load` on restore so a resumed turn holds the same gate.
- [selfcheck.md](selfcheck.md) — loads every built-in skill.

## How to Test

```sh
cargo test -p kerness skill                                               # pass = 45 passed, 0 failed
cargo test -p kerness --test skills_e2e                                   # pass = 13 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_skill_loader.py -q  # pass = 10 passed
.venv/bin/python -m pytest bindings/python/tests/test_skill_runtime.py -q # pass = 17 passed
```

- `bindings/python/tests/test_skill_runtime.py:139` —
  `test_a_builtin_bundle_is_listed_and_granted` — and `:149`
  `test_an_untrusted_bundle_is_listed_but_not_granted`: the grant is
  conditional on the policy, and the listing is not.
- `bindings/python/tests/test_skill_loader.py:59` —
  `test_absent_narrows_nothing_and_an_empty_list_narrows_to_nothing` — why
  `allowed_tools` stays `Option` all the way to Python, and `:79`
  `test_requires_tools_is_a_plain_tuple_with_no_absent_state` for why its
  counterpart does not.
- `crates/kerness/tests/skills_e2e.rs:405` —
  `a_required_tool_comes_back_past_the_gameplans_own_list` — and `:449`
  `a_required_tool_nobody_registered_is_refused_before_the_run`: the two ends of
  the additive direction, driven through a whole session.
- `crates/kerness/tests/skills_e2e.rs:131` —
  `only_the_name_and_the_description_reach_the_system_prompt`; `:223`
  `a_second_load_in_the_same_turn_says_so_instead_of_repeating`; `:264`
  `allowed_tools_narrows_what_the_rest_of_the_turn_is_offered`; `:290`
  `two_skills_in_one_turn_union_their_gates`; `:385`
  `an_empty_allowed_tools_leaves_only_the_skill_tool`.
- `bindings/python/tests/test_skill_runtime.py:108` —
  `test_the_skill_tool_is_never_gated_out`; `:50`
  `test_the_body_is_served_once_per_turn`; `:115`
  `test_the_enum_is_this_agent_s_skills_and_the_handler_loads_them` — the
  `Skill` handler called directly from Python.
- Gaps: no test drives a Python grant hook or `skills_for` lookup that raises
  (`bindings/python/src/skill.rs:22`, `:95`); the swallow-and-continue behaviour
  is observed in code only.

## Review and Refactor Guide

- Changing the `SKILL.md` frontmatter contract (a new key) → `load_skill` and
  `parse_tool_list` in `loader.rs`, `SkillConfig` and its `PySkillConfig`
  mirror (`bindings/python/src/types.rs:1152`), and the loader tests in both
  languages. A key that parses and does nothing violates the root's dead-key
  rule; it must reach `fold` or the session.
- Changing gate or requirement semantics → `fold`, `apply_gate`,
  `admit_required`, the composition order in `Shared::active_tools`
  (`crates/kerness/src/session.rs:457`), and `check_required_tools`; re-run
  `skills_e2e` because the order is only observable through a session's second
  provider call.
- Changing what `load` returns (body, manifest text, the "already loaded" line)
  → `SkillActivation::load` and `manifest`; the strings are asserted by
  `runtime.rs` tests and by `skills_e2e.rs`, and models read them.
- Changing bundle trust → `is_builtin`, `manifest`'s `filter(|_| skill.builtin)`,
  and the session hook at `crates/kerness/src/session.rs:606`; the three tests
  named under Design must keep passing, and a widening is a `blocker`-class
  security change per the root's Review Checks.
- Safe extension points: a new bundle directory name is one entry in
  `BUNDLE_DIRS`; a new built-in skill is a directory under
  `crates/kerness/assets/skills/` duplicated byte-for-byte under
  `bindings/python/kerness/skills/` (`bindings/python/tests/test_packaging.py:42`
  asserts the pair).
- Forbidden coupling: nothing in `skill/` may import `session`; the session
  hands callbacks down. The `Skill` handler must stay a pure closure over the
  activation so `PyToolHandler` can expose it.
- Compatibility: `loaded_skills` in the run checkpoint is replayed through
  `load` on restore, so a change to `load`'s side effects changes what a resumed
  turn holds; check `crates/kerness/tests/resume.rs`.

Improvement candidates (proposals, not accepted work):

- Test the two Python swallow paths in `bindings/python/src/skill.rs` with a
  raising hook and a raising lookup; success check: a new case in
  `test_skill_runtime.py` asserting the activation still loads and no exception
  escapes.

## Open Gaps / Roadmap

- The gate is a union across active skills
  (`bindings/python/tests/test_skill_runtime.py:88`), so activating a broad skill
  widens what a narrow one permitted. Intersection would be the stricter reading
  but would make two useful skills mutually exclusive.
- Bundle grants are read-only directory grants; a skill cannot ship a script and
  also be permitted to run it without the policy allowing the program separately.
  `requires-tools` closes half of this — the skill can claim `run_command` — but
  the program allowlist is still the caller's to write.
- A skill's body is loaded whole. There is no way to pull in one section.
- The body lives for one turn. A later turn that needs the same skill invokes it
  again and pays again; that is the price of not mutating a prefix-cached system
  prompt (`crates/kerness/src/skill/runtime.rs:1`).
