# TUI feasibility & design proposal for murshid

**Status:** EXPLORATION — proposal for founder ratification. Nothing here is
decided. Decisions that are the founder's to make are flagged **[FOUNDER
DECISION]**; my opinions are flagged **[RECOMMENDATION]**.
**Author:** design agent (research + design only; no code written)
**Date:** 2026-07-05
**Inspiration named by founder:** gitui ("levels", not memorizing shortcuts,
multiple views on the data).
**Scope note:** this is a design document under `specs/explorations/`, not a
task spec. It does not enter the T-ladder or `INDEX.md` unless the founder
promotes it.

---

## 1. Executive summary & recommendation

murshid today is a single-pane, single-key ambient watcher: it reacts to file
saves, shows **at most one card at a time** (the deliberate CD-1/D10 "quiet,
over-your-shoulder" constraint), and takes single-key responses
(`a`/`g`/`u`/`n`/`e`/`t`/`k`). Everything else the tool knows — the mastery
meter, the pull queue, threads, the event history, goal/drift, throttle state
— lives behind separate one-shot commands (`progress`, `review`, `goal`) or is
invisible entirely. The founder, inspired by gitui, is asking whether a
terminal UI with discoverable "levels" and multiple views on the same data
would be a better surface.

**Is it feasible? Yes, technically.** The engine is already decomposed into a
library crate (`lib.rs`) with clean domain modules; all state is either in
SQLite (`profile.db`) or in the in-memory `WatchSession`. A read-mostly TUI
over that data is a modest, well-bounded build. The hard parts are **not**
engineering — they are two ratification questions (§4, §5).

**Is it worth it? Conditionally, and not yet.** A TUI would genuinely fix real
friction (memorized shortcuts, no way to see the queue/meter/history without
leaving the pane, and the "silence is ambiguous" problem the founder just hit
— §2). But murshid's core loop (does a proactive card actually teach?) is
**still unvalidated** — the dogfood gate has barely started collecting EFP
data. A full-screen TUI is a large surface to build and maintain against a loop
that might still change shape. Building it now risks polishing a frame around a
painting that isn't finished.

**Smallest valuable version [RECOMMENDATION]:** a **separate, read-mostly
`murshid tui` dashboard** — a window onto `profile.db` and (optionally) the
live session — that leaves `watch` exactly as it is. This is the option that
(a) delivers the discoverability and multi-view value the founder wants, (b)
directly serves the observability need (§2), (c) does **not** disturb the
ambient/noise-budget ethos the spec spent all of CD-1/CD-2 protecting, and
(d) is the cheapest to build and safest to abandon if the core loop changes.

**Go / no-go / conditional [RECOMMENDATION]: CONDITIONAL GO on a spike, NO-GO
on a commitment.** Specifically:

- **Now:** ratify the two philosophy/dependency decisions (§4, §5). If the
  founder wants to move, greenlight a **time-boxed spike** (§8, Phase 0) of a
  read-only dashboard behind a feature flag — enough to feel it, not enough to
  own it.
- **Not now:** do not replace the `watch` pane, do not build write-paths
  (responding to cards from the TUI), and do not treat the TUI as a T-ladder
  commitment until the dogfood gate says the core loop is real.

The rest of this document is the evidence and the trade-offs behind that
recommendation.

---

## 2. Current UX inventory & friction

### What `watch` shows today

`watch::run` (`src/watch/mod.rs`) owns the terminal pane and spawns four
concurrent workers over a shared `Arc<WatchSession>`:

- **stdin reader** (`watch/keys.rs`) — the single-key response handler.
- **offer-poll** (`watch/offers.rs`) — the struggle-offer timer (D15).
- **file-event → quiescence worker** (`watch/sweep.rs`) — the judge path.
- **keep-alive / SIGINT shutdown poller** (`watch/mod.rs`).

The visible surface is a linear scroll of `println!` lines:

- Session-start banners: frequency detent, directness, inferred goal, retention
  prune notice, retrieval questions.
- One card at a time (`card::render_card_at_rung`), 80-col markdown-ish text.
- A one-line presence indicator for the queue ("N more thoughts — `m`").
- Struggle offers ("stuck on E0308 for 12 min — want a hint? [y/N]").
- Throttle notices, mastery back-off notices, the session-end bookend.

### The single-key model

Inside the pane, cards take single-key responses (from `README.md` and
`response.rs` / `offer.rs` / `keys.rs`):

`a` applied · `g` got it · `u` not useful · `n` not now (snooze) · `e` escalate
· `t` tell me · `k` ask a follow-up (opens a thread) · `m` browse queue · a
number pulls a queued item · `r` in-pane review · `g` (no pending card) edits
the goal · `y`/anything resolves a pending offer.

This is faithful to CD-3's terminal-presentation contract (I21: footer key
hints + `?`, progressive disclosure). But it is a **flat, memorized keymap with
no always-visible legend** — exactly the thing the founder cited gitui as
solving.

