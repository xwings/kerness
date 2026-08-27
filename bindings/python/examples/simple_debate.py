#!/usr/bin/env python3
"""Minimal debate example using the kerness framework.

Demonstrates per-agent providers: each agent can use a different LLM backend.
You can also set a single session-level provider if all agents use the same one.
"""

import os
import sys

import kerness


def main() -> None:
    # Per-agent providers — each agent uses its own backend
    openrouter_key = os.environ.get("OPENROUTER_API_KEY", "")
    openai_key = os.environ.get("OPENAI_API_KEY", "")
    claude_key = os.environ.get("ANTHROPIC_API_KEY", "")

    if not openrouter_key:
        print("Set OPENROUTER_API_KEY environment variable first.")
        print("Optionally set OPENAI_API_KEY and ANTHROPIC_API_KEY for multi-provider.")
        sys.exit(1)

    # Fallback: use OpenRouter for agents without a specific provider
    session = kerness.Session(
        gameplan="debate",
        topic="Is pineapple acceptable on pizza?",
        provider=kerness.OpenRouterProvider(api_key=openrouter_key),
    )

    # Alice uses OpenAI directly (if key available), otherwise falls back to session provider
    if openai_key:
        session.add_participant(
            "Alice", model="gpt-4o", persona="Pragmatic engineer",
            provider=kerness.OpenAIProvider(api_key=openai_key),
        )
    else:
        session.add_participant("Alice", model="openai/gpt-4o", persona="Pragmatic engineer")

    # Bob uses Claude directly (if key available), otherwise falls back to session provider
    if claude_key:
        session.add_participant(
            "Bob", model="claude-sonnet-4-20250514", persona="Devil's advocate",
            provider=kerness.ClaudeProvider(api_key=claude_key),
        )
    else:
        session.add_participant("Bob", model="anthropic/claude-sonnet-4", persona="Devil's advocate")

    # Moderator uses session-level OpenRouter provider
    session.add_orchestrator("Moderator", model="openai/gpt-4o")

    result = session.run()

    print("\n--- Result ---")
    print(f"Topic: {result.topic}")
    print(f"Turns: {result.turns_completed}")
    print(f"Consensus: {result.consensus_reached}")
    if result.summary:
        print(f"Summary: {result.summary}")


if __name__ == "__main__":
    main()
