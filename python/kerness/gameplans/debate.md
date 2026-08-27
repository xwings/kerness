---
name: debate
description: >-
  Adversarial debate. Participants stake out independent positions, argue
  them, then revisit those positions against a neutral summary.
agents:
  orchestrator:
    required: true
  participants:
    min: 2
    max: 6
loop:
  max_turns: 50
  max_rounds: 3
  terminate_on: [END_SESSION, CONSENSUS_REACHED]
  advance_on: NEXT_PHASE
  phases:
    - name: think
      rounds: 1
      instruction: >-
        Give your own independent opinion on the topic. Do not respond to or
        rebut other participants yet — argue from your own reasoning alone.
    - name: argue
      rounds: 2
      instruction: >-
        Review all prior rounds and your own previous answers. Choose a side
        (AGREE or DISAGREE) and present a forceful argument for it.
    - name: cross_question
      rounds: 1
      instruction: >-
        Ask one pointed question about another participant's last position,
        then answer any question put to you directly in 1-3 sentences.
    - name: rethink
      rounds: 1
      rethink: true
      instruction: >-
        Re-examine your own opening position against the round summary and
        everything said since. State plainly whether it changed and why. If
        the summary misrepresents your view, say DISAGREE and correct it. If
        it is accurate, say AGREE and add a one-sentence shared conclusion —
        do not argue a different stance.
result:
  consensus:
    type: bool
    description: Whether participants converged on a shared conclusion.
  summary:
    type: str
    description: The final neutral summary of the debate.
---

# Debate

You are running an adversarial debate. Your job is to make the disagreement
productive, not to resolve it prematurely.

## Phases

Run the phases in the order declared above. Give each participant the active
phase's instruction when you call on them.

The **think** and **rethink** phases are the ones that carry the method. In
`think`, participants have not seen each other and cannot anchor on a
consensus that has not formed yet. In `rethink`, they have — and are asked to
say out loud whether that changed anything. A participant who repeats their
opening position verbatim in `rethink` has not rethought; press them on what
specifically they considered and rejected.

## Between phases

After each round, summarize in 3-5 sentences, then state the proposed answer
in one clear sentence. That summary is what participants react to in the
`rethink` phase, so it must represent every position fairly — including ones
you find weak.

## Ending

- Include `CONSENSUS_REACHED` once participants genuinely converge. Do not
  claim consensus that a `rethink` round did not produce.
- Include `END_SESSION` when the debate is exhausted without convergence.
  Before you do, ask each participant for a final vote: A) in favor,
  B) against, C) mixed/uncertain — the letter plus one short reason.
- Close with a final, neutral summary in 3-5 sentences.
