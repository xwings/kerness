# Skills

## Goal

A skill is a directory holding a `SKILL.md` and optionally `scripts/` and
`references/`. Loading one gives an agent a body of instructions it can pull in
on demand through the `Skill` tool, and — when the access policy trusts bundles —
read access to the skill's own directory. A skill may also narrow the turn to a
subset of tools. Serves **M2**.

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
| `crates/kerness-py/src/skill.rs` | `PySkillActivation`, `PySkillRegistry` |
| `python/kerness/{skill_loader,skill_runtime}.py` | re-export shims |

## Key Types and Entry Points

- `crates/kerness/src/skill/loader.rs:28` — `SkillConfig` — name, description,
  body, base directory, and the optional allowed-tools list.
- `crates/kerness/src/skill/loader.rs:49` — `bundle_paths()` — the `scripts/` and
  `references/` directories that exist; these are what get granted.
- `crates/kerness/src/skill/loader.rs:24` — `BUNDLE_DIRS` — `["scripts", "references"]`.
- `crates/kerness/src/skill/loader.rs:71` — `load_skill(name_or_path)` — built-in
  name or path, like the gameplan loader.
- `crates/kerness/src/skill/runtime.rs:38` — `SKILL_TOOL_NAME` — `"Skill"`, the one
  reserved tool name (`harness.rs:25`).
- `crates/kerness/src/skill/runtime.rs:80` — `SkillActivation` — the skills
  available to one agent; `load(name)` at `:120` is what the `Skill` tool calls,
  and it is where bundle paths are granted.
- `crates/kerness/src/skill/runtime.rs:111` — `gate()` — the union of every active
  skill's allowed tools, or `None` when no active skill narrows anything.
- `crates/kerness/src/skill/runtime.rs:196` — `SkillRegistry` — per-agent
  activations; `activation_for(name)` at `:212`, `build_tool(activation)` at `:225`.
- `crates/kerness/src/skill/runtime.rs:258` — `apply_gate(tools, gate)` — narrows a
  tool list. `None` and an empty set mean opposite things: no declaration versus a
  declaration that permits nothing, which is why `SkillConfig.allowed_tools`
  stays `Option` all the way to Python.
- `crates/kerness/src/skill/runtime.rs:59` — `format_skills_index(skills)` — the
  index block in the agent's system prompt.

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
cargo test -p kerness skill                                  # pass = 0 failed
.venv/bin/python -m pytest tests/test_skill_loader.py -q     # pass = 0 failed
.venv/bin/python -m pytest tests/test_skill_runtime.py -q    # pass = 0 failed
```

- `tests/test_skill_runtime.py:138` — `test_a_builtin_bundle_is_listed_and_granted` —
  and `:148` `test_an_untrusted_bundle_is_listed_but_not_granted`: the grant is
  conditional on the policy, and the listing is not.
- `tests/test_skill_loader.py:59` — `test_absent_narrows_nothing_and_an_empty_list_narrows_to_nothing` —
  why `allowed_tools` stays `Option` all the way to Python.
- `tests/test_skill_runtime.py:107` — `test_the_skill_tool_is_never_gated_out` — a
  skill cannot narrow away the tool that loads skills.
- `:49` — `test_the_body_is_served_once_per_turn` — a skill's text does not
  re-enter the context on every subsequent call.

## Open Gaps / Roadmap

- The gate is a union across active skills
  (`tests/test_skill_runtime.py:87`), so activating a broad skill widens what a
  narrow one permitted. Intersection would be the stricter reading but would make
  two useful skills mutually exclusive.
- Bundle grants are read-only directory grants; a skill cannot ship a script and
  also be permitted to run it without the policy allowing the program separately.
- A skill's body is loaded whole. There is no way to pull in one section.