### The separate one-shot commands

- `murshid goal [text]` — print/set the session goal.
- `murshid review [path]` — solicited whole-diff review digest (D18).
- `murshid progress` — the open per-concept mastery meter (I24), read straight
  from `concept_memory`.

Each opens its own DB connection, loads a pack, prints, exits. None of them are
reachable *from within* `watch` except `r` (review) and the queue browse — so
to see your mastery meter or history you leave the pane.

### The concrete friction

1. **Memorized shortcuts, no discoverability.** Nine-plus context-dependent
   keys with no persistent legend. gitui's answer — a command bar that always
   shows the keys valid *right now* — is the founder's stated draw.
2. **No persistent multi-pane view.** The queue, the meter, threads, event
   history, goal/drift, and throttle state are all things murshid *knows* but
   can only surface one-at-a-time, transiently, in a scrolling log. There is no
   way to *look at* the state of the session; you can only wait for it to
   scroll past.
3. **Silence is ambiguous — the observability gap the founder just hit.** When
   you save a file and nothing appears, that single silence collapses several
   very different outcomes:
   - the **screen** model found no candidate teaching moment (nominal);
   - a candidate was **dropped** at the D9 gate (a leg failed — nominal but
     invisible);
   - the judge **succeeded** but the card was **queued** (budget spent) or
     **suppressed/throttled/silenced-by-rung** (nominal but invisible);
   - the provider **failed** / degraded mode (a *problem*, currently only a
     status line that may already have scrolled away);
   - nothing was dispatched at all because quiescence/parse gating held.

   Today these look identical: the pane just sits there. There is no live view
   of "what did the judge just do, and why did (or didn't) a card result." This
   is the same distinction the code already draws internally — `JudgeMode`
   (`judge.rs`), the `card_shown` / `prompt_offered` / `throttle_change` /
   `check_result` event kinds (C5), the degraded-mode reason string — but never
   surfaces as a *live, inspectable* view.

   **Synergy with the in-flight observability work (T14).** The founder's
   observability effort (surfacing live judge outcomes: declined vs failed vs
   succeeded-but-queued) and a TUI want *the same thing* — a live, structured
   view of the judgment pipeline. A TUI is the natural home for T14's output: a
   "session activity / judge trace" panel that turns the ambiguous silence into
   a legible feed. **[RECOMMENDATION]** treat the observability data model as
   the shared dependency and design it once, whether or not the TUI ships —
   T14 is valuable stand-alone (even as structured log lines), and the TUI
   consumes it rather than reinventing it.

---

## 3. What gitui does well, mapped to murshid

gitui (Rust: **ratatui** — the maintained fork of the archived tui-rs — over
**crossterm** for the terminal backend; a central **event loop** draining
input + async git results; a **stack of modal components** each implementing a
draw/handle-event contract; an always-visible **command bar** of contextually
valid keybindings; keyboard-first list/scroll navigation; a syntax-highlighted
**diff panel**; background git work on a thread feeding results back over a
channel). Trait-by-trait mapping to murshid's data:

