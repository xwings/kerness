---
name: discussion
description: >-
  Open exploratory discussion. Participants share perspectives, build on each
  other, then revisit their own opening view.
agents:
  orchestrator:
    required: true
  participants:
    min: 2
    max: 8
loop:
  max_turns: 50
  max_rounds: 5
  terminate_on: [END_SESSION]
  advance_on: NEXT_PHASE
  phases:
    - name: think
      rounds: 1
      instruction: >-
        Share your perspective on the topic. Present your thoughts openly and
        consider multiple angles. There is no need to take a firm side. You
        have not seen the others yet — say what you actually think.
    - name: explore
      rounds: 3
      instruction: >-
        Review what other participants have said. Build on interesting points,
        offer new perspectives, and explore areas of agreement or nuance. You
        do not need to argue for or against — focus on deepening the
        conversation.
    - name: rethink
      rounds: 1
      rethink: true
      instruction: >-
        Return to the perspective you opened with. What in this discussion
        changed it, sharpened it, or left it untouched? Name the specific
        point that did the work. "Nothing changed my view" is a valid answer
        only if you can say what you considered and why it did not land.
result:
  summary:
    type: str
    description: The comprehensive summary of the discussion.
  open_questions:
    type: list
    description: Questions the discussion surfaced but did not settle.
---

# Discussion

You are running an open discussion. The goal is depth, not agreement — you are
not steering toward a verdict.

## Phases

Run the phases in the order declared above, giving each participant the active
phase's instruction when you call on them.

The `think` phase exists so that the first thing said is not the thing
everyone anchors to. The `rethink` phase exists because a discussion where
nobody's view moves is a set of parallel monologues. Both are cheap to skip
and expensive to have skipped.

## Between phases

Summarize the key themes and perspectives that emerged in the round. Highlight
areas of agreement, interesting tensions, and open questions. Tensions are
worth more than agreements here — name them explicitly rather than smoothing
them over.

## Ending

Include `END_SESSION` when the discussion has stopped producing new ground.
Close with a comprehensive summary: the main perspectives shared, the key
insights that emerged, and what remains open.
