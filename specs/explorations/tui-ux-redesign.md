# murshid TUI — UX redesign proposal ("Focus")

Status: SHIPPED as T15 (merged to main 2026-07-06, `c45707a`); superseded-in-spirit
by [`tui-redesign-2-workspace.md`](tui-redesign-2-workspace.md). Originally an
exploration / design proposal (no code) — see [T15-tui-spike.md](../tasks/T15-tui-spike.md)
for the implementation record. Target stack: ratatui 0.29 +
crossterm 0.28 only (the crate's existing deps). Author POV: senior TUI/UX
designer, writing for the founder — a Rust engineer and, for now, the sole
user, who has said plainly "I don't like the UI."

This proposes **one** cohesive direction. It is deliberately opinionated: the
founder will react to it *built*, so this commits to a single vision rather
than hedging across three half-designs.

---

## 0. The problem, stated plainly

The current TUI is four co-equal tabs, each of which dumps a pure engine
string into a scrolling `Paragraph`. The only styling is reverse-video on the
active tab. It reads like a debug dump of `WatchSession`, because that is
literally what it is — `draw_dashboard` builds a `String` by `push_str`-ing
every field of the live state in sequence (busy, card, parse-wait, offer,
queue, budget, throttle, goal, drift, judge, activity) and wraps the whole
thing in one bordered box.

That layout makes three mistakes:

1. **The one thing that matters — the teaching card — has no more visual
   weight than "throttled categories: none."** Everything is the same gray
   text at the same indentation. There is no hero.
2. **Four equal tabs imply four equal jobs.** But murshid does *one* job at a
   time (D10: one card on screen), and the other three "tabs" are things you
   consult occasionally (mastery) or almost never (the raw events table).
   Co-equal tabs make the rare things feel as important as the live moment.
3. **It's a dashboard that screams**, which is exactly what a calm mentor
   (I7 "ship chill") must not be. Density is the enemy here, not the goal.

The redesign fixes all three by inverting the model: **not a dashboard of
panels, but a single quiet reading surface with a card at its center**, framed
by a whisper-thin band of ambient context, with the rare views (mastery,
history) summoned as full-screen overlays rather than living as permanent
competing tabs.

---

## 1. Design principles (the commitments)

1. **One surface, one moment.** The screen is a reading surface for the single
   card on screen, not a grid of live gauges — because the product's promise
   is "one expert thought at a time" (D10), not telemetry.
2. **The card is the hero; everything else is a frame around it.** Mastery,
   budget, goal, and history are context *for* the card, never peers of it —
   so they get a thin ambient band and summonable overlays, not equal tabs.
3. **Color is meaning, never decoration, and never the only signal.** Every
   color pairs with a glyph and a word so the UI is fully legible under
   `NO_COLOR`, in a 16-color terminal, and to a screen reader (I21).
4. **Calm by default; earn every pixel of attention.** Idle is a serene empty
   state, not an error or a blank — silence is a feature (I7), but it is
   always *labeled* silence (the "silence is ambiguous" dogfood lesson).
5. **The keybar is the manual.** Only currently-valid keys are ever shown, as
   labeled chips, so the founder's "don't make me memorize shortcuts" holds —
   you read what you can do, you never recall it (the lazygit lesson).
6. **Progressive disclosure maps onto the ladder.** "Levels" the founder wants
   = the card's own rungs (R0 question → R2 explanation → R3 worked example)
   plus drill-down from a mastery row into that concept's history. Depth is
   opt-in, never dumped.
7. **The mastery meter is a real meter.** It is the trust mechanism (I24), so
   it renders as actual bars with state color — not a `{:>3}%` column in a
   monospace table.
8. **Degrade in layers.** Design monochrome first (is it usable?), then add 16
   ANSI colors (is it readable?), then optional accent (is it lovable?) — each
   tier must stand on its own, per the layered-TUI discipline.

---

## 2. Information hierarchy (from the user's POV, ranked)

What the user actually needs, moment to moment, ranked by how much visual
weight it should get:

**PRIMARY — "what is murshid telling me, and what do I do about it?"**
- The active teaching card (concept, anchor, why, rule, rung) — the hero.
- The valid actions on it (a/g/u/n, e/t/k) — the contextual keybar.
- A pending struggle offer, when one exists — a focused y/n callout that
  *replaces* the card's action row (it is the one thing that legitimately
  interrupts, per D8/I10, but stays non-modal: keep typing and it expires).

