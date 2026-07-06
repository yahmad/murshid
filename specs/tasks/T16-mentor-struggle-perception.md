# T16 — Mentor-grade struggle perception + model-directed context

**Status:** Drafted 2026-07-06 — **pending founder go before it becomes active**.
**Traces to:** SPEC v0.5 (C6 two-stage judge · D15 struggle signals · C2 advice
window · D9 precision gate · C7/D16 scheduling+budget · invariants I1/I4/I7/I9/
I10/I11/I14), the realignment audit (the Socratic-core gap), and
[`explorations/tui-redesign-2-workspace.md`](../explorations/tui-redesign-2-workspace.md)
(the surfaces this lands on — the `k` conversation shipped there as R5).

## Why

Founder dogfeedback: *"the feedback isn't mentor-like or smart enough; it isn't
understanding when I'm struggling."* Two root causes:

1. **Perception is mechanical.** Struggle today = an error-streak (3× the same
   E-code) **or** time-in-red past a baseline **or** a literally-typed "help"
   comment. Those blunt proxies miss the *shape* of being stuck — circling one
   function, revert loops, half-finished approaches, reading-not-editing — and
   they never name *what concept* you're stuck on (the offer only knows an error
   code).
2. **Judgment is context-starved.** The judge sees exactly one tree-sitter
   enclosing item (C6). A problem whose real cause is the caller, a trait
   definition, or another file is judged blind.

T16 replaces the proxies with a model that reasons over the *accumulated,
temporal* picture of your work, and lets the judge pull the context it decides
it needs — while staying inside the same calm, non-modal, budgeted contract.
This is the Socratic core the realignment audit named as missing.

## Founder decisions (2026-07-06) — baked in

- **Edit log (build it now).** Capture a content-level edit history — the *path*
  of edits (revert loops, rewrites), not just the two-point session snapshot. A
  written-then-reverted line is invisible to a two-point diff, yet that churn is
  often exactly where struggle lives. Bigger lift, chosen as the truest signal.
- **Fire alone when confident.** A HIGH-confidence, concept-named perception
  judgment may fire an offer on its own — no mechanical co-signal required —
  backstopped by the existing throttle/suppression economy that auto-quiets the
  channel if it's wrong too often. (Low/medium confidence must still co-fire
  with a mechanical signal.)
- **Conversation is already shipped** (redesign R5: `k` → threaded follow-up),
  so T16's "response" rung is the struggle-response *upgrade*, not conversation.

## The ladder (each rung ships a user-visible increment)

### T16a — Model-directed judge context
The stage-1 screen model (which already sees the diff) emits a bounded
**context request**: typed asks — `{symbol: name}`, `{caller: fn}`,
`{range: file,start,end}`, `{file: path}` (cap ~4, config-grade). A new
tree-sitter-backed **resolver** (in `site.rs`) fetches those exact ranges and
appends them to the judge context: **the model decides *what*, tree-sitter
fetches *precisely*.** A **judge-context budget** (config-grade, ~24k tokens —
generous given the 150k local window, but bounded) fills the enclosing item
first, then requests by priority, dropping over-budget asks with a T14 trace
note. Grounding (`quote_is_grounded`) verifies against the **full provided
(possibly multi-file) context**, not just the anchor file — the guarantee gets
*stronger* while staying honest. *Visible:* correct cards that cite the caller /
trait def / other file on the same moments murshid already speaks on. Lowest
risk, no new interruption.
- **Amends C6** (stage-1 gains a context-request output leg; stage-2 gains
  model-directed extra context) and **D9** (grounding vs full context; new
  invariant: *tree-sitter fetches, the model decides* — resolution stays
  deterministic and grammar/pack-driven).

