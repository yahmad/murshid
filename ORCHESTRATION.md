# Orchestration

The main Claude Code session is the **orchestrator**. Run it on Opus
(`/model opus` if needed). The orchestrator never writes code directly — it
plans, delegates, and adjudicates.

## The loop, per spec

1. **Read** the active spec (specs/INDEX.md → the referenced file).
2. **Plan**: break it into small implementation units.
3. For each unit:
   a. Delegate to the **implementer** subagent.
   b. When it reports done, delegate to the **reviewer** subagent.
   c. If the reviewer returns CHANGES NEEDED, hand the list back to the
      implementer. Loop until APPROVE.
   d. Mark the unit complete; move to the next.
4. When all units are APPROVE and `go test ./...` is green, stop.

## Why this shape

- The implementer (Sonnet) is cheap and fast for bulk code.
- The reviewer (Opus) is read-only — it cannot silently fix and then bless its
  own work, which is the failure mode that makes self-grading worthless.
- The orchestrator (Opus) is the only one that decides a unit is done, and it
  decides on the reviewer's cited evidence, not on the implementer's assertion.

## Where Gemini fits (separately)

Do NOT route Gemini's coding through this loop (its diffs fight Claude Code's
edit format). Instead:
- **Antigravity**: open this repo for a long-context, whole-repo architecture
  pass before you write a big spec, and as an independent second-opinion review
  ("review this implementation against specs/0001 and list risks").
- That uses your Google AI Pro sub directly — no API key, no router.

## Starting the loop

> You are the orchestrator. Read specs/INDEX.md and the active spec. Plan it into
> units, then for each unit delegate implementation to the `implementer` subagent
> and verification to the `reviewer` subagent, looping until the reviewer
> approves. Do not write code yourself. Stop when all units pass review and
> `go test ./...` is green.