| gitui trait | How it maps onto murshid |
|---|---|
| **"Levels" / modal view stack** (status → log → diff → stash, each a full view you push into and pop out of) | murshid has natural "levels": session dashboard → card detail → thread → concept-history. Also lateral views: mastery meter, event history, queue, goal/drift. A view stack replaces "run a different command" with "press a key to descend." Directly answers the founder's ask. |
| **Always-visible contextual command bar** | Replaces murshid's memorized keymap. Show exactly the verbs valid on the focused object: on a card → `a/g/u/n/e/t/k`; in the queue → pull/back; in the meter → nothing (read-only). This is the single highest-value, lowest-risk borrow. |
| **Keyboard-first list navigation** | The **queue** (`queue::render_queue_list`) and the **mastery meter** (`progress::render_progress`) are already lists rendered as text. Making them navigable lists (arrow/`j`/`k`, enter to inspect) is a near-direct lift. |
| **Diff panel (syntect-highlighted)** | murshid's **session diff** (`session::compute_session_diff`) and a card's **worked diff** (the R3 bottom-out, currently folded text) map straight onto a diff pane. Showing the grounding quote + worked rewrite side-by-side beats the folded-text render. Note: gitui uses syntect for highlighting — a dependency question (§4); murshid could ship plain/ANSI diff first. |
| **Async work → event channel** (git ops never block the UI thread) | murshid already does this: the file-event → quiescence → judge path runs on its own worker and the offer-poll on a timer. A TUI's redraw loop would consume the *same* state (`WatchSession` mutexes + `profile.db`) that those workers produce. The concurrency model is already the shape gitui uses. |
| **Status view = "state of the repo at a glance"** | murshid's analog is "state of the *session* at a glance": active card + queue depth + budget/throttle + goal + drift — none of which has a home today. This is the flagship dashboard view (§7). |
| **Log view = scrollable history** | murshid's `events` table (C5, append-only) is a ready-made log: `card_shown`, `card_response`, `prompt_offered`, `throttle_change`, `check_result`, `encounter`, … A browsable event history is the observability view (§2, §7) and is almost pure-read. |
| **`?` help overlay** | Already mandated by I21; a TUI makes it a real overlay instead of a printed block. |

**Where the analogy breaks (important):** gitui is a **foreground, you-drive-it
tool** — you open it *to do git*. murshid's `watch` is an **ambient,
it-watches-you tool** — it runs beside your editor and speaks rarely, by
design. gitui's whole-screen, attention-demanding modality is exactly what CD-1
spent its entire evidence base (Greptile's 79% nits, Tricorder's EFP discipline,
the Clippy calibration failure) arguing *against* for the proactive channel.
This tension is §5, and it is the crux.

---

## 4. Central question #1 — the dependency rule

### The constraint

`CLAUDE.md`: "Standard library only unless a spec explicitly allows a
dependency." SPEC **C10** makes this normative and names the exact allowlist:

> `rusqlite` (bundled), `serde`/`serde_json`, `keyring`, `notify`, `libc`,
> `tree-sitter` + per-pack grammar crates, `ureq` (BYOK HTTP). **Adding any
> other dependency requires a spec amendment naming it.**

`Cargo.toml` matches this exactly, plus one **dev-dependency**, `proptest`,
which the manifest explicitly annotates as "the single approved non-stdlib
addition to the tree" (test-only). So there is real precedent for the process:
**every non-stdlib crate in this repo was individually justified**, and one
(`proptest`) was added specifically as a scoped, documented exception.

A real TUI effectively requires **ratatui + crossterm** (or an equivalent like
termion, or the heavier cursive). These are not in the allowlist. A syntax-
highlighted diff (gitui-style) would additionally want **syntect** + a theme —
another crate, and a heavy one.

### The precedent, examined honestly

The tree-sitter precedent is instructive. tree-sitter (`C9`, payload 6) was
admitted because the spec's *pedagogy* genuinely needs it: the `Site` identity
(C2), the parse-gate in the quiescence moment (D8/C12), and concept anchoring
all depend on real parsing. It earned its place by being load-bearing for a
*ratified design decision*. `ureq` similarly is load-bearing for BYOK. `notify`
for the watcher. Each dep maps to a spec requirement it is the only reasonable
way to satisfy.

ratatui + crossterm would **not** clear that same bar today, because there is
no ratified requirement that says "murshid has a TUI." The dependency would be
justified by a *new product decision*, not an existing one. That is precisely
why this is a founder decision, not an implementation one.

### The options

**(a) Amend the spec to permit ratatui + crossterm.**
Precedent exists (the process is well-worn; C10 anticipates exactly this
amendment path; proptest shows scoped exceptions are acceptable). ratatui +
crossterm are mature, widely-used, pure-Rust, permissively licensed, and
maintained (ratatui is the living successor to tui-rs; gitui, k9s-likes, and
much of the Rust TUI ecosystem run on them). Trade-offs:
- **For:** the only sane way to build a real TUI; small, focused crates; no
  transitive surprise the size of an async runtime; crossterm is already the de
  facto terminal backend the spec's own CD-3 prior-art survey (lazygit/k9s/
  gh-dash/Charm) implicitly assumes.
