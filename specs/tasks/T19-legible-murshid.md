# T19 — "Legible murshid" (the Working Backwards UX queue)

**Status:** Drafted 2026-07-09 — founder decisions taken same day (see below);
next ladder after the T17 merge (`f4db7f7`). Builds on a branch
(`yasir/t19-legible`), serial + gated per rung.
**Traces to:** the Working Backwards diagnosis (private:
`dev-context/specs/murshid/research/wb-gap-analysis-2026-07.md`, from the
PR/FAQ + user-manual + CX-walkthrough pack, all findings spot-verified at
source), the T17 dogfood findings (2026-07-09, in the T17 spec), and the
2026-07-08 vision confirmations (mentor-grade quality; passive stream; no
quiz surface).

## Why

Founder dogfooding hit the response keys and couldn't tell what they do —
and the Working Backwards pass showed that's one instance of a theme: **the
product's four load-bearing promises are currently illegible or false at
the surface.** A fresh user dies at setup (two-vendor default, retired
model id, setup that reports success without checking); a working user
can't distinguish healthy silence from a dead judge (health frozen at
startup); the memory loop never shows itself working (no credit for earned
quiet, no undo, graduation reads as the product dying); and "any model" is
untrue for reasoning/local models. None of this was caused by T17 — the
rung gates verified what was built; these are gaps in what was never built.

## Founder decisions (2026-07-09 — these override older spec text)

- **`g` (got it) seeds a spaced concept window** (non-decaying, ~30d
  strawman — config-grade), like `u`'s but gentler. It does NOT write BKT
  evidence: I22/I23 ("only *doing* is mastery evidence") stays intact.
- **The pre-TUI stdin recall quiz is REMOVED** (T5-era `watch` startup
  loop). The retrieval/staleness machinery underneath stays (it drives
  spaced resurfacing); only the blocking question-and-answer surface goes.
  Reaffirms 2026-07-08 "passive stream suffices; no quiz surface."
- **Out-of-box default = ONE cloud provider** (Gemini both slots:
  flash-lite screen / flash judge — one key, free tier). `murshid setup`
  verifies the key live and reports exactly what it found/what's missing.

## Ladder (each rung ships a visible increment; implementer → adversarial gate)

- **R0 — runnable out-of-box (Q1).** One-provider default config; retire
  the dead model id; `setup` does a live key check + honest report
  (found/missing/verified); README quickstart matches reality.
- **R1 — live judge health (Q2).** Degraded state recomputed at runtime
  from consecutive dispatch failures (threshold config-grade); the R1-T17
  band chip carries the last error string (post-`show-error` detail);
  recovery flips it back. Silence while degraded is now visibly different
  from healthy quiet.
- **R2 — self-teaching responses (Q3).** `?` help gains a one-clause
  memory-consequence per response key; the first-ever card (per profile)
  carries a one-time vocabulary line; `g` seeds its spaced concept window
  (decision above). Keybar labels unchanged (width is finite; help is the
  teaching surface).
- **R3 — quiz removal (Q4).** Delete the pre-TUI recall loop per the
  decision above; verify week-2 launch goes straight to the TUI.
- **R4 — visible memory + graduation (Q5).** Idle-banner credit lines
  ("quiet on N mastered concepts", "backing off X — applied 3×");
  resurfaced cards say "back after N days"; concept detail lists active
  quiets/mutes with expiry; revert-during-ack-flash undo for a mistyped
  response; bookend renders as the TUI's final frame (not post-exit
  stdout); mastery reachable as a transient over a card.
- **R5 — BYO-model honesty (Q6).** Per-slot `timeout` + `max_tokens`
  config keys (defaults = today's 60s/1024); documented reasoning-model
  guidance in README/config comments.
- **R6 — silence legibility + polish (Q7+Q9).** Muted state owns the idle
  banner (never "watching" while muted); cooldown wording
  checks-not-promises; jargon pass (`min_gap`→"hint spacing" label,
  "judge degraded"→plain words, raw `rung N`→the T15 depth names);
  `n`-widening advertised at press time; settings-persist writes only the
  cycled value (clobber fix); `e/E` collision resolved (events → `:events`
  + rail, `E` freed or kept with explicit chip).

## Invariants
I22/I23 untouched (R2's `g` window is suppression, not evidence). No new
dependencies. TUI-confined where the fix is presentational; engine changes
only where named (R1 health, R2 `g`, R3 removal, R5 config). Nothing here
reopens T17's gated behavior — additive legibility only.

## Config-grade (C1)
`g` window length (~30d) · degraded-mode failure threshold (~3 consecutive)
· per-slot timeout/max_tokens · first-card vocabulary line (one-shot flag).

## Needs founder input (non-blocking; strawmen chosen)
The `g` window length; whether `E` stays after `:events` exists; the
degraded threshold; R4's exact banner wording (tune by dogfood).
