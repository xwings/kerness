#!/usr/bin/env python3
"""Open discussion example — orchestrator-driven flow."""

import os
import sys

import kerness


def main() -> None:
    api_key = os.environ.get("OPENROUTER_API_KEY", "")
    if not api_key:
        print("Set OPENROUTER_API_KEY environment variable first.")
        sys.exit(1)

    session = kerness.Session(
        gameplan="discussion",
        topic="What makes a good programming language?",
        provider=kerness.OpenRouterProvider(api_key=api_key),
        channel=kerness.ConsoleChannel(),
    )

    session.add_agent("Alice", model="openai/gpt-4o", persona="Systems programmer who values performance")
    session.add_agent("Bob", model="anthropic/claude-sonnet-4", persona="Web developer who values developer experience")
    session.add_agent("Carol", model="openai/gpt-4o", persona="Academic who studies programming language theory")
    session.add_agent("Facilitator", model="openai/gpt-4o", role="orchestrator")

    session.add_skill("summarize")

    result = session.run()

    print("\n--- Result ---")
    print(f"Topic: {result.topic}")
    print(f"Turns: {result.turns_completed}")
    if result.summary:
        print(f"Summary: {result.summary}")


if __name__ == "__main__":
    main()