- **Against:** it grows the dependency surface for a *frontend*, when C10's
  spirit is "the pedagogy engine stays lean." It also sets precedent — the next
  "nice frontend crate" (syntect, a markdown renderer, a color crate) will cite
  it. Contain that by amending for *exactly* ratatui + crossterm and nothing
  else, with syntect/highlighting explicitly deferred.
- **Maintenance:** low-to-moderate. ratatui has occasional breaking releases;
  pinning is cheap. The bigger maintenance cost is the UI *code*, not the crate.

**(b) Hand-roll a stdlib-only TUI over raw ANSI + termios (via `libc`, already
allowed).**
Technically possible — `libc` is in the allowlist, so raw-mode termios and ANSI
escape sequences need no new crate. **[RECOMMENDATION: reject this.]** Be
skeptical here. What "a TUI" actually requires, that you would be
re-implementing by hand:
- raw-mode setup/teardown that survives panics and SIGINT (murshid already has
  a delicate async-signal-safe SIGINT path in `watch/mod.rs` — hand-rolled raw
  mode fighting that is a real hazard);
- an input parser for escape sequences (arrows, resize, paste, mouse) across
  terminals that disagree;
- a double-buffered diff-renderer to avoid flicker (the thing ratatui *is*);
- layout math, wrapping, scroll regions, Unicode width handling (murshid
  already hand-computes 80-col wrap; extending that to a full layout engine is a
  project);