**SECONDARY — "how am I doing / what happened?" (consulted, not watched)**
- The mastery meter (I24) — summoned with one key, full screen, then dismissed.
- The event/history feed — summoned, mostly for the shown/declined/dropped
  distinction the "silence is ambiguous" work exposed.
- The pull queue ("2 more thoughts — m") — a one-line presence indicator on
  the home surface; its contents are a lightweight list, not a whole tab.

**AMBIENT — "is the mentor healthy and paying attention?" (glanceable, never
demanding)**
- Watching / thinking / waiting-on-parse status (the pulse).
- Budget bucket (a slim gauge), throttled categories, current goal + drift,
  judge live/degraded. These live in a single dim status band and never move
  focus. A degraded judge is the one ambient item allowed to raise its voice
  (it colors up), because a silently broken mentor is the worst failure.

The redesign's whole job is to make the vertical rhythm of the screen *equal*
this ranking: the card owns the center and the most ink; ambient state is one
dim line; secondary views are off-screen until asked for.

---

## 3. Screen-by-screen mockups (real murshid content)

Content below uses the real sample card from `card.rs` tests (Borrow vs.
clone, `src/main.rs:42`) and realistic engine state. Box drawing is
illustrative of the ratatui `Block`/`Layout` structure described in §4–5.

### 3.1 HOME — a card on screen (the flagship; most detail)

The four-region vertical layout: **header (1) · surface (min) · ambient band
(1) · keybar (1)**. The card sits in a centered column with padding — it does
not stretch edge to edge, so it reads like a page, not a log.

```
 murshid                                              ● watching · src/main.rs
────────────────────────────────────────────────────────────────────────────

        ┌ ★ idiom · Borrow vs. clone ──────────────────── rung 2/3 ┐
        │                                                          │
        │   src/main.rs:42                                         │
        │   │ person.name.clone()                                  │
        │                                                          │
        │   The call only reads the name, so cloning the String    │
        │   allocates and copies data a borrow would have served   │
        │   just as well.                                          │
        │                                                          │
        │   Rule  Take &str when the function only needs to read   │
        │         the value.                                       │
        │         → pack-docs/redundant-clone                      │
        │                                                          │
        │   ↑ this pattern also appears at src/util.rs:88          │
        └──────────────────────────────────────────────────────────┘

                              2 more thoughts — m

────────────────────────────────────────────────────────────────────────────
 goal: ship the parser  ·  budget ▐▐▐▐▐░░ 3.4  ·  judge live
 [a] applied   [g] got it   [u] not useful   [n] not now   [e] more   [t] fix   [k] ask
```

Notes on what carries hierarchy here:
- **Header (row 1):** left `murshid` wordmark (dim, bold); right the *pulse* —
  `● watching · <file last judged>`. The dot is the single live-status glyph
  (see §3.4/§3.5 for its other faces). Rendered on a `REVERSED` 1-line block
  so it reads as a title bar in both light and dark terminals without
  hardcoding a background color.
- **Card title bar:** `★ idiom · Borrow vs. clone` on the left of the card's
  `Block` title; `rung 2/3` on the right (`title` + `title_alignment(Right)`
  via a second `title` span). The `★` glyph + `idiom` word carry the category;
  color (§4) is the redundant third channel. The border color is the
  category's accent.
