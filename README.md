# Kerness

Kerness is a synchronous framework for building multi-agent harnesses. A
Markdown *gameplan* supplies a machine-readable harness contract in YAML
frontmatter and human instructions in its body. The contract controls roles,
participant bounds, loop limits and phases, termination tokens, available tools
and skills, and the shape of the returned result.

Everything the harness sits on belongs to the framework — provider transport,
tool dialects, prompt assembly, the orchestrator loop, access control, memory,
session files, and compaction — so a new harness is a new Markdown file rather
than a new runtime. The bundled `debate`, `discussion`, and `research`
gameplans are worked examples of the contract, not the product.

The framework is written in Rust and ships with Python bindings. The Rust crate
`kerness` is usable on its own; the `kerness` Python package is the same
framework with a Python surface.

## Install

```sh
pip install kerness            # runtime
pip install 'kerness[structured]'   # plus pydantic, for OpenAIProvider(output_type=...)
```

To build from a checkout:

```sh
maturin develop --release
python -m kerness.selfcheck     # pass = "OK: all core checks passed"
```

## A run, in Python

```python
from kerness import ConsoleChannel, OpenAIProvider, Session

session = Session(
    gameplan="debate",
    topic="Should the cache be write-through?",
    provider=OpenAIProvider(api_key="..."),
    channel=ConsoleChannel(),
)
session.add_participant("Alice", model="gpt-4o", persona="pragmatic_engineer.md")
session.add_participant("Bob", model="gpt-4o", persona="devils_advocate.md")
session.add_orchestrator("Mod", model="gpt-4o")

result = session.run()
print(result.summary)
```

Agents can use provider-native tool calling or the text-fence fallback; the
dialect is chosen per provider and normalized above that point, so a session can
mix backends. Skills use progressive disclosure — prompts carry names and
descriptions, and the full instructions load through a turn-local `Skill` tool.

There is no daemon and no server. A run that is given a `session_file` writes
its state to disk after every turn and continues from that file the next time
the same script runs.

## Layout

```text
crates/kerness/      # the framework, pure Rust
crates/kerness-py/   # the PyO3 extension module, kerness._core
python/kerness/      # the Python package: shims, deliberate Python, assets
tests/               # pytest suite
examples/            # runnable integrations
```

`ARCHITECTURE.md` is the entry point for how the pieces fit together.

## License

MIT. See [LICENSE](LICENSE).
