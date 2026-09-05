#!/usr/bin/env python3
"""Drive the Rust kernel one step at a time, with no key or network.

Run after installing the Python binding:
    python bindings/python/examples/host_control.py
"""

from pathlib import Path
from tempfile import TemporaryDirectory

import kerness

GAMEPLAN = """---
name: host-control
agents:
  orchestrator: false
  participants: {min: 1, max: 1}
loop:
  max_turns: 4
  max_rounds: 2
  terminate_on: [DONE]
result:
  summary: {type: str}
---

Answer the host's request.
"""


class Scripted(kerness.Provider):
    def __init__(self):
        super().__init__(retries=0, backoff_sec=0)
        self.calls = 0

    def chat(self, model, messages):
        self.calls += 1
        if self.calls != 1:
            raise RuntimeError("Unexpected extra provider call")
        return kerness.ProviderResponse(
            content="Write-through keeps the invalidation rules simple.", model=model,
        )


def main():
    with TemporaryDirectory(prefix="kerness-host-control-") as directory:
        workspace = Path(directory)
        gameplan = workspace / "gameplan.md"
        gameplan.write_text(GAMEPLAN, encoding="utf-8")
        provider = Scripted()
        session = kerness.Session(
            gameplan=str(gameplan), topic="Which cache policy should we use?",
            provider=provider, model="offline-model", turn_delay_sec=0,
            memory=str(workspace / "memory.md"),
            access_policy=kerness.AccessPolicy(workspace=workspace),
        )
        session.add_agent("Advisor")
        run = session.start(
            mode="host_driven", budget={"max_provider_operations": 1},
            event_sink=lambda event: print(
                f"event {event['sequence']}: {event['event']['kind']}"
            ),
        )
        # start consumes session. Only the independent run handle is used now.
        asked = False
        while True:
            step = run.step()
            if step["status"] == "progress":
                continue
            if step["status"] != "waiting" or step["reason"]["kind"] != "input":
                raise RuntimeError(f"Unexpected step: {step}")
            if not asked:
                asked = True
                run.step({
                    "kind": "select_agent", "agent": "Advisor",
                    "instruction": "Recommend one cache policy.",
                })
                continue
            # Rust validates this shape. Finish makes no provider/judge call.
            finished = run.step({
                "kind": "finish", "result": {"summary": "Use write-through caching."},
            })
            assert finished["status"] == "finished"
            outcome = finished["outcome"]
            assert outcome["reason"]["kind"] == "completed"
            assert outcome["diagnostics"]["valid"]
            assert provider.calls == 1
            print("Result:", outcome["result"]["fields"]["summary"])
            print("Usage:", outcome["usage"])
            break


if __name__ == "__main__":
    main()
