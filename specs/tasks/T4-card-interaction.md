# T4 — Card interaction

**Status:** Planned (after T3)
**Traces to:** SPEC v0.5 — D5/D17/D18/D19/D20, I15–I21, C3/C4/C6/C7/C12.
**User outcome:** every card is a conversation the founder controls: step the
help up or down, unfold a worked fix, ask a follow-up question in place, ask
anything via a comment addressed to the mentor, and request a whole-diff
"how would you have done this better?" — all without a chat app appearing.

## Scope

IN: full C3 response enum, C4 rung ladder rendering + directness knob,
worked-diff unfold, card-anchored threads, `// murshid:` channel, `murshid
review` solicited digest, BYOK consent, `applied` detection (mechanical).
OUT: memory-driven entry rung (T5 — until then entry = R2 + knob offset),
generation moments/R0 entry (T5, needs mastery state), application
detections for BKT (T5).

## Requirements

### Responses & ladder (C3, C4, D5)
1. Full response enum wired: `applied`·`escalated`·`got_it`·`not_now`·
   `not_useful`·`expired`. `applied` is detected mechanically: within the
   session, a later quiescence diff at the card's site removes the flagged
   pattern (advice_fp's anchor no longer matches) without a new finding —
   status flips to applied; manual `a` key also available.
2. `[dial] directness = guide-me|balanced|tell-me` (default `balanced`).
   Entry rung until T5: R2 + knob offset (guide-me −1 → R1, tell-me +1 →
   R3), clamp [R1, R3] (R0 needs mastery state, T5).
3. Escalation keys on the focused card: `e` steps one rung up (R1→R2→R3);
   `t` ("just tell me") jumps straight to R3. Every step is an `escalated`
   response event recording from→to.
4. R3 = the worked diff unfolded as a commented worked example (I19):
   minimal diff hunk + one comment line per changed region explaining the
   why. Reveals are logged (rung_shown updated) — click-through gaming must
   be visible in the data.

### Threads (D20, C5, C6)
5. `k` (ask) on a focused card opens an inline prompt; free-form question
   goes to the judge model with the card's anchor + concept + canon entry +
   thread history as context — nothing else (anchor-scoped by
   construction). Answer renders under the card; ≤ 5 user turns per card
   (C12), then the card suggests parking it ("thread cap — anything more
   belongs in your own exploration").
6. Thread turns respect the card's current rung (a follow-up is not an
   automatic bottom-out; if the answer would require revealing the fix at
   R1, it hints and offers `e`).
7. Thread verbs act only on this card. Transcripts land in `threads` (C5;
   add the table in this task's migration) and as `thread_msg` events.
   Unresolved threads (no terminal response on the card) are listed in the
   bookend.
8. Threads are pull-priced: no budget interaction; each turn passes the
   sanitizer; BYOK consent per C6 (`consent.solicited_spend`, default
   `ask`: first thread turn per session confirms with a rough token
   estimate; `always` skips).

### murshid-comments (D17, C2)
9. Detect fresh comments matching the pack's address token (`// murshid:`
   from surface.toml) in the session diff at quiescence. Each is a DIRECT
   ask: skips the offer stage and the screen stage; judge answers the
   question about the enclosing item as a card at the next quiescence
   moment. Pull-priced (no push budget), EFP-exempt logging (C3),
   `comment_ask` event.
10. A direct ask on a concept clears that concept's suppressions (snooze
    tiers and offer-declines — asking trumps 'not now'). Answered comments
    never re-trigger: advice_fp = (comment_text_hash, site); the card notes
    the comment can be deleted. Unresolved murshid-comments appear in the
    bookend.
11. Slot contention (C7): a direct-ask answer owns the single card slot on
    arrival; a displaced pushed card returns to the queue head.

### Solicited review (D18, C6, C12)
12. `murshid review` (command or `r` in-pane): judges the full session diff
    in one batched pass (screen → judge over all changed hunks), relevance-
    ranked digest of top 3 cards + "N more queued". Architecture-category
    advice is ALLOWED here regardless of detent floor (this surface is its
    home). Also offered as one line at commit detection and in the bookend
    (never auto-runs). BYOK consent per C6 with token estimate (a full-diff
    judge pass is the expensive call).
13. Review cards carry the goal line in their judge context and are logged
    EFP-exempt. D18 parity test as an assertion: the review prompt MUST
    include goal text + below-mastery concept slugs (from T5 when it lands;
    a static placeholder list until) — a bare "critique this diff" prompt
    is a spec violation.

## Acceptance

- `cargo test` green. New tests: applied-detection via site re-check;
  knob→entry mapping + clamps; escalation event from→to chain; R3 render
  contains per-region comments; thread turn cap enforced; thread context
  assembly is anchor-scoped only (asserted: no other files in payload);
  rung-respecting thread answers (fixture); comment-token detection incl.
  non-`//` token from a synthetic surface.toml; suppression-clear on direct
  ask; answered-comment non-retrigger; slot contention (displaced card to
  queue head); review digest ranking + top-3 cap; review prompt contains
  goal text (parity assertion); consent gating for thread/review spends.
- Manual dogfood: full loop — card appears, `e` up, `k` one question, fix
  applied, card flips to applied; a `// murshid: why?` comment gets
  answered; `murshid review` on a messy diff returns 3 ranked cards.
- No TODO markers in covered files.
