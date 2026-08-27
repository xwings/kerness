"""Environment self-check: ``python3 -m kerness.selfcheck``.

Answers one question — is this installation usable, and which optional
features are available? Exits 0 when every core subsystem imports and its
built-in assets load, 1 otherwise. Optional features report their status
without affecting the exit code.
"""

from __future__ import annotations

import importlib
import sys

# (module, label) pairs that must import for the package to be usable.
_CORE_MODULES: list[tuple[str, str]] = [
    ("kerness._core", "core"),
    ("kerness.exceptions", "exceptions"),
    ("kerness.jsonschema", "jsonschema"),
    ("kerness.utils", "utils"),
    ("kerness.tooling", "tooling"),
    ("kerness.toolschema", "toolschema"),
    ("kerness.provider", "provider"),
    ("kerness.agent", "agent"),
    ("kerness.access", "access"),
    ("kerness.memory", "memory"),
    ("kerness.channel", "channel"),
    ("kerness.persona_loader", "persona"),
    ("kerness.skill_loader", "skill-loader"),
    ("kerness.harness", "harness"),
    ("kerness.gameplan_loader", "gameplan"),
    ("kerness.prompting", "prompting"),
    ("kerness.conversation", "conversation"),
    ("kerness.compaction", "compaction"),
    ("kerness.sessionfile", "sessionfile"),
    ("kerness.toolkit", "toolkit"),
    ("kerness.agent_runtime", "agent-runtime"),
    # 'kerness.skills' is the SKILL.md data directory, so the runtime is
    # named skill_runtime to avoid shadowing it.
    ("kerness.skill_runtime", "skill-runtime"),
    ("kerness.loop", "loop"),
    ("kerness.session", "session"),
]

# (import target, label, what it enables) — absence is reported, not fatal.
_OPTIONAL: list[tuple[str, str, str]] = [
    ("pydantic", "pydantic", "structured output (OpenAIProvider(output_type=...))"),
]


def _check_imports(failures: list[str]) -> None:
    for module, label in _CORE_MODULES:
        try:
            importlib.import_module(module)
        except Exception as exc:  # noqa: BLE001 - report anything at all
            print(f"FAIL {label}: {type(exc).__name__}: {exc}")
            failures.append(label)
        else:
            print(f"PASS {label}")


def _check_assets(failures: list[str]) -> None:
    """Built-in gameplans, personas, and skills must load, not merely exist."""
    try:
        from kerness.gameplan_loader import (
            list_builtin_gameplans,
            load_gameplan,
        )

        names = list_builtin_gameplans()
        for name in names:
            gameplan = load_gameplan(name)
            if not gameplan.harness.loop.terminate_on:
                raise ValueError(f"gameplan '{name}' declares no terminate_on")
        print(f"PASS gameplans ({', '.join(names)})")
    except Exception as exc:  # noqa: BLE001
        print(f"FAIL gameplans: {type(exc).__name__}: {exc}")
        failures.append("gameplans")

    try:
        from kerness.skill_loader import list_builtin_skills, load_skill

        names = list_builtin_skills()
        for name in names:
            load_skill(name)
        print(f"PASS skills ({', '.join(names)})")
    except Exception as exc:  # noqa: BLE001
        print(f"FAIL skills: {type(exc).__name__}: {exc}")
        failures.append("skill-assets")

    try:
        from kerness.persona_loader import list_builtin_personas, load_persona

        names = list_builtin_personas()
        for name in names:
            load_persona(f"{name}.md")
        print(f"PASS personas ({', '.join(names)})")
    except Exception as exc:  # noqa: BLE001
        print(f"FAIL personas: {type(exc).__name__}: {exc}")
        failures.append("personas")


def _check_optional() -> None:
    for module, label, enables in _OPTIONAL:
        try:
            importlib.import_module(module)
        except ImportError:
            print(f"SKIP {label}: not installed — {enables} unavailable")
        else:
            print(f"PASS {label}: {enables} available")


def main() -> int:
    """Run every check and return a process exit code."""
    import platform

    print(f"kerness selfcheck (python {platform.python_version()})")
    print("-" * 52)

    failures: list[str] = []
    _check_imports(failures)
    _check_assets(failures)
    print("-" * 52)
    _check_optional()
    print("-" * 52)

    if failures:
        print(f"FAILED: {len(failures)} check(s): {', '.join(failures)}")
        return 1
    print("OK: all core checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
