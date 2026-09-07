---
eatmycode_version: "1.2.0"
---

# Roadmap detail

Supporting page for [ARCHITECTURE.md](../ARCHITECTURE.md#roadmap): the
milestone table with the evidence behind each status, the repository
configuration gap with its success check, and the improvement
candidates each owner records.

## Implementation milestones

The M1–M3 core upgrade is implemented in Rust and exposed through the binding;
execution is synchronous throughout, with no hidden executor or concurrent
agent scheduling.

| Milestone | Status | Delivered behaviour and evidence |
| --- | --- | --- |
| **M1 — Runtime ownership and tool capabilities** | done | Owned `SessionRun`, complete `ToolSpec` registration, contextual handlers with immutable identity and scoped capabilities ([session.md](session.md), [run.md](run.md)). |
| **M2 — Host-driven execution** | done | Shared run/step engine, typed input, events and control, external approval, single-agent host mode, schema-2 continuation, v1 boundary migration, explicit reconciliation ([run.md](run.md), [loop.md](loop.md), [sessionfile.md](sessionfile.md)). |
| **M3 — Outcomes and budgets** | done | Strict result diagnostics, typed terminal and turn reasons, retained committed history, normalized usage and operation, tool and time admission; token and cost limits require measured-threshold mode ([provider.md](provider.md), [memory.md](memory.md)). |
| **M4 — Adapters and richer sessions** | deferred | Native streaming, workflow adapters, session-store listing and forking, typed content parts, MCP and sequential subagents each need a separately justified change; parallel execution requires revising the synchronous invariant. |

M4 adapters must consume the M1–M3 contracts: streaming needs a transport seam
for partial output and explicit retry semantics, richer content needs a schema
and context-accounting change, workflow and MCP adapters reuse the execution,
access, approval and budget boundaries, and tools with several resumable
effects need a continuation protocol because synchronous callbacks cannot be
unwound and replayed. The practical limits of the current contracts are in
[run.md](run.md).

## Repository defects (evidence-backed, not yet fixed)

| Defect | Evidence | Fix and success check |
| --- | --- | --- |
| The CI MSRV toolchain does not match the declared minimum | `.github/workflows/ci.yml:62` pins `dtolnay/rust-toolchain@1.120.0`, while `Cargo.toml` declares `rust-version = "1.88"`; the explicit local 1.88.0 check passes | Pin `1.88.0` and exclude this minimum-version action from automatic version bumps; success = the CI MSRV job checks the declared floor with the current lockfile. |

The current `Cargo.lock` records both workspace crates at `0.1.2-dev`, matching
`Cargo.toml`. A clean current-source copy passes
`cargo +1.88.0 check --workspace --all-targets --locked`; there is no remaining
lockfile mismatch in this tree.

## Improvement candidates (proposals, not accepted work)

Each is recorded in its owning module with benefit and success check:

- A Rust-side asset parity check so the duplicated `assets/` cannot drift when
  the Python surface is not installed ([testing.md](testing.md)).
- Doc comments for `auto_approve_prefixes`, the loosest command mechanism
  ([access.md](access.md)).
- `.pyi` stubs for `_core`, so editors see the binding's surface
  ([bindings.md](bindings.md)).
- A query parameter on `MemoryStore::read`, a trait contract change and
  therefore a decision rather than an omission ([memory.md](memory.md)).
- A summary tool catalog, justified only once a tool source the host did not
  enumerate exists ([toolkit.md](toolkit.md)).
- An `OSError` arm in `from_py` so `Error::Io` survives a round trip
  ([errors.md](errors.md)); a caller or removal for the
  unreferenced `host_briefing` ([loop.md](loop.md)); a Rust unit
  test for `add_context`'s duplicate refusal ([context.md](context.md)).
