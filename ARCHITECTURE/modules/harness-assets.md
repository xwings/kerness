---
eatmycode_version: "2.0.0"
---

# Harness contracts and assets

Owner: [Project architecture](../../ARCHITECTURE.md)

Read when: changing gameplan keys, role/persona meaning, YAML parsing, skill
metadata/loading, asset discovery or either distribution's bundled asset content.

## Responsibility and Status

Implemented: Markdown/YAML becomes validated harness configuration plus prose.
This owner covers asset semantics and loaders in both artifact trees; Python
packaging mechanics belong to the binding owner. Scheduling and skill activation
belong to their runtime owners. Bundled gameplans are examples of the contract,
not special runtime modes. Tests verify parsing and enforcement; external bundle
contents remain caller inputs.

## Code Map

| Path / symbol | Role |
| --- | --- |
| [harness.rs](../../crates/kerness/src/harness.rs), `parse_harness`, `validate_harness` | Typed agents/loop/result contracts; aggregated registration errors. |
| [gameplan.rs](../../crates/kerness/src/gameplan.rs), `load_gameplan`; [role.rs](../../crates/kerness/src/role.rs), [persona.rs](../../crates/kerness/src/persona.rs) | Resolve files and distinguish structural position from persona prose. |
| [assets.rs](../../crates/kerness/src/assets.rs), `root`; [yaml.rs](../../crates/kerness/src/yaml.rs), `parse` | Shared search/frontmatter helpers and YAML scalar semantics. |
| [skill/loader.rs](../../crates/kerness/src/skill/loader.rs), `skill/mod.rs` | Skill names/descriptions/tool lists and bundle locations. |
| [crate assets](../../crates/kerness/assets), [package assets](../../bindings/python/kerness); [harness_contract.rs](../../crates/kerness/tests/harness_contract.rs), [public_api.rs](../../crates/kerness/tests/public_api.rs) | Gameplans/roles/personas/skills; contract enforcement and discovery tests. Package directory ownership here is limited to those four asset subdirectories. |

## Local Conventions

Use [root conventions](../../ARCHITECTURE.md#code-conventions). Required project
rules: every accepted harness key has a validation/rendering/execution consumer;
bundled assets stay domain-neutral. Put application-specific assets with examples
or the application. Both distributed Markdown asset trees must have equal content
(`bindings/python/tests/test_packaging.py`); no generator synchronizes them.
Inventory tests discover assets from disk rather than keeping a second name list
(`tests/public_api.rs`). Keep lookup errors explicit about the paths tried.

## Contracts and Invariants

- Asset root precedence is explicit `assets::set_root`, `KERNESS_ASSETS`, then
  the crate manifest's `assets/`. Assets remain files so bundle paths can be
  granted/read; import bootstrap points Python at its installed package.
- Shared path candidates try the supplied path, caller search directories, then
  built-ins. Gameplans/skills use their own fixed lookup rules; role/persona
  resolution can also search beside the loaded gameplan (`assets.rs`, loaders).
- YAML uses event-level 1.1 scalar rules: bare `no` is false, quoted `"no"` stays
  text. Dates and nonfinite numbers stay strings; multiple documents fail.
  Replacing the parser must preserve these decisions (`yaml.rs` inline tests).
- `validate_harness` checks required roles, participant bounds and registered
  tools/skills/context, reporting all violations before the first provider call.
  A declared but unregistered requirement must not disappear silently
  (`tests/harness_contract.rs`). `Skill` is a reserved tool name.
- Roles determine position from frontmatter; persona only decorates prompts.
  Absent role/prose resolves to participant, never privilege inferred from a word
  in the text (`role.rs`, `agent.rs`). Persona sections are optional.
- Skill metadata distinguishes absent versus empty `allowed-tools`; required
  tools must exist in session registration. Loading validates the slug and
  description; body disclosure and permission changes are runtime concerns
  (`skill/loader.rs`, `tests/skills_e2e.rs`).

## Dependencies and Boundaries

[Runtime](runtime.md) consumes contracts and resolves registered agents/resources;
read it whenever an accepted key changes behavior. [Tools/access](tools-access.md)
owns `skill/runtime.rs` and enforced tool/bundle grants; read it for tool metadata
changes. [Python bindings](python-bindings.md) exposes parsed types and checks
asset parity; read it when changing a schema, exporting a loader or adding an
asset family. Shared errors/rendering helpers are runtime-owned. This owner does
not grant filesystem access merely because a path loaded successfully.

## Change Guide

| Change trigger | Inspect / extend | Required docs / checks |
| --- | --- | --- |
| New/changed harness field | Parser + validator + runtime consumer; `harness_contract.rs` | Update this owner and runtime; parser tests and session enforcement must both pass. |
| Role/persona/search/YAML rule | Loader and inline tests; `public_api.rs`; matching Python loader tests | Update lookup/compatibility here and Python surface when affected. |
| Skill tool requirement or bundled file | Loader, mirrored asset, `skills_e2e.rs` | Read tools/access for activation; run packaging parity and discovery checks. |

## Verification

The [root Rust suite](../../ARCHITECTURE.md#verification) covers inline loader/YAML
tests, `harness_contract` preflight errors and scheduler consumers, `skills_e2e`
requirements, and `public_api` discovered asset loads. The
[Python suite](python-bindings.md#verification) covers `test_harness.py`, four
`test_*_loader.py` files, `test_skill_runtime.py`, and `test_packaging.py` parity.
Installed `kerness.selfcheck` must find every bundled family. See
[baseline evidence](../topics/build-checks.md#evidence-and-gaps).

## Known Gaps

The two asset trees require manual paired edits. Rust asset lookup's manifest
fallback is a filesystem path, not embedded bytes; relocated applications need
an explicit asset root. Validation proves declared syntax and runtime consumers,
not the usefulness or trustworthiness of arbitrary skill prose.
