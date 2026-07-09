# T17 — Passive learning hint-stream (redesign round 3)

**Status:** **Done — MERGED to main 2026-07-09** (`f4db7f7`; R0–R6 all gated; 942+1 tests). The 2026-07-09 dogfood findings below feed T19.
**Traces to:** SPEC v0.5 (I9 offer-never-acts, I10 non-modal offers, D10 frequency knob, D15 struggle signals, D16 scheduling+budget, the surfacing C-number), the round-3 design pass (2026-07-07), and founder dogfood notes on the T16 branch (2026-07-06/07). Amends I9/I10/D10/D16 + the surfacing contract (exact C/I numbers live in the private design-authority SPEC — **founder to map**).

## Why
Founder dogfooding T16: the *ask-permission, one-card-hero* model is friction. The TUI is a **passive side panel**, so a hint is not a modal interruption — just show it. Embrace proactive hinting, made good (unlike Clippy, which was modal, dumb, and never learned) by **memory + feedback**: low-value hints suppressed, useful patterns reinforced on a **spaced** schedule. And the layout should be a hint **stream** (history-as-home) with split-panes, not a card-hero + fullscreen overlays.

## Founder decisions (2026-07-06/07)
- **Drop the offer/consent dialogue → ALWAYS hint directly** (no `[y/N]`).
- **Anti-repeat = DECAYING + SPACED, not never-repeat.** Repetition is a reinforcement *feature*; muting is temporary and only for the *annoying/rejected*:
  - `u` (not useful) → quiet ~14d, then **may resurface if still unmastered**; a repeat `u` → longer.
  - ignored K× with no action → quiet a few days.
  - back-to-back same concept → short cooldown.
  - `g`/`a` (got it / mastered) → quiet **until it goes stale or the user regresses**, then resurface for reinforcement (driven by BKT mastery + `staleness.rs`).