- **Anchor:** `file:line` then the offending expression as a quoted line with a
  left rule (`│`), visually echoing a code gutter without any syntax
  highlighting (we have no syntect and don't want it).
- **Why / Rule:** the two prose fields, `Rule` as a labeled, slightly indented
  block with the doc ref on its own `→` line (a link with visible fallback,
  I21). Generous blank lines between fields — this is the "16px between
  sections, 8px within" spacing discipline translated to terminal rows.
- **Multi-site:** the T2 aggregation ("also at …") as one dim `↑` line inside
  the card, not a separate section.
- **Queue presence:** the existing `render_queue::presence_indicator` string
  ("2 more thoughts — m"), centered and dim, *below* the card — clearly
  secondary, clearly pullable, never stealing the card's frame.
- **Ambient band (row n-1):** goal · budget gauge · judge, all dim. One line.
- **Keybar (row n):** only the keys valid *right now* on this card, as `[k]
  label` chips. `e`/`t`/`k` are the escalation/disclosure keys.

### 3.2 HOME — empty state (caught up)

The most important state to get *right*, because it's the most common one and
the current UI renders it as the deadpan `(no card on screen)`. It should feel
like calm, not like nothing is running.

```
 murshid                                                        ● watching · idle
────────────────────────────────────────────────────────────────────────────




                                    ✓

                            You're all caught up.

                 murshid is watching. Keep coding — I'll speak
                    up when there's something worth a look.

                        last thought: 6m ago · 4 shown today




────────────────────────────────────────────────────────────────────────────
 goal: ship the parser  ·  budget ▐▐▐▐▐▐▐ 7.0  ·  judge live
 m mastery   e events   ? help   q quit
```

The `✓` is muted-success green (never red — empty is good). The one factual
line ("last thought 6m ago · 4 shown today") reassures that the pipeline is
alive and reads from `activity_log` / the events count. The keybar shrinks to
just navigation because there is no card to act on.

### 3.3 HOME — struggle offer pending (the one legitimate interrupt)

The offer replaces the card's action row with a single focused question. It is
still non-modal (I10): keep typing and it expires; the keybar shows only y/n
plus "ignore = keep working."

```
 murshid                                              ● watching · src/parse.rs
────────────────────────────────────────────────────────────────────────────

        ┌ ⚑ a moment ──────────────────────────────────────────────┐
        │                                                          │
        │   You've been on the same error in src/parse.rs for a    │
        │   while (3 reds, ~8 min).                                 │
        │                                                          │
        │   Want me to take a look?                                 │
        │                                                          │
        └──────────────────────────────────────────────────────────┘


────────────────────────────────────────────────────────────────────────────
 goal: ship the parser  ·  budget ▐▐▐▐▐░░ 3.4  ·  judge live
 [y] yes, look   [n] not now   (or keep typing — this fades)
```

Evidence is named ("3 reds, ~8 min" — I11 "show the evidence"), never
surveillance-flavored. The `⚑` glyph is amber; this callout uses a distinct
"attention" accent, not a category color, because an offer isn't a card.

### 3.4 HOME — working ("asking the model…")

When offer-accept spawns the blocking struggle judge on its background thread,
`WatchSession::busy` is `Some(label)`. The pulse changes and the surface shows
a calm working state — never a frozen screen.

```
 murshid                                                     ◐ thinking · src/parse.rs
────────────────────────────────────────────────────────────────────────────



                                  ◐  asking the model…

                             (this runs in the background —
                              the UI stays live, press q to quit)



────────────────────────────────────────────────────────────────────────────
 goal: ship the parser  ·  budget ▐▐▐▐▐░░ 3.4  ·  judge live
 q quit
```

The pulse glyph animates across ticks by cycling `◐ ◓ ◑ ◒` (or ASCII `|/-\`)
on the ~200ms poll the event loop already runs — a cheap, honest "I'm working"
signal that costs nothing (no new threads, no timers; just index the frame by
a tick counter). This directly answers the current "⏳ asking the model…" line,
which today is buried mid-dashboard.

### 3.5 HOME — waiting: file doesn't parse yet

`parse_waiting` is non-empty. This is a *deliberate* hold (C12 parse gate), and
the whole point of the dogfood fix was that it must not look like murshid is
broken. So it gets a first-class, clearly-benign surface state with a distinct
pulse glyph (`⏸`), not a buried line.

```
 murshid                                                          ⏸ waiting · parse
────────────────────────────────────────────────────────────────────────────



                          ⏸  waiting on a clean parse

                 2 files don't parse yet — I'll resume the moment
                          they do. Nothing to fix here but
                                 the syntax:

                              · src/parse.rs
                              · src/lexer.rs



────────────────────────────────────────────────────────────────────────────
 goal: ship the parser  ·  budget ▐▐▐▐▐░░ 3.4  ·  judge live
 m mastery   e events   ? help   q quit
```

Reuses the logic of the existing `view::parse_wait_line` (kept as the model;
the file list becomes the bulleted body). The `⏸` is a neutral/dim accent, not
a warning color — this is patience, not an error.

### 3.6 MASTERY — the meter (summoned overlay, real bars)

Pressing `m` opens mastery full-screen over the home surface. This is the
trust mechanism (I24), so it is the one place the UI gets to be a little
data-rich — but as bars, grouped by category, colored by state, with the state
word always present (never color-only).

```
 murshid · mastery                                          14 concepts · 3 active
────────────────────────────────────────────────────────────────────────────

  BUG
  ! use-after-move            ██████████░░░░░░░░░░  52%   help 2   learning
  ! unchecked-index           ████████████████░░░░  81%   help 1   3d ago

  IDIOM
› ★ Borrow vs. clone          ████████████░░░░░░░░  61%   help 2   just now
  ★ ?-operator                ███████████████████░  95%   help 0   backing off
  ★ iterator-vs-loop          ░░░░░░░░░░░░░░░░░░░░   —     help –   not yet seen

  BEST-PRACTICE
  ◆ error-context             ██████████████░░░░░░  70%   help 1   stale · 9d

────────────────────────────────────────────────────────────────────────────
 ↑/↓ move   ⏎ concept detail   m/esc home   ? help   q quit
```

- The bar is built from styled `Span`s (`█` filled / `░` empty), colored by
  **state** (green mastered, terminal-fg learning, amber stale, dim throttled),
  with the numeric `%` immediately after so meaning survives `NO_COLOR`.
- `backing off` renders the I24 fade announcement inline (concept at 95%,
  help 0) — the meter is *where* the withdrawal is stated, never silent.
- The selected row (`›` marker + `REVERSED`) is the drill-down cursor.
- `not yet seen` (zero-row) is dim with an em-dash bar — reads clearly as
  "unmeasured," matching `progress::render_row`'s "(not yet encountered)."

**Concept detail (the "level" / drill-down)** — `⏎` on a row pushes into a
detail view for that concept: its mastery curve as a `Sparkline` of past
grades, recent encounters, and when help last shifted. This is the founder's
"multiple views on the same data" — the meter is the overview, `⏎` is the
zoom.

```
 murshid · mastery › Borrow vs. clone                                      idiom
────────────────────────────────────────────────────────────────────────────

  mastery   ████████████░░░░░░░░  61%   help level 2   last seen just now

  trend     ▁▂▃▃▄▅▅▆   (8 encounters — climbing)

  recent
    just now   shown  · src/main.rs:42        (this session)
    2h ago     applied · src/util.rs:88
    1d ago     got it  · src/net.rs:integers
    3d ago     shown   · escalated to worked example

  help shifted 3 → 2 after two clean applications (2d ago)

────────────────────────────────────────────────────────────────────────────
 ↑/↓ move   esc back to meter   m/esc home   q quit
```

### 3.7 EVENTS — the history feed (summoned overlay)

Pressing `e` opens the session's event feed. Its main job is the
shown/declined/dropped distinction (the "silence is ambiguous" fix made
visual). Marker glyph + color + the kind word — three channels.

```
 murshid · events                                       session · filter: all
────────────────────────────────────────────────────────────────────────────

  10:32:14  ✦ shown       Borrow vs. clone            src/main.rs:42
  10:31:02  ✓ applied     ?-operator                  src/net.rs:17
  10:29:44  ~ declined    unchecked-index   not a teaching moment (judge)
  10:28:10  · offered     struggle → look?            src/parse.rs
  10:27:55  ! dropped     redundant-clone   contract failure (schema)
  10:24:03  · throttled   idiom now tail              rate limit
  10:19:30  ◆ goal        inferred: "ship the parser"

────────────────────────────────────────────────────────────────────────────
 ↑/↓ move   f filter (all › shown › declined › dropped)   e/esc home   q quit
```

- `✦ shown` (accent), `✓ applied` (green), `~ declined` (dim — a *correct*
  "not a teaching moment," not a failure), `! dropped` (red — a genuine
  contract failure). The declined-vs-dropped color/glyph split is the whole
  point.
- The right column shows the human-meaningful payload (concept + anchor +
  reason), not the raw `payload_json` the current UI truncates to 60 chars.
- `f` cycles the existing `EventsFilter`; the cycle is spelled out in the
  keybar so it's discoverable.

### 3.8 Card rungs — progressive disclosure in one surface

The same card slot renders differently by rung — this *is* the "levels" idea,
built in. Compact examples of the three the founder will see most:

**R0 — recall (generation moment):** a question, nothing revealed.
```
        ┌ ★ idiom · Borrow vs. clone ──────────────────── rung 0/3 ┐
        │                                                          │
        │   src/main.rs:42                                         │
        │   │ person.name.clone()                                  │
        │                                                          │
        │   How would you write this differently?                  │
        │                                                          │
        └──────────────────────────────────────────────────────────┘
 [e] hint   [t] show me   [k] ask   [n] not now
```

**R3 — worked example unfolds in place** (the folded fix line is replaced by
the commented worked diff, per I19). The diff renders with `+`/`-` gutter
glyphs colored green/red — the *only* place we tint code, and it degrades to
plain `+`/`-` which is already meaningful:
```
        │   worked example                                         │
        │   - fn print_name(name: String)                          │
        │   + fn print_name(name: &str)                            │
```

---

## 4. Visual language

### 4.1 Color roles (meaning-first, all 16-safe)

Every role is a `(glyph, ratatui color, word)` triple. Color is the *third*
redundant channel — remove it (NO_COLOR) and glyph+word still carry it.

| Role | Glyph | ASCII fallback | ratatui `Color` | Where |
|------|-------|----------------|-----------------|-------|
| Category: bug | `!` | `!` | `Red` | card border+title, meter, events |
| Category: idiom | `★` | `*` | `Yellow` | " |
| Category: best-practice | `◆` | `+` | `Cyan` | " |
| Category: architecture | `▲` | `^` | `Magenta` | " |
| Success / applied / mastered | `✓` | `v` | `Green` | empty state, events, meter |
| Declined (correct non-moment) | `~` | `~` | `DarkGray`/dim | events |
| Dropped (contract failure) | `!` | `!` | `Red` | events |
| Attention / offer | `⚑` | `!` | `Yellow` bold | struggle offer |
| Ambient / secondary text | — | — | `DarkGray` + `DIM` | status band, labels |
| Focus / selection | `›` | `>` | `REVERSED` | list cursor |
| Live pulse (watching) | `●` | `*` | `Green` | header |
| Working pulse | `◐◓◑◒` | `\|/-\` | `Yellow` | header |
| Waiting-on-parse | `⏸` | `=` | `DarkGray` | header, surface |
| Degraded judge | `▲` | `!` | `Red` | ambient band (allowed to shout) |

Body prose is always **`Color::Reset`** (the terminal's own foreground) so it
is correct in both light and dark themes — we never hardcode white or black
text. Backgrounds are avoided entirely except `REVERSED` (header bar, list
selection), which inherits the user's own colors and thus can't clash.

**Layered degradation (concrete):**
- **Monochrome / NO_COLOR:** `card::color_allowed()` already exists — extend
  the same gate. Glyphs + words + borders + spacing carry 100% of the meaning.
  Nothing in §3 becomes ambiguous without color.
- **16-color:** use the named `Color` variants above (they map to the user's
  palette). Do **not** use `Color::Rgb`/`Indexed(>15)` for meaning — reserve
  truecolor for at most a nicer dim gray, and always behind a capability check
  (default to `DarkGray`).
- **Truecolor:** optional polish only; never required.

Detect NO_COLOR via the existing helper; there is no need to probe terminal
capability beyond that for v1 (named colors already degrade in the emulator).

### 4.2 Borders, whitespace, hierarchy

- **The card is the only bordered box on the home surface.** Borders are
  expensive attention; per the whitespace research ("borders only if they add
  value"), we spend our one border on the hero and use *whitespace* everywhere
  else. The ambient band and keybar are borderless dim lines; the header is a
  reversed bar with a horizontal rule beneath it.
- **Centered reading column:** the card `Block` is placed in a centered rect
  (~62 cols wide, capped), via a horizontal `Layout` with flexible side
  margins — so on a wide terminal the card reads like a page, not a stretched
  banner. (Reuse the existing `centered_rect` helper, generalized.)
- **Vertical rhythm:** one blank row between card fields, two around the card
  itself. Section labels (`Rule`, `worked example`, category headers in the
  meter) are the hierarchy markers — bold or dim, never underlined.
- **Padding inside the card:** ratatui `Block::padding(Padding::horizontal(2))`
  gives the card interior breathing room without manual space-prefixing.

### 4.3 The mastery meter treatment (the gauge)

Not text. Options in ratatui 0.29, and the call:
- **Per-concept bar:** build from styled `Span`s — `█` × filled, `░` × empty —
  inside a `Table` or `List` row. This is preferred over `Gauge`/`LineGauge`
  per row because it composes cleanly with the `%`, help, and state columns in
  one line and colors by state. (`Gauge` is a whole-widget block; awkward to
  tile per row.)
- **Concept-detail trend:** ratatui **`Sparkline`** over the concept's grade
  history — the `▁▂▃▄▅▆▇` curve in §3.6. This is exactly what `Sparkline` is
  for, and it makes "am I actually learning?" visible (I25).
- **Budget (ambient band):** a slim inline bar (`▐▐▐▐▐░░`) or ratatui
  **`LineGauge`** — a fraction of capacity. This needs `TokenBucket` to expose
  `capacity()` (see §7, flagged data need); without it, fall back to the raw
  `tokens_available()` number as today.

---

## 5. Interaction model

### 5.1 The big call: single surface + summoned overlays, NOT gitui panels

The founder cited gitui as inspiration ("levels," "don't memorize shortcuts,"
"multiple views on the data"). It's tempting to copy gitui *literally*:
persistent side-by-side panels, `Tab` to move focus, the focused panel
brightened. **I'm rejecting the literal copy** and taking the *principles*
instead, because gitui's multi-panel model is right for its data (many git
objects you cross-reference simultaneously — status ↔ diff ↔ log) and wrong for
murshid's data (one card, read in full, that you act on and dismiss). Three
live panels would (a) starve the card of reading width, (b) resurrect exactly
the "dashboard that screams" I7 forbids, and (c) show mastery/events
permanently even though you consult them minutes-to-days apart.

So: **the home surface is always the card (or its empty/working/waiting/offer
face). Mastery and events are full-screen overlays you summon and dismiss.**
This keeps gitui's actual gifts — clear focus (there's only ever one thing
focused), contextual keys, and multiple views on the data (home / meter /
concept-detail / events are the "views," reached by drilling, not by tiling).

### 5.2 Navigation — drop numbered tabs, use named summon + Esc

Replace `1/2/3/4` + `Tab`-cycle with a verb-based model that reads off the
keybar:
- `m` → mastery overlay. `e` → events overlay. `⏎` on a meter row → concept
  detail. `esc` → pop one level back toward home. `q`/`Ctrl-C` → quit
  (unchanged: restore terminal then `run_shutdown_cleanup`).
- No number keys to memorize, no invisible tab order. The keybar always shows
  the summon keys available from where you are. This is the direct answer to
  "don't make me memorize shortcuts."
- `Tab` can be kept as a nicety (cycle home → mastery → events) for muscle
  memory, but it's no longer the primary model.

### 5.3 The contextual keybar

One dim line, always the bottom row, showing **only keys valid on the focused
object right now** — the current `draw_keybar` already branches on
tab/offer/card presence; the redesign keeps that logic and restyles it:
- Chips render as `[k] label` where `k` is accent/bold and `label` is dim. A
  disabled action is simply absent, never grayed-in-place.
- Card present: `[a] applied  [g] got it  [u] not useful  [n] not now  [e] more  [t] fix  [k] ask`.
- Offer present: `[y] yes, look  [n] not now  (or keep typing — this fades)`.
- Empty/waiting: `m mastery  e events  ? help  q quit`.
- Overlays: their own navigation set (see mockups).

### 5.4 How card responses and the offer *feel*

- Response keys are unchanged in behavior — they still call
  `keys::handle_card_key` / `keys::handle_offer_key` (the T15 requirement that
  a/g/u/n/e/t/y/n behave identically). Only the *presentation* changes.
- On a response, the card should **acknowledge and clear with a beat**: e.g.
  the card border flashes green on `a`/`g` for one tick, then the surface
  transitions to the next queued card or the caught-up empty state. Cheap to
  do on the existing tick loop (set an "ack until tick N" flag in `App`).
- The offer never occupies the card slot (it can't, per its own invariant) —
  it renders as the §3.3 callout and leaves any real card untouched behind the
  same "one thing focused" rule.

### 5.5 Help (`?`)

Keep the `?` overlay, restyled to match: a centered `Clear`ed box, but grouped
and glyph-labeled like the rest, and framed as "here's the whole map" rather
than a flat key dump. `?` remains a toggle; any key dismisses.

---

## 6. What to CUT / MERGE / ADD

**CUT**
- **The four co-equal tabs.** Home is not a tab; it's the app. Mastery/events
  are overlays. This is the single biggest clutter reduction.
- **The tab strip (row 1 reverse-video labels).** Replaced by the header
  pulse; the "where am I" is answered by the overlay title (`murshid ·
  mastery`) — a k9s-style breadcrumb, shown only when you're off home.
- **The all-in-one dashboard string.** `draw_dashboard`'s giant `push_str`
  block is dismantled: card → the hero surface; goal/budget/throttle/judge →
  the ambient band; recent-activity strip → folded into the events overlay and
  the empty-state "last thought" line.
- **Raw `payload_json` in events.** Render meaning, not JSON.

**MERGE**
- Dashboard + Card tabs → the single Home surface (they were always showing the
  same card; Card just added the read-only thread).
- The read-only thread transcript → into the card detail via `k` (it's
  read-only in the spike anyway; it doesn't need its own permanent tab).
- Busy / parse-wait / offer / empty → four *faces of the same home surface*,
  selected by state, rather than lines stacked in a dashboard.

**ADD**
- A real **empty/caught-up** state (§3.2) — today's `(no card on screen)` is
  the worst line in the UI.
- The **animated pulse** in the header (watching / thinking / waiting).
- **Concept-detail drill-down** with a `Sparkline` trend (§3.6) — the "levels"
  the founder wanted, and the sharpest expression of I24/I25.
- **Response acknowledgment** (the green-border beat) so actions feel like they
  landed.
- A **budget gauge** (needs `capacity()`, §7).

---

## 7. Build plan (ordered, for the implementer)

Each step is independently shippable and leaves the app working. Do them in
order; step 1 alone will already make the founder feel the difference.

**Step 0 — `theme` module (foundation, ~1 file, pure).** Add
`src/tui/theme.rs`: the `(glyph, color, ascii_fallback, label)` role table
from §4.1, a `no_color()`/`glyphs_ok()` gate (reuse `card::color_allowed`), and
small helpers `category_style(&Category)`, `state_style(&ConceptState)`,
`chip(key, label)`. No behavior change; everything else builds on it.

**Step 1 — the Home card as the hero (highest impact).** Rewrite the card
rendering to emit ratatui `Line`/`Span`s instead of one `Paragraph` string:
- New `view::render_card_block(&PendingCard, &SurfaceConfig) -> Vec<Line>`
  producing the §3.1 layout (title bar with category glyph+color and `rung
  n/3`, gutter anchor, why, labeled Rule + `→` doc line, multi-site line).
  Source fields all already on `PendingCard.card`; rung on `PendingCard.rung`.
- Place it in a centered, padded `Block` (generalize `centered_rect`).
- Restructure `view::draw` into the 4-region layout (header / surface /
  ambient band / keybar) — replaces the current 3-row `Length(1)/Min/Length(2)`.
- Ship the empty state (§3.2) and the queue presence line here too.
This step touches `view.rs` heavily and `mod.rs` barely. No new engine data.

**Step 2 — header pulse + ambient band.** Add the header (reversed 1-line
block, wordmark + pulse) and the dim ambient band (goal · budget · judge).
Pulse state derives from existing `WatchSession`: `busy.is_some()` →
thinking; `!parse_waiting.is_empty()` → waiting; else watching. Animate by a
tick counter in `App` (increment each poll iteration in `mod.rs`).

**Step 3 — working / parse-wait surface faces (§3.4/§3.5).** Promote
`busy`/`parse_waiting` from buried lines to full home-surface states, reusing
`parse_wait_line`'s content. Pure presentation.

**Step 4 — offer callout (§3.3).** Render `pending_offer` as the centered
`⚑` callout with named evidence; keybar switches to `[y]/[n]`. Behavior via
existing `keys::handle_offer_key` — unchanged.

**Step 5 — response acknowledgment beat.** Add `ack_until_tick` to `App`; on a
card response, flash the card border green for a few ticks before the surface
re-renders the next state. Small, delightful.

**Step 6 — mastery overlay with real bars (§3.6).** Move `draw_mastery` behind
the `m` summon; render `progress::build_rows` as a `Table`/`List` with
span-built `█/░` bars colored by `ConceptState`, category group headers, `%`,
help, age, state word, and the I24 "backing off" line. `progress::build_rows`
already returns everything needed.

**Step 7 — concept detail + `Sparkline` (§3.6 detail).** `⏎` pushes to a
concept-detail view. **Flagged data need:** a per-concept encounter/grade
history query — likely a new `db` read (e.g. `get_concept_events(conn,
concept_id)` filtering the events table by concept), feeding the `Sparkline`
and the "recent" list. This is the one step needing new engine-side data.

**Step 8 — events overlay (§3.7).** Move `draw_events` behind `e`; map each
`kind` to its `(glyph, color, word)` and render a human payload column instead
of raw JSON. Keep `EventsFilter`/`f`. May want a small payload-summarizer
helper (pure) that turns known event payloads into the right-column text.

**Step 9 — navigation swap + keybar restyle.** Replace `1-4` with `m`/`e`/`⏎`/
`esc` summon+pop in `handle_key` (`App` gains an overlay/nav stack — a tiny
enum `Focus { Home, Mastery, ConceptDetail(id), Events }`). Restyle
`draw_keybar` into `[k] label` chips driven by the current focus + card/offer
presence.

**Flagged new render/data needs (only two):**
1. `budget::TokenBucket::capacity()` getter (currently private) — for the
   gauge fraction in the ambient band (Step 2). Trivial, no invariant impact.
2. A concept-scoped events query for the detail sparkline/history (Step 7).
   Everything else in the design is already reachable from `WatchSession` +
   the existing pure render inputs.

Recommended first PR: **Steps 0–2** (theme + hero card + header/ambient). That
is the "I don't like the UI" → "oh, I like this" moment, and it's
self-contained in `theme.rs` + `view.rs` with minimal `mod.rs`/`app.rs`
changes.

---

## 8. Alternatives considered (and why rejected)

**A. Literal gitui multi-panel dashboard** — persistent card-list ┃ card-detail
┃ mastery, `Tab` to move focus, focused panel brightened. *Rejected:* murshid
shows one card at a time (D10) and you read it in full then dismiss it — there
is no list of simultaneous objects to cross-reference the way git status/diff/
log demand. Three live panels starve the card of width and rebuild the
"dashboard that screams" I7 forbids. We took gitui's *principles* (single clear
focus, contextual keys, multiple views reached by drilling) without its layout.

**B. Minimal reskin of the four tabs** — keep the tab strip and the four
`Paragraph`s, just add borders, color, and a nicer keybar. *Rejected:* cheap,
but it treats the symptom (ugly text) not the disease (co-equal tabs bury the
one moment that matters). The card would still be one gray block among four
equal tabs. This design costs a bit more and fixes the actual problem.

**C. Chat/REPL scrolling transcript** — a single scrolling conversation of
cards and responses, like a chat client. *Rejected:* T15's whole point was to
leave the scrolling pane behind; a transcript re-buries the *current* card
under history, and history is exactly the secondary/ambient thing the hierarchy
(§2) says to demote. The events overlay already serves "what happened"
without pushing it into the primary surface.

---

## Sources (TUI research)

- Lazygit — panels always visible, always-clear focus, contextual vim-style
  keys, strong visual consistency: <https://github.com/jesseduffield/lazygit>,
  <https://www.bwplotka.dev/2025/lazygit/>
- gitui — the founder's Rust inspiration; feature/focus priorities:
  <https://github.com/gitui-org/gitui>
- k9s — breadcrumb "where am I," pop-up command bar shown only when needed,
  color-as-state (running/error/terminating), skin/invert for light-dark:
  <https://k9scli.io/>, <https://k9scli.io/topics/skins/>
- Spacing / whitespace discipline (distinct spacing per hierarchy level;
  borders only when they add value):
  <https://uxplanet.org/principles-of-spacing-in-ui-design-a-beginners-guide-to-the-4-point-spacing-system-6e88233b527a>,
  <https://www.canva.com/learn/white-space-design/>
- Layered TUI color discipline (monochrome → 16-color → truecolor, each tier
  independently usable): <https://www.remoteopenclaw.com/skills/hyperb1iss/hyperskills/tui-design>
```
