#!/usr/bin/env python3
"""Three-agent web research using agent-browser and a research gameplan."""

from __future__ import annotations

import os
import sys

import kerness


def main() -> None:
    api_key = os.environ.get("OPENAI_API_KEY", "")
    if not api_key:
        print("Set OPENAI_API_KEY environment variable first.")
        sys.exit(1)

    # policy = kerness.AccessPolicy(
    #     auto_approve_prefixes=["agent-browser"],
    # )

    session = kerness.Session(
        gameplan="research",
        topic=(
            "Sub-agent: definition and current development status.\n\n"
            "Each participant must use the agent-browser skill to "
            "guide their web research workflow, use Google for search, open "
            "2-3 sources, then cite them with detailed page information "
            "(page title + full URL). The "
            "orchestrator should synthesize a final summary."
        ),
        provider=kerness.OpenAIProvider(api_key=api_key),
        channel=kerness.ConsoleChannel(),
        access_policy=None,
    )

    session.exec = ["agent-browser*"]

    session.add_agent("Alex", model="gpt-5.1", persona="Systems researcher")
    session.add_agent("Bo", model="gpt-5.2", persona="AI product analyst")
    session.add_agent("Chen", model="gpt-5.1", persona="ML engineer")
    session.add_agent("Lead", model="gpt-5.2", role="orchestrator")

    session.add_skill("summarize")
    session.add_skill("fact-check")
    session.add_skill("agent-browser")


    result = session.run()

    print("\n--- Result ---")
    print(f"Topic: {result.topic}")
    print(f"Turns: {result.turns_completed}")
    if result.summary:
        print(f"Summary: {result.summary}")


if __name__ == "__main__":
    main()
