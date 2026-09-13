---
eatmycode_version: "2.0.0"
---

# Build and verification contract

Owner: [Project architecture](../../ARCHITECTURE.md)

Read when: changing manifests, CI, shared test helpers, public documentation or
architecture routes/agent aliases; setting up or validating both artifacts.

## Contract

The [Cargo workspace](../../Cargo.toml) owns version, Rust 2021/MSRV 1.88,
dependency baselines and release build settings. [CI](../../.github/workflows/ci.yml)
declares Rust format/lint/test/examples/docs plus Python install/test/selfcheck/
lint. The [core manifest](../../crates/kerness/Cargo.toml) inherits workspace
metadata/dependencies; `Cargo.lock` records resolution. Python configuration is owned by
[Python bindings](../modules/python-bindings.md); read that owner for changes
under `bindings/python/`.

From repository root, set up an isolated Python build environment:

```sh
python3 -m venv .venv
.venv/bin/pip install 'maturin>=1.7,<2.0' 'ruff>=0.16,<0.17'
```

From `bindings/python/`, build/install the extension and dev dependencies:

```sh
../../.venv/bin/maturin develop --extras dev
```

This needs Rust >=1.88, a working compiler/linker, CPython >=3.10 and dependency
access/cache. Return to the repository root for all checks below. Rebuild after
Rust/binding/version changes. Python dev dependencies include pytest and Pydantic.
No application credentials are needed for the automated suites/offline examples.

The [root commands](../../ARCHITECTURE.md#verification) are the baseline. Additional
existing CI checks are:

| Check | Command at repository root | What passing proves |
| --- | --- | --- |
| Minimum toolchain | `cargo +1.88.0 check --workspace --all-targets --locked` | With installed 1.88.0, committed dependency resolution compiles at the declared minimum. |
| Examples | `cargo build -p kerness --examples` | All Rust examples compile; it does not call live models. |
| API documentation | `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps -p kerness` | Rust documentation has no warnings. |

Shared test infrastructure is `crates/kerness/src/testing.rs`,
`crates/kerness/tests/common/mod.rs` and `bindings/python/tests/conftest.py`.
Consumers own behavioral assertions; these helpers own reusable temporary files
and scripted providers/channels. Inventory the whole suite before test edits per
[Agent Rules](../AGENT_RULES.md), then inspect only matching cases. Do not replace
behavior tests with copied inventories. Asset/module discovery is exercised by
`tests/public_api.rs` and `bindings/python/tests/test_packaging.py`.

Root README, LICENSE and `assets/` are public project documentation/branding.
Runtime bundled assets live under the two artifact trees. Architecture files and
relative root `AGENT.md`, `AGENTS.md`, `CLAUDE.md` symlinks form the agent map;
all aliases must resolve to `ARCHITECTURE.md`.

## Change and Verify

For shared manifest/dependency changes read both affected Rust owners and Python
bindings; run Rust baseline, locked minimum, examples/rustdoc and installed Python
checks. For fixtures run affected consumers and the aggregate suite. For doc-only
changes verify cited source, routes, local links/anchors, required headings,
shared rules, character limits and versions; no new behavioral tests are needed.
With eatmycode available use its Architecture Verification and stamp only after
validation, root last. Otherwise retain the root/rules/owner reading contract.

Every root route must find one canonical owner, its applicable constraints and
checks. In particular, runtime-to-binding API changes require both owners, while
asset content changes require harness/assets plus packaging parity. The build
topic is navigation/check guidance, not an extra runtime subsystem.

## Evidence and Gaps

Verified locally with Rust/Cargo 1.88.0 and Python 3.13.5: root format/Clippy
checks, 525 Rust unit/integration tests plus one doctest, locked all-targets
compilation, example builds and warning-free rustdoc. `offline_debate`,
`host_control` and `resume_approval` completed offline; host control recorded
one provider operation and resumed approval executed each saved note once.
The minimum check used `cargo check --workspace --all-targets --locked` with
the active 1.88.0 toolchain, equivalent to the explicit selector above.

An isolated maturin 1.15.0 build with dev extras passed 502 Python tests,
installed-package selfcheck including assets/Pydantic, Ruff 0.16.7, and the
Python host-control example. These results establish the local baseline;
rerun affected checks when source or configuration changes.

Existing configuration gap: the CI job named MSRV 1.88 actually selects
`dtolnay/rust-toolchain@1.120.0` ([CI](../../.github/workflows/ci.yml)). Thus its name
does not prove Rust 1.88 coverage. Correcting that pin is a proposed follow-up,
not part of this architecture update. Local minimum-toolchain verification can
independently establish compilation at 1.88.

The [wheel build matrix](../../.github/workflows/release.yml) includes Windows
despite POSIX-only declared support/access assumptions. Windows runtime behavior
is unverified; reconciling the matrix and support claim is a proposed follow-up.

CI declares Python 3.10/3.13 on Ubuntu; a local run on one interpreter does not
verify that matrix or macOS. No Python type checker or architecture checker is
configured in CI. Offline tests do not certify live model/OAuth/network behavior.