### T16b — Edit-log capture + temporal struggle perception
(a) **Edit log:** capture content-level edit events per file per session
(a bounded append log / ring buffer of edit deltas + timestamps) so revert and
rewrite churn is visible — the substrate the two-point `compute_session_diff`
lacks. (b) **Perception pass:** a cheap-model pass reasons over `{accumulated
session diff + the edit-log churn + a signal summary (error/red streaks, recent
check history, touch cadence) + goal + tracked below-mastery concepts}` and
emits `{ stuck: bool, confidence: 0..1, site_hint?, concept?: <taxonomy slug or
null>, one_line_evidence }`. It **proposes**; the existing offer gate
(`run_poll_loop`) **disposes** — idle gate, `already_offered`, D12 throttle,
cross-session suppression, one-offer-at-a-time, expire-on-typing (I10) are all
unchanged. Anti-nag: a **confidence floor** (config-grade, strawman ≥0.7); a
high-confidence + concept-named judgment may fire alone (reading the founder
decision as: the model's judgment *is* the convergence of the signals it reasoned
over, honoring I14's "no prompt on a single naked inferred signal"); low/medium
must co-fire with a mechanical signal. The offer line becomes concept-named:
*"looks like you're circling ownership in `parse_config` — hint? [y/N]"* (still
one line, still `[y/N]`, still I9 offer-never-act, I11 evidence-named).
- **Amends D15/C12** (a model-perception signal augments the mechanical
  convergence as the thing that decides to speak) + a **new edit-log contract**
  + a **new invariant**: *perception is a candidate generator only — it never
  bypasses the offer gate or writes a notice/card directly.*

### T16c — Perception-directed struggle RESPONSE + "since last spoke" baseline
Rebuild the struggle-accept response (`keys::run_struggle_judge_and_show`) on
T16a's model-directed context and T16b's accumulated-diff framing, so the
post-"yes" hint is about the *whole struggle arc*, not one hunk — the most
direct fix for "not mentor-like." Add a rolling **"since murshid last engaged"
baseline** (a second per-file snapshot stamped when murshid last spoke) so a
second offer/response reasons about *new* struggle rather than re-teaching. The
`k` conversation (shipped R5) inherits the richer context automatically.
- **Amends C2** (a perception baseline distinct from the I1 session-start card
  advice-window, which is untouched).

### T16d (optional) — Perception into memory + calibration
Feed the temporal judgment into the D14 session bookend (*"you circled
`parse_config` for 20 min on lifetimes"*) and as a BKT hard/struggle signal;
calibrate the confidence floor against founder-labeled examples. *Visible:* the
recap and mastery meter reflect struggle, not just cards shown.

## Riskiest tensions (must hold, or the design fails)
1. **Perception vs ambient/non-modal (the Clippy risk).** Perception is ONLY a
   candidate generator; the confidence floor + the I14-preserving rule + the
   untouched idle/throttle/suppression/one-at-a-time gate are the sole
   authority. This is an invariant — a builder must not wire perception straight
   to `ws.notice`/a card.
2. **Cost/latency of an extra model pass.** Mitigate: run perception on the
   cheap screen model, and only when a cheap mechanical pre-gate is warm (any
   red streak, or churn above base rate, or a help comment) — quiet green work
   costs nothing; OR fold it into the existing stage-1 call. Recommended: T16b
   ships the session-level pass gated by a warm pre-gate.
3. **False positives that nag.** I4 forbids naked confidence gates — the floor
   is necessary-not-sufficient, backstopped by the throttle/suppression EFP
   economy (the channel auto-quiets if its action rate drops below the floor).
4. **Grounding drift with multi-file context.** Resolved context blocks carry
   `file:line` headers; the card anchor must match the block the grounding quote
   was found in.

## Out of scope
The conversational UI (shipped, R5); any NEW proactive surface (perception routes
through the EXISTING offer/card machinery — D16 budget); the BYOK consent gate
(a separate go-public decision, currently deferred).

## Config (config-grade per C1 — tunable without a spec amendment)
Confidence floor (~0.7) · judge-context token budget (~24k) · context-request
cap (~4) · edit-log retention bound. Founder should sanity-check starting values
against dogfood.

## Tests (approach)
Pure: the context-request resolver (symbol/caller/range → exact text; over-budget
drop), grounding-vs-full-context, the perception output schema + gate routing
(proposes-not-disposes), edit-log capture + ring-buffer bound, confidence-floor
gating, the fire-alone-vs-must-co-fire rule. DB-fixture: edit-log persistence,
the "since last spoke" baseline. Live-dispatch bits (the actual model call)
aren't unit-testable — mirror the comment-ask coverage note.

## Open question for the founder (sharpens T16b)
Concrete *"here's a struggle murshid missed"* examples would calibrate the
edit-log + perception design — e.g. "I rewrote `detect_level` four times over ten
minutes and it stayed silent," or "I left a `// this is wrong` and kept editing
around it." The edit-log decision already commits us to capturing revert loops;
real examples would tune the confidence floor and the pre-gate.
