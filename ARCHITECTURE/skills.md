# Skills

## Goal

A skill is a directory holding a `SKILL.md` and optionally `scripts/` and
`references/`. Loading one gives an agent a body of instructions it can pull in
on demand through the `Skill` tool, and — when the access policy trusts bundles —
read access to the skill's own directory. A skill also says which tools the turn
should hold: `allowed-tools:` narrows it to a subset, and `requires-tools:` adds
back what the skill cannot work without.

The split is two files: `loader.rs` reads a skill off disk, `runtime.rs` decides
what an agent can do with it.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/skill/loader.rs` | `SkillConfig`, loading, bundle paths |
| `crates/kerness/src/skill/runtime.rs` | activation, the `Skill` tool, the tool gate |
| `crates/kerness/src/skill/mod.rs` | the two submodules |
| `crates/kerness/assets/skills/*/SKILL.md` | the built-in skills |
| `bindings/python/src/skill.rs` | `PySkillActivation`, `PySkillRegistry` |
| `bindings/python/kerness/{skill_loader,skill_runtime}.py` | re-export shims |

## Key Types and Entry Points

- `crates/kerness/src/skill/loader.rs:27` — `SkillConfig` — name, description,
  body, base directory, the optional allowed-tools list, and the required-tools
  list at `:46`.
- `crates/kerness/src/skill/loader.rs:56` — `bundle_paths()` — the `scripts/` and
  `references/` directories that exist; these are what get granted.
- `crates/kerness/src/skill/loader.rs:23` — `BUNDLE_DIRS` — `["scripts", "references"]`.
- `crates/kerness/src/skill/loader.rs:78` — `load_skill(name_or_path)` — built-in
  name or path, like the gameplan loader.
- `crates/kerness/src/skill/loader.rs:156` — `parse_tool_list(value, key, source)`
  — both lists go through it, so inline and block YAML parse the same way for
  each and the error names whichever key was wrong.
- `crates/kerness/src/skill/runtime.rs:38` — `SKILL_TOOL_NAME` — `"Skill"`, the one
  reserved tool name (`harness.rs:25`).
- `crates/kerness/src/skill/runtime.rs:80` — `SkillActivation` — the skills
  available to one agent; `load(name)` at `:135` is what the `Skill` tool calls,
  and it is where bundle paths are granted.
- `crates/kerness/src/skill/runtime.rs:112` — `gate()` — the union of every active
  skill's allowed tools, or `None` when no active skill narrows anything.
- `crates/kerness/src/skill/runtime.rs:122` — `required()` — the union of every
  active skill's required tools.
- `crates/kerness/src/skill/runtime.rs:193` — `fold(state, skill)` — folds one
  loaded skill into the activation state: its requirements accumulate, its gate
  unions in.
- `crates/kerness/src/skill/runtime.rs:212` — `SkillRegistry` — per-agent
  activations; `activation_for(name)` at `:228`, `build_tool(activation)` at `:241`.
- `crates/kerness/src/skill/runtime.rs:274` — `apply_gate(tools, gate)` — narrows a
  tool list. `None` and an empty set mean opposite things: no declaration versus a
  declaration that permits nothing, which is why `SkillConfig.allowed_tools`
  stays `Option` all the way to Python.
- `crates/kerness/src/skill/runtime.rs:294` — `admit_required(tools, registered,
  required)` — the one additive step, applied after `apply_gate`.
- `crates/kerness/src/skill/runtime.rs:59` — `format_skills_index(skills)` — the
  index block in the agent's system prompt.

### Why `requires-tools` is a plain list and `allowed-tools` is an `Option`

They are opposite directions and the ambiguity only exists in one of them. For
`allowed-tools`, absent means *narrow nothing* and empty means *permit nothing* —
collapsing them would silently grant every tool to a skill that asked for none.
For `requires-tools`, absent and empty both say *this skill requires nothing*, so
there is no second state to keep apart and an `Option` would carry a distinction
nobody could act on.

### What a requirement can and cannot reach

`admit_required` adds only out of what the caller registered with `add_tool`, so
a skill cannot invent a handler. It runs last — past the gameplan's `tools:`
narrowing and past another skill's gate — because a skill that ships
instructions for a tool the agent cannot call is prose about nothing. The check
that a required name was registered at all happens once, in
`session.rs:check_required_tools`, before the first provider call: a skill
requiring a tool nobody wrote is a session error, not a tool that quietly never
appears.

## Interactions

- Registered and resolved by [session.md](session.md); names checked against
  [harness.md](harness.md)'s `resolve_skills`.
- Grants directories to [access.md](access.md) through `allow_dirs`, and only
  when the policy trusts bundles.
- Its index goes into a prompt via [prompting.md](prompting.md).
- The `Skill` tool is a `ToolSpec` like any other — see [toolkit.md](toolkit.md).
- Loaded for every built-in by [selfcheck.md](selfcheck.md).

## How to Test

```sh
cargo test -p kerness skill                                               # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_skill_loader.py -q  # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_skill_runtime.py -q # pass = 0 failed
```

- `bindings/python/tests/test_skill_runtime.py:139` — `test_a_builtin_bundle_is_listed_and_granted` —
  and `:149` `test_an_untrusted_bundle_is_listed_but_not_granted`: the grant is
  conditional on the policy, and the listing is not.
- `bindings/python/tests/test_skill_loader.py:59` — `test_absent_narrows_nothing_and_an_empty_list_narrows_to_nothing` —
  why `allowed_tools` stays `Option` all the way to Python, and `:79`
  `test_requires_tools_is_a_plain_tuple_with_no_absent_state` for why its
  counterpart does not.
- `crates/kerness/tests/skills_e2e.rs` —
  `a_required_tool_comes_back_past_the_gameplans_own_list` and
  `a_required_tool_nobody_registered_is_refused_before_the_run`: the two ends of
  the additive direction, driven through a whole session.
- `bindings/python/tests/test_skill_runtime.py:108` — `test_the_skill_tool_is_never_gated_out` — a
  skill cannot narrow away the tool that loads skills.
- `:50` — `test_the_body_is_served_once_per_turn` — a skill's text does not
  re-enter the context on every subsequent call.

## Open Gaps / Roadmap

- The gate is a union across active skills
  (`bindings/python/tests/test_skill_runtime.py:87`), so activating a broad skill widens what a
  narrow one permitted. Intersection would be the stricter reading but would make
  two useful skills mutually exclusive.
- Bundle grants are read-only directory grants; a skill cannot ship a script and
  also be permitted to run it without the policy allowing the program separately.
  `requires-tools` closes half of this — the skill can now claim `run_command`
  — but the program allowlist is still the caller's to write.
- A skill's body is loaded whole. There is no way to pull in one section.