- light/dark + NO_COLOR + screen-reader degradation (I21) done correctly.
This is re-implementing ratatui, badly, forever. The maintenance burden lands
on a **solo maintainer** (Decision 7's bar) and competes with the pedagogy work
that is the actual product. The stdlib rule exists to keep the *engine* lean
and portable; honoring it by hand-building a terminal UI framework inverts its
intent. Cost estimate: weeks to a rough version, indefinite tail of
terminal-compatibility bugs.

**(c) Don't build a TUI.**
Keep the ambient pane; address the *specific* friction with smaller moves
inside the existing constraints: a proper `?` help block (I21 already wants
it), a richer presence line, and — most importantly — ship the **observability
data** (T14) as structured status lines so "silence is ambiguous" is fixed
*without* a new frontend. This is the true zero-cost baseline and the honest
comparison point for whether a TUI earns its keep.

### Recommendation on the dependency rule

**[RECOMMENDATION]** If the founder decides a TUI is worth building at all
(§5, §8), then **option (a) — a narrow spec amendment naming exactly `ratatui`
and `crossterm`** — is the right path, with:
- syntect / syntax highlighting **explicitly excluded** from the amendment
  (start with plain or ANSI-colored diffs; revisit only if it earns it);
- the amendment scoped as "frontend-only; the pedagogy engine and all
  `lib.rs` domain modules remain stdlib+C10-allowlist"; the TUI lives in its
  own module/binary and the engine never depends on ratatui types.

Reject (b) outright. Treat (c) as the fallback that must be beaten — and note
that (c)'s core deliverable (observability/T14) is worth doing *regardless*, so
do it first (§8, Phase 0) and let it inform whether (a) is still wanted.

---

## 5. Central question #2 — ambient vs foreground

This is as much product philosophy as engineering, and it is the decision that
most affects whether a TUI *helps* or *betrays* murshid.

### The clash

The spec's identity is a **quiet, ambient, over-your-shoulder** watcher:

- I7 "Ship chill" — defaults at the quiet end; first run deliberately
  under-communicative.
- D10 — at most ONE pushed card on screen; everything else queues behind a
  one-line indicator; the noise *budget* is a token bucket.
- I10 — offers are non-modal, steal no focus, expire silently; continuing to
  type is a valid decline.
- The entire CD-1 parity gate — worst case, murshid degrades to "silent until
  asked" and is *never inferior* to a zero-noise prompt persona.

A TUI, in the gitui sense, is the **opposite**: full-screen, foreground,
attention-demanding, you-look-at-it. Dropping murshid's ambient watcher into a
full-screen app that demands the foreground would **directly violate I7 and
I10** and re-introduce exactly the attention-cost the noise budget exists to
ration. This is the single biggest risk in the whole proposal and must be
confronted head-on.

### The options

**(a) TUI as a SEPARATE `murshid tui` dashboard; `watch` stays as-is.**
A read-mostly, on-demand window onto `profile.db` + (optionally) the live
session. You open it when *you* want to look — at the meter, the queue, the
event history, the goal — and close it. `watch` remains the ambient channel and
keeps owning the proactive, budgeted, one-card interaction exactly as specified.
- **Preserves the ethos fully.** The ambient channel is untouched; the TUI is a
  *pull* surface (you open it), which is squarely inside I5's "unlimited pull"
  and the "look at the state when you want" model. It demands the foreground
  only when the user chose to open it — that is not a Clippy violation, it is a
  dashboard.
- **Serves observability best:** the event-history + judge-trace view (§2, §7)
  lives here naturally without adding a single interruption to `watch`.
- **Cheapest, safest, most reversible.** No rewrite of the delicate watch
  concurrency/SIGINT code; if the core loop changes, the dashboard just reads
  whatever the new schema holds.
- **Cost:** it is a *second* place state is shown; some duplication of render
  logic (mitigated — see §6, the render functions are already pure).

**(b) TUI REPLACES the `watch` pane entirely.**
`watch` becomes a full-screen ratatui app: a live dashboard with the active
card, the queue, the meter, all on screen at once.
- **For:** one surface, everything visible, maximal discoverability; the
  "levels" idea in full.
- **Against — serious:** it converts the *ambient* watcher into a *foreground*
  app, contradicting I7/I10 and the CD-1 attention economy. It forces a
  full-screen takeover of a terminal the user wanted *beside* their editor, not
  *instead* of it. It also means rebuilding the entire delicate
  stdin/offer/sweep/SIGINT concurrency inside an event loop — the highest-risk
  engineering, over the least-validated part of the product. **[RECOMMENDATION:
  reject for v1.]** This is the version that looks most like gitui and is the
  most tempting, and it is the wrong one to build first.

**(c) Hybrid.**
`watch` stays ambient by default; a keystroke (say `d` for dashboard) *within*
`watch` opens the full-screen TUI as an overlay/alternate-screen, and closing
it returns to the ambient pane. Or: the TUI runs as a thin always-on status
strip while cards still render inline.
- **For:** best of both — ambient by default, rich view on demand, one process.
- **Against:** the most engineering (you now maintain *both* an ambient render
  path and a TUI event loop in one binary, plus the transition between them and
  the terminal-mode switching around the existing SIGINT/raw-mode hazards). Good
  as a *destination*, risky as a *starting point*.

### Recommendation on ambient vs foreground

**[RECOMMENDATION] Option (a) for the first build, with (c) as the eventual
destination if (a) proves its worth.** A separate `murshid tui` dashboard
preserves the quiet/noise-budget ethos completely (the ambient channel is
sacred and untouched), delivers the founder's discoverability + multi-view +
observability goals, and is the cheapest and most reversible. Only after the
dashboard is loved *and* the core loop is validated should the hybrid overlay
(c) be considered. **Never (b)** — replacing the ambient pane with a foreground
app is the one move that fights the spec's spine.

**[FOUNDER DECISION]** This is fundamentally a product-identity call: is
murshid allowed a foreground surface at all, or is "ambient only" part of its
soul? The spec's non-goals ("not a chatbot") and I7/I10 lean ambient; a
read-only dashboard is compatible with that reading, a full-screen replacement
is not. The founder owns this line.

---

## 6. Architecture sketch

Enough to judge feasibility — not a full design.

### Where the TUI layer sits

The engine is already in the right shape for this. `lib.rs` exposes every
domain module publicly; `main.rs` is a thin binary. A TUI would be **a new
frontend module** (e.g. `src/tui/`) and a new command arm (`murshid tui`),
sitting *over* the same `lib.rs` surface the CLI uses — peer to `cli/` and
`watch/`, never depended on *by* the engine. The engine stays frontend-agnostic
(Decision 4 already promises this: "engine stays frontend-agnostic behind a
protocol seam").

Critically, the **render logic is already pure and reusable**:
`progress::render_progress`, `queue::render_queue_list`,
`card::render_card_at_rung`, `bookend::render_bookend`,
`review::render_review_digest` are pure functions from data → String. A TUI can
either reuse them verbatim (render into a scrollable text widget — the cheapest
path) or read the same underlying structs (`progress::build_rows`,
`QueueEntry`, `Card`) and lay them out as native widgets. This dramatically
lowers the cost of option §5(a): the dashboard is mostly "call the existing
build_* functions and draw the result."

### What it reads

All of it already exists:
- **`profile.db`** via `db::*`: `list_concept_memory` (meter),
  `get_events_for_session` / event history (observability), `cards` (queue +
  card state), `threads` (`get_thread_messages`), suppressions, throttle
  history (`recent_card_statuses_for_category`, `latest_throttle_action`).
- **Pack data** via `pack::*` (taxonomy for slug→name, canon) — same loads
  `progress`/`review` already do.
- **Live session state** (only if attaching to a running `watch`): the
  `WatchSession` mutexes (pending card, queue, budget bucket, throttled set,
  goal cluster, drift, struggle tracking).

### How it receives live updates from a running `watch` session

This is the one real integration seam, and it has a spectrum:

1. **Poll `profile.db` (read-only connection) on a timer.** [RECOMMENDATION for
   MVP.] The event log is append-only (C5) and SQLite is WAL-mode
   (`open_connection` sets `journal_mode=WAL`), so a second read-only connection
   can tail `events`/`cards`/`concept_memory` while `watch` writes, with no lock
   contention. The dashboard redraws from the DB every N ms or on a keypress.
   - **For:** zero coupling to the running process; works even with no live
     `watch` at all (pure historical dashboard); trivially safe; reuses the
     entire existing persistence layer.
   - **Against:** in-memory-only state (the live pull *queue* and the *budget
     bucket* live in `WatchSession`, and per C2 the queue "dies at session end"
     — it is not in the DB) is invisible to a pure-DB reader. The meter, event
     history, cards, threads, goal, throttle, and struggle *events* are all in
     the DB and fully visible; the ephemeral queue/bucket are not.
   - **Mitigation:** either accept that the live-queue view requires attaching
     to the process (below), or have `watch` periodically checkpoint a
     lightweight session-state snapshot to a known file/table the TUI can read
     (small, additive, spec-neutral).

2. **IPC / channel between `watch` and the TUI** (unix socket, or a shared
   in-process channel if they are the same binary in the §5(c) hybrid).
   - **For:** exact live view of `WatchSession` including the ephemeral queue
     and bucket; push updates, no polling latency.
   - **Against:** real new surface — a protocol, a socket lifecycle, error
     handling when `watch` dies mid-view. Overkill for a read-mostly MVP; this
     is where the "protocol seam" of Decision 4 would eventually live, but it is
     not MVP work.

3. **File-watch the DB** (reuse `notify`, already a dependency) to redraw on
   change instead of polling — a refinement of (1).

### Integration seams & risks (real ones)

- **Terminal ownership / raw mode vs the existing SIGINT path.** `watch` has a
  carefully async-signal-safe SIGINT handler and a keep-alive poller
  (`watch/mod.rs`). A separate `murshid tui` process sidesteps this entirely
  (it owns its own terminal); a hybrid/replacement (§5 b/c) collides with it and
  must be designed carefully (alternate screen enter/leave, panic hooks that
  restore the terminal, SIGWINCH). This risk is *avoided* by §5(a) and *incurred*
  by §5(b)/(c) — another reason to start with (a).
- **Two render surfaces to keep consistent.** Mitigated by reusing the pure
  `render_*` functions.
- **DB read amplification.** A polling redraw must not hammer SQLite; debounce
  and cache. Low risk given WAL + the retry/backoff layer already in `db`.
- **The ephemeral-state gap** (queue/bucket not in DB), per above — a design
  choice to make explicitly, not discover late.

---

## 7. Proposed views

Concrete panel inventory. For each: what it shows, key interactions, why it
beats today. All are **read-mostly** in the recommended MVP (write-paths — e.g.
responding to a card from the TUI — are a deliberate later phase, §8).

### V1 — Session dashboard (flagship)
- **Shows:** active card (or "no card") · queue depth + top few queued concepts
  · budget/throttle state (which categories are queue-only right now) · current
  goal + drift status · degraded/live judge mode · a compact recent-activity
  strip.
- **Interactions:** navigate to any sub-view; `?` help overlay.
- **Beats today:** this state has *no home* today — it only scrolls past. It is
  the "state of the session at a glance" gitui's status view provides, and the
  first thing the founder would open.

### V2 — Mastery / progress view (the open meter, I24)
- **Shows:** `progress::build_rows` output as a navigable list — per-concept
  p_mastery, help level/rung, staleness, throttle flag.
- **Interactions:** sort/filter (by mastery, by staleness, by category); enter
  a concept → its history (encounters, outcomes, the threads attached to it).
- **Beats today:** `murshid progress` is a static dump you run in another
  terminal; here it is live, navigable, and drillable into the evidence behind
  each number — which is exactly I24's "the meter is the trust mechanism for
  adaptivity."

### V3 — Card + thread interaction view
- **Shows:** the six-field card (D19) with the worked diff in a proper diff
  pane (grounding quote vs worked rewrite), the ladder rung + escalation state,
  and the anchored thread transcript (`threads`).
- **Interactions (read-mostly MVP):** scroll, expand/fold the worked diff, read
  the thread. (Write-paths — respond/escalate/ask from here — are Phase 3, and
  only if §5 moves toward hybrid.)
- **Beats today:** the worked diff is folded text and threads scroll away; a
  dedicated view makes the bottom-out worked example (I19) a real side-by-side.

### V4 — Event / history browser (serves observability directly)
- **Shows:** the `events` table (C5) as a scrollable, filterable log —
  `card_shown`, `card_response`, `prompt_offered`, `prompt_response`,
  `throttle_change`, `check_result`, `encounter`, `goal_inferred`, … with the
  judge-outcome distinction from T14 (declined / dropped-at-gate / succeeded-
  but-queued / provider-failed) rendered explicitly.
- **Interactions:** filter by kind/session/concept; jump to the card/concept an
  event refers to.
- **Beats today:** this is the *direct fix* for "silence is ambiguous" (§2).
  Every judgment leaves a trace; today none of it is inspectable live. This view
  and T14 are the same feature wearing two hats.

### V5 — Goal / drift view
- **Shows:** current goal, how it was inferred (branch/commits/cluster), drift
  touches, and the session bookend (D14) as a live-updating panel rather than a
  one-shot exit screen.
- **Interactions:** edit goal (opens `$EDITOR` exactly as `keys.rs` does today
  — a safe, existing write-path); see goal-relevance of queued cards.
- **Beats today:** goal state is a startup banner you can't revisit; the
  bookend is exit-only. A live goal panel makes the D14 relevance-lens legible.

**Priority [RECOMMENDATION]:** V1 + V4 first (dashboard + history/observability
— the highest value and most-read, least-write), then V2 (meter), then V3/V5.

---

## 8. Phased path + MVP

Framed as spike-vs-commitment, smallest-valuable-first.

### Phase 0 — Observability data (do this regardless) — SPIKE, no new deps
Ship T14's structured judge-outcome data (declined / dropped-at-gate /
succeeded-but-queued / provider-failed) as **structured status lines + event
rows** in the existing `watch` pane and `events` table. **No TUI, no new
crate.** This directly fixes "silence is ambiguous," is valuable stand-alone,
and produces the exact data model the TUI's V4 later consumes. It is also the
honest test of §4 option (c): if this alone relieves enough pain, the TUI is
less urgent.

### Phase 1 — Read-only dashboard spike — TIME-BOXED SPIKE
Behind a feature flag / unstable `murshid tui` command. Requires the §4(a)
amendment (ratatui + crossterm only). Build **V1 (dashboard) + V4 (history)**
as a *separate, read-only* process polling `profile.db` (§6 option 1). Reuse
the pure `render_*` functions. Success criterion: does the founder actually
open it during a dogfood session, and does it make the session legible? This is
a spike — explicitly disposable, not a T-ladder commitment.

### Phase 2 — Meter + navigation — COMMITMENT (only if Phase 1 earns it)
Add **V2 (mastery)** with drill-down, sorting/filtering, and the `?` command
bar with contextual keybindings (the founder's core "don't memorize shortcuts"
ask). Promote from spike to a real task spec in the T-ladder. Add live-update
via `notify` file-watch (§6 option 3) instead of naive polling.

### Phase 3 — Card/thread view + (maybe) write-paths — LATER
Add **V3 (card+thread)** and **V5 (goal/drift)**. Only here consider whether the
TUI should gain *write* interactions (respond to cards), which is also where the
§5 hybrid (c) question reopens. Gate this on: (a) core loop validated by
dogfood, and (b) the read-only dashboard proven valuable.

**What is a spike vs a commitment:** Phase 0 and Phase 1 are spikes (cheap,
reversible, feature-flagged). Phase 2+ are commitments that should only begin
after the dogfood gate says the core loop is real and the spike says the TUI is
loved. Do not commit to Phase 2 before both signals exist.

---

## 9. Risks & founder decisions required

### Decisions requiring founder ratification [FOUNDER DECISION]
1. **Dependency-rule amendment (§4).** Amend C10 to permit `ratatui` +
   `crossterm` (frontend-only, syntect excluded)? This is a spec amendment, not
   an implementation call — it is the one hard gate. Without it, only §4(b)
   (hand-rolled, recommended-against) or §4(c) (no TUI) are legal.
2. **Ambient vs foreground identity (§5).** Is murshid allowed a foreground
   surface at all? Ratify: separate read-only dashboard (a) — **recommended**;
   full replacement of the watch pane (b) — **recommended against**; hybrid (c)
   — as an eventual destination, not a start. This is a product-identity call
   touching I7/I10 and the non-goals.
3. **Scope vs dogfood priority (§8).** Does a TUI spike belong ahead of, or
   behind, the current dogfood-driven priorities? The core loop is unvalidated;
   the founder owns whether frontend polish preempts pedagogy validation. My
   read: do Phase 0 (observability) now, hold the TUI spike until the dogfood
   gate produces EFP signal.
4. **Live-state coupling (§6).** Accept the DB-poll MVP's blind spot (ephemeral
   queue/bucket not visible without process attach), or fund the small
   session-snapshot checkpoint, or defer live-queue viewing entirely?

