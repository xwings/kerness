#!/usr/bin/env python3
"""Research mode example — orchestrator-driven investigation."""

import os
import sys

import kerness


def main() -> None:
    api_key = os.environ.get("OPENROUTER_API_KEY", "")
    if not api_key:
        print("Set OPENROUTER_API_KEY environment variable first.")
        sys.exit(1)

    session = kerness.Session(
        gameplan="research",
        topic="What are the implications of quantum computing on current cryptographic standards?",
        provider=kerness.OpenRouterProvider(api_key=api_key),
        channel=kerness.ConsoleChannel(),
    )

    session.add_participant("Dr. Chen", model="openai/gpt-4o", persona="Quantum computing researcher")
    session.add_participant("Prof. Smith", model="anthropic/claude-sonnet-4", persona="Cryptography expert")
    session.add_participant("Dr. Patel", model="openai/gpt-4o", persona="Cybersecurity policy analyst")
    session.add_orchestrator("Lead Researcher", model="openai/gpt-4o")

    session.add_skill("summarize")
    session.add_skill("fact-check")

    result = session.run()

    print("\n--- Result ---")
    print(f"Topic: {result.topic}")
    print(f"Turns: {result.turns_completed}")
    if result.summary:
        print(f"Summary: {result.summary}")

    # Memory is free-form prose, and readable after the session
    print("\n--- Memory ---")
    print(session.memory.read() or "(empty)")


if __name__ == "__main__":
    main()