- **Positive feedback:** a light **`👍 useful / more like this`** key on a hint (alongside `a`/`g`/`u`) — the explicit positive signal that reinforces a concept and feeds the channel-health metric (the offer's "yes" is gone).
- **Frequency detent → a single `min_gap` cooldown.** Default **~8 min**, live-tunable, `off` = mute proactive hints entirely (kill switch).
- **Slim chrome:** drop the goal from the ambient band; the "next nudge" becomes a cooldown countdown; model-state is **silent when healthy**, loud only on a problem.
- **History-stream is home:** the main pane is the scrollable stream of hints; the hero card is only the empty/pre-input state; a selected hint opens in a split detail pane; Events becomes a **debug split-pane** (more per-event info); prefer split-panes over fullscreen switching.

## The crux — anti-repeat that survives IGNORE (not just decline)
Today's anti-repeat was built for a *decline-driven* world. Under always-hint, nobody declines — they **ignore**. So the load-bearing test for the whole design is: **a dismissed or ignored hint must not come back across sessions** (except as deliberate spaced reinforcement). Four gaps found in the design pass that R2 MUST close, before the always-hint flip:
1. `card_exists_with_advice_fp` is **session-scoped** — an ignored hint re-fires next session.
2. The proactive card uses a **synthetic fp** (`struggle-offer:{signal}:{concept}`); the real `advice_fingerprint(concept, site)` only exists after accept — so the cross-session ledger block never applies to hints. → the streamed hint must carry the **real fp up front**.
3. `not_useful` suppresses only the **site**, not the concept — the same concept re-fires elsewhere.
4. The 2-declines→`offer-concept` suppression is **dead** (no decline exists).

The composed rule (a hint for concept C at site S will NOT fire if): same-session fp exists; OR fp was dismissed Applied/GotIt/NotUseful; OR C is mastered (until stale/regressed); OR C is in a decaying `hint-concept` suppression; OR C's category is D12-throttled; OR the min_gap hasn't elapsed. **Spaced resurfacing is the deliberate exception**: mastery + staleness decide when a quieted concept is *due* to return.

New memory (on the existing `suppressions` table + a small shows-tally; no heavy schema churn):
- **`hint-concept` suppression** — cross-session, concept-scoped, **decaying** window (seeded by `u`, or by K silent ignores).
- **cross-session shown-and-ignored counter** — after K silent shows of an fp with no positive action, auto-suppress (handles the *ignored* case, the common one for a passive panel; `expired` is not ledger-blocking today, which is why the same hint returns — this closes it).

## Ladder (each rung ships a visible increment; implementer → adversarial gate)
Ordered so the **learning memory (R2) lands before the always-hint flip (R3)** — otherwise it's Clippy.
- **R0 — cooldown replaces frequency.** `noise.rs` Detent + quiet/standard/chatty + `WatchSession::frequency` → a single `[dial] min_gap` (capacity-1 `TokenBucket`, refill = min_gap; keep the struct, drop the detent wrapper). Band shows `next hint in ~Nm`. Settings drops frequency, gains `min_gap` (+ `off`). No behavior flip. Config default 8m.
- **R1 — model-state silent when healthy.** Drop the always-on `judge live`; render loud only on degraded/unreachable/rate-limited.
- **R2 — the learning memory (the hard rung — gate hardest).** Unify the proactive card onto the real advice_fp; add the `hint-concept` decaying cross-session suppression (`u`-driven + K-ignore-driven); add the shown-and-ignored counter; wire spaced resurfacing off mastery+staleness. Acceptance: **dismiss or ignore a hint, prove it does not come back next session — but a still-unmastered concept resurfaces after its window.**
- **R3 — offer → direct hint.** Delete the `[y/N]` consent (`PendingOffer`, `classify_offer_key`, accept/decline); perception/struggle/sweep produce a card **directly** via the cooldown+memory gate, running the existing `run_struggle_judge_and_show` inline at gate-pass (not on accept). `select_perceived_candidate`'s confidence-floor / fire-alone rule stays the FP filter. Add the `👍 useful` key + its positive signal. Safe only because R2 shipped.
- **R4 — history-stream as home.** The stream list becomes the main pane; hero card = empty state only.
- **R5 — split-pane detail + inline ask.** Selected-row detail pane replaces the fullscreen reader; `k` conversation inline.
- **R6 — events split-pane + slim chrome.** Events debug split (per-event payload/trace); 1-row keybar; goal out of the band.

## Spec amendments
- **I9** recast: the offer channel is gone; the spirit stays — **hints never act on the user's code, they inform** (no auto-fix/edit).
- **I10** replaced by a new invariant — **"Passive, always-visible, feedback-suppressed":** a hint (a) appears non-modally in the stream without stealing focus/keystrokes, (b) is min_gap-spaced (no consent), (c) is feedback-suppressed so a dismissed/repeatedly-ignored concept won't re-fire across sessions except as deliberate spaced reinforcement. **Learning, not consent, is the anti-nag mechanism.** (T16b's "perception is a candidate-generator only" invariant survives verbatim — the confidence gate is still the FP filter; only its downstream changes.)
- **D10** amend: frequency detent → single `min_gap`.
- **D16** amend: capacity-1 bucket, refill = min_gap, shared by all proactive surfaces; drop the borrow/accept-preempt path; **keep** the likely-bug strict-mode bypass (a verified bug preempts the cooldown).
- **D15** amend: perception produces a hint directly; the confidence-floor + I14-preserving co-fire rule remain the sole FP authority.
- **Surfacing contract** amend: "at most one card; the offer never occupies the slot" → "a stream of hint cards; the newest is the live/highlighted entry; older entries + threads scroll." (Private-SPEC C-number — founder to map + assign the new invariant's number.)

## Config (config-grade per C1 — tunable without amendment)
`min_gap` = 8 min (off = mute) · `u`-suppress window ~14d, decaying (repeat `u` → longer) · ignored-K threshold before auto-suppress · got-it spaced-resurface window (the knob the founder will tune by feel: how long a mastered/dismissed concept stays quiet before it's due) · the `👍` positive-signal weight.

## The single biggest risk + mitigation
Always-hint amplifies every bad hint, and a weak learning loop = hated Clippy. Mitigation is entirely in **R2, before the flip**: real advice_fp on hints (cross-session block applies), the shown-and-ignored counter (handles ignore, the common case), `u`→concept-scope suppression, D12 as the channel circuit-breaker (consider a shorter window / mid-session recompute), and the `min_gap = off` kill switch. The honest acceptance test for R2: **ignore/dismiss a hint and prove it never comes back** (while a still-unmastered concept resurfaces on its spaced window).

## T16 carry-over
Perception, the edit-log, model-directed context, and the arc-response all carry over intact — only the *trigger* moves from "on accept" to "on gate-pass." The deferred T16d BKT struggle signal becomes more valuable here (mastery-silence is now a load-bearing suppressor) but stays deferred until calibrated. The bookend's `struggled_concepts` source event (`prompt_offered`) must be renamed/kept as the direct-hint path logs its equivalent.

## Needs founder input (non-blocking; strawmen chosen)
The private-SPEC C/I numbers to map; final windows for the decaying suppression + spaced-resurface (strawmen above, tune by dogfood); whether the severity floor is kept as one fixed list or dropped (recommend: dropped — min_gap + memory gate).

## Dogfood findings 2026-07-09 (post-ladder, pre-merge — follow-up queue)
1. **Response-key vocabulary is not self-explanatory** (founder had to ask what
   `a/g/y/u/n` mean). The keys' *memory semantics* — applied=mastery credit,
   got-it=known/not-evidence, 👍=more-like-this, u=concept-wide 14d quiet,
   n=session snooze — are the product's core contract with the user and are
   currently invisible. Fix direction: `?` help explains what each response
   DOES (not just its name), and/or the first-run/welcome surface teaches the
   vocabulary once. Small, high-value, post-merge rung.
2. **Thinking-model tolerance is a BYOK-neutrality gap**: a reasoning model
   (local qwen3.5) blew the fixed 60s transport cap on judge-sized prompts and
   exhausted `max_tokens: 1024` on reasoning tokens, returning empty content
   (empty-`content` → parse_error drops). Fix direction: config-grade
   transport timeout + response token budget, and possibly a "reasoning
   model" hint per model entry. (The empty `curl error:` detail that masked
   this is already fixed on this branch — `show-error`.)
3. **Running-session settings-persist can clobber a hand-edited config**: the
   session holds `[dial]` in memory and `persist_dial_settings` writes ALL
   dial values back on any settings change, silently overwriting a file edit
   made while the session runs (observed live: `min_gap` 8m → "off").
   Fix direction (pick at follow-up): re-read the file before persisting and
   merge, or only write the value the user actually cycled.
