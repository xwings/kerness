#!/usr/bin/env python3
"""Texas Hold'em example — 3 players with distinct personalities."""

import json
import os
import re
import sys
from pathlib import Path

import kerness

#: Every asset this example loads sits beside it. Resolving from ``__file__``
#: rather than the working directory is what lets it run from anywhere: a
#: cwd-relative path works only from the repository root, and a persona that
#: fails to resolve is a hard error rather than a silently plainer agent.
HERE = Path(__file__).resolve().parent


class RecordingChannel(kerness.ConsoleChannel):
    def __init__(self) -> None:
        super().__init__()
        self.events: list[dict[str, str]] = []

    def send(self, sender: str, content: str) -> None:
        self.events.append({"sender": sender, "content": content})
        super().send(sender, content)

    def send_system(self, content: str) -> None:
        self.events.append({"sender": "system", "content": content})
        super().send_system(content)


def _parse_stacks(line: str) -> dict[str, int]:
    # Expected format: "Stacks: Alice=1000, Bob=950, Carol=1050"
    _, _, rest = line.partition(":")
    stacks: dict[str, int] = {}
    for part in rest.split(","):
        part = part.strip()
        if not part:
            continue
        name, _, amt = part.partition("=")
        name = name.strip()
        amt = amt.strip()
        if name and amt.isdigit():
            stacks[name] = int(amt)
    return stacks


def build_hand_log(events: list[dict[str, str]]) -> list[dict[str, object]]:
    hand_logs: list[dict[str, object]] = []
    current: dict[str, object] | None = None
    for ev in events:
        content = ev["content"]
        if "HAND " in content and " START" in content:
            match = re.search(r"HAND\s+(\d+)\s+START", content)
            if match:
                hand_no = int(match.group(1))
                funds: dict[str, int] = {}
                for line in content.splitlines():
                    if line.startswith("Stacks"):
                        funds = _parse_stacks(line)
                        break
                current = {
                    "round": hand_no,
                    "funds": funds,
                    "process": [],
                    "result": {},
                }
                hand_logs.append(current)
            continue
        if current is None:
            continue
        current["process"].append({
            "sender": ev["sender"],
            "content": content,
        })
        if "HAND " in content and " END" in content:
            for line in content.splitlines():
                if line.startswith("Stacks"):
                    current["result"] = _parse_stacks(line)
                    break
            current = None
    return hand_logs


def _extract_final_json(events: list[dict[str, str]]) -> list[dict[str, object]] | None:
    # Dealer should emit: "FINAL_JSON: <json>"
    for ev in reversed(events):
        if ev["sender"] != "Dealer":
            continue
        content = ev["content"]
        if "FINAL_JSON:" not in content:
            continue
        _, _, payload = content.partition("FINAL_JSON:")
        payload = payload.strip()
        if not payload:
            continue
        try:
            return json.loads(payload)
        except json.JSONDecodeError:
            return None
    return None


def main() -> None:
    api_key = os.environ.get("OPENAI_API_KEY", "")
    if not api_key:
        print("Set OPENAI_API_KEY environment variable first.")
        sys.exit(1)

    channel = RecordingChannel()
    session = kerness.Session(
        gameplan=str(HERE / "gameplan" / "texas_holdem.md"),
        topic=(
            "三人德州扑克多轮牌局，请一直模拟到决出最终胜者（所有筹码集中到一人），"
            "或最多进行20手牌后结束。Dealer 必须在每个下注回合逐一询问每位成员的"
            "决策意见（@Name），不能跳过。"
        ),
        provider=kerness.OpenAIProvider(api_key=api_key),
        channel=channel,
        max_rounds=20,
        max_turns=200,
        system_prompt=(
            "You are a poker player in a simulated Texas Hold'em hand. "
            "Follow the dealer's instructions. Reply with one action and amount "
            "if needed, plus a brief rationale."
        ),
    )

    session.add_agent(
        "Leo",
        model="gpt-5.2",
        persona=str(HERE / "personas" / "aggressive_bluffer.md"),
    )
    session.add_agent(
        "Mina",
        model="gpt-4o",
        persona=str(HERE / "personas" / "tight_conservative.md"),
    )
    session.add_agent(
        "Kai",
        model="gpt-5.1",
        persona=str(HERE / "personas" / "analytic_pro.md"),
    )
    session.add_agent(
        "Dealer",
        model="gpt-5.1",
        role="orchestrator",
        persona="Professional poker dealer and fair referee",
    )

    result = session.run()

    print("\n--- Result ---")
    print(f"Topic: {result.topic}")
    print(f"Turns: {result.turns_completed}")
    if result.summary:
        print(f"Summary: {result.summary}")
    hand_logs = _extract_final_json(channel.events)
    if hand_logs is None:
        hand_logs = build_hand_log(channel.events)
    print("\n--- JSON ---")
    print(json.dumps(hand_logs, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
