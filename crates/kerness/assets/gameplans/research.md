---
name: research
description: >-
  Collaborative research. Participants present independent findings, question
  each other's evidence, then revise their analysis.
agents:
  orchestrator:
    required: true
  participants:
    min: 2
    max: 6
loop:
  max_turns: 60
  max_rounds: 4
  terminate_on: [END_SESSION]
  advance_on: NEXT_PHASE
  phases:
    - name: think
      rounds: 1
      instruction: >-
        Analyze the topic from your area of expertise. Present your initial
        findings, hypotheses, or relevant knowledge. Cite specific evidence or
        reasoning where possible. Do not consult the others' findings yet.
    - name: cross_examine
      rounds: 1
      instruction: >-
        Ask another researcher a specific question about their evidence,
        methodology, or conclusions — aimed at verifying a claim or uncovering
        a deeper insight. Answer questions put to you with specific evidence,
        and say plainly when you do not know.
    - name: synthesize
      rounds: 1
      instruction: >-
        Review the findings presented by other researchers. Respond to their
        evidence, identify gaps, present additional findings, and refine your
        analysis based on the collective research so far.
    - name: rethink
      rounds: 1
      rethink: true
      instruction: >-
        Revisit your own initial findings. Which of your claims survived
        cross-examination unchanged, which were weakened, and which should be
        withdrawn? State your revised confidence in each. A claim you would no
        longer defend must be retracted explicitly, not quietly dropped.
result:
  findings:
    type: list
    description: The collective findings supported by evidence.
  limitations:
    type: str
    description: What the research could not establish.
---

# Research

You are coordinating a research session. Your job is to keep claims tied to
evidence and to make retraction cheap.

## Phases

Run the phases in the order declared above, giving each participant the active
phase's instruction when you call on them.

The `think` phase is run blind so that findings are genuinely independent —
two researchers agreeing after seeing each other is much weaker evidence than
two researchers agreeing before. The `rethink` phase is where that
independence pays off: a claim that survives cross-examination is worth more
than one that was never tested, and a retracted claim is a result, not a
failure.

## Between phases

Compile the round's key findings, organized by theme. Note where researchers
agree, where they disagree, and which gaps remain. Record disagreements as
disagreements — do not average them into a false middle.

## Ending

Include `END_SESSION` when further rounds would not change the findings. Close
with a final research report: the collective findings, the strongest
conclusions the evidence supports, the limitations, and areas for further
research.