### Top risks
- **Scope creep mid-dogfood.** The biggest risk: building a large, attractive
  frontend *now* diverts effort from validating the core teaching loop, and a
  full TUI (§5 b/c) is a genuinely large, open-ended surface. Mitigation: spike
  small, read-only, feature-flagged; Phase 0 first.
- **Maintenance burden on a solo maintainer.** Decision 7's bar is
  solo-maintainable side-income. A TUI event loop + terminal-compat tail is
  ongoing cost. Mitigation: reuse pure render functions; narrow dep footprint;
  reject hand-rolled (§4b).
- **The core loop is still unvalidated.** A TUI frames a picture that may still
  be repainted. Building write-paths / a replacement pane over an unproven loop
  risks throwing work away. Mitigation: read-only first; commit only after the
  dogfood gate.
- **Ethos regression.** A foreground app quietly eroding the ambient/noise-
  budget discipline the spec spent CD-1/CD-2 protecting. Mitigation: §5(a) keeps
  the ambient channel untouched; never ship (b).
- **Dependency-precedent creep.** The amendment becoming a doorway for
  syntect/markdown/color crates. Mitigation: amend for exactly two named
  crates; require a fresh amendment for any further frontend dep.

---

## Appendix: key source references

- Ambient watch loop & concurrency: `src/watch/mod.rs` (`WatchSession`, the
  four workers, SIGINT path).
- Single-key model: `src/watch/keys.rs`, `src/response.rs`, `src/offer.rs`.
- Struggle offers: `src/watch/offers.rs`.
- Pure render functions (reuse targets): `src/progress.rs`
  (`render_progress`/`build_rows`), `src/queue.rs` (`render_queue_list`),
  `src/card.rs` (`render_card_at_rung`), `src/bookend.rs`, `src/review.rs`.
- One-shot commands: `src/cli/progress.rs`, `src/cli/review.rs`,
  `src/cli/goal.rs`.
- Data available to a TUI: `src/db/` (`events`, `cards`, `concept_memory`,
  `threads`, `suppressions`; WAL mode in `open_connection`).
- Dependency rule: `CLAUDE.md`; SPEC **C10** (allowlist + amendment path);
  `Cargo.toml` (the proptest dev-dep precedent).
- Ethos/invariants this proposal protects: SPEC **I5, I7, I10, I21**; **D10**
  (one card, budget); **C2** (queue dies at session end — the ephemeral-state
  gap); **CD-1/CD-2 parity gates**.
- Observability tie-in: `src/judge.rs` (`JudgeMode`, degraded-mode reason);
  C5 event kinds; the in-flight **T14** work.
