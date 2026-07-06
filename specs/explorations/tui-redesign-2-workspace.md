# TUI UX Redesign, round 2 — "one living workspace" (2026-07-06)

**Status:** MERGED to main 2026-07-06 (`988be41`). Originally an exploration /
proposal — not a spec yet, for founder review before any build; see
[specs/INDEX.md](../INDEX.md) for the shipped rung-by-rung record.
**Supersedes-in-spirit:** [`tui-ux-redesign.md`](tui-ux-redesign.md) (the "Focus"
design that shipped as T15). Focus fixed the *dashboard-that-screams* problem by
making the card a hero; this round fixes what Focus left incoherent — the pile of
full-screen overlays around that hero. Focus stays the center of gravity; this
gives everything *around* it a coherent model.
**Inputs:** three research passes (2026-07-06) — a best-in-class TUI pattern
catalog, a coherence audit of the shipped Focus TUI, and a personas +
testing-flows + gap analysis. All three converge on the same verdict.

---

## TL;DR — the one big move

The Focus redesign got the *center* right (one hero card) but the *periphery*
wrong: mastery `m`, settings `s`, events `E`, history `h`, goal `G`, help `?` are
each a **full-screen overlay summoned by an ad-hoc, inconsistently-cased letter
key**. The 4-region frame (header · surface · ambient band · keybar) is coherent;
**the surface region and its navigation are not** — one region renders four
unrelated layout idioms, "overlay" means two different things, and every datum is
siloed to exactly one screen.

Replace the overlay-drawer with **one living workspace** governed by four
disciplines from the tools that feel coherent (gitui, lazygit, k9s, helix, bacon):

1. **One persistent frame; acting changes a pane's contents, not the screen.**
2. **The keybar is the interface's memory** — a key exists on a surface only if the
   footer shows it; `?` covers the long tail; a `:` command covers "go elsewhere."
3. **Escape is a spring, not an ejector seat** — WATCHING is the one home; `Esc`
   always climbs toward it; only `q`-at-home quits.
4. **Ambient default (the bacon model)** — the resting state is near-empty; detail
   escalates transiently and recedes on its own.

Plus one hard prerequisite that isn't a layout problem but blocks everything:
**settings must persist** (G5).

---

## Diagnosis (why it still feels off, post-Focus)

- **Mixed mental models.** The surface slot hosts four idioms — centered bordered
  card, centered prose, full-width flush-left list, centered reading column — with
  nothing but header text signaling which you're in. "Overlay" means both
  *popup-over* (help, goal) and *full-screen-swap* (mastery/events/settings/history).
- **Navigation friction.** Home faces are mutually exclusive, so you never see a
  card + your progress + recent activity together. Opening any overlay **blinds you
  to live mentor-state and to a card firing behind it** (G14). The overlays you most
  want — mastery ("am I improving on *this*?"), settings ("turn it down, now") — are
  **dead while a card is up** (G3), exactly when you want them.
- **Discoverability gaps.** `m/s/E/h/G/?` are learnable only via `?` or luck; `E` is
  advertised on no keybar; the empty-home hint says press `g` but the binding is `G`
  (and `g` there does nothing) — a dead advertised affordance.
- **Data siloing + trust gaps.** No dashboard; each datum lives in one screen.
  Proactive cards don't say **why they fired** (offers do) — to a skeptic that reads
  as spam (G4). Settings **don't persist** — the one action a skeptic or team-lead
  takes to accept the tool silently evaporates (G5).

---

## The new surface model — four modes, each with a rule

The fix for "panes vs full-window vs card" is not to pick one — it's to give each a
**clear job**, so the user always knows which they're in and why. Today there are
four idioms with **no rule**; here there are four with a **rule for each**, legible
from the mode token in the header.

| Mode | When | What | Rule |
|---|---|---|---|
| **Focus pane** (card / watching) | always — the base | the current hint, or the calm resting state | the center of gravity; never taken over except to read deeply |
| **Rail** (optional split) | toggled (`Tab`) | persistent secondary state: mastery-at-a-glance, recent activity, "N waiting" | never blocks the card; the card stays live beside it |
| **Full-screen** | drilling into deep reading only | full history transcript, concept-detail trend | reserved for *reading*, reached by `Enter` on a rail row; `Esc` springs back |
| **Transient popup** | a momentary edit | goal, settings, `?`, `:` command | layers *over* without leaving; one action, then gone |

### Resting (WATCHING) — ambient, near-empty

```
┌ murshid · watching · src/main.rs ································· judge ● live ┐
│                                                                              │
│                     ✓  all set — I'm reading along.                          │
│              When you get stuck, a hint appears right here.                  │
│                     goal: get comfortable with ownership                     │
│                                                                              │
├──────────────────────────────────────────────────────────────────────────┤
│ goal: ownership · next nudge ~5m · 2 concepts climbing                      │
│ g goal · tab panels · : commands · ? help · q quit                          │
└──────────────────────────────────────────────────────────────────────────┘
```

### A hint — card-hero (kept), with the two additions the personas demanded

```
┌ murshid · hint · borrow-vs-clone ···························· rung 2/3  ▓▓▓░░ ┐
│  ⓘ flagged because this pattern recurs 3× in this file        ← why it spoke │  (G4)
│  │ let name = person.name.clone();                                           │
│                                                                              │
│  Cloning here copies the whole String just to read it. A borrow (&) lets     │
│  the callee read it without taking ownership or allocating.                  │
│  Rule  prefer &str for read-only access  → std::primitive::str               │
├──────────────────────────────────────────────────────────────────────────┤
│ goal: ownership · next nudge ~5m · judge ● live                              │
│ e explain more · t show the fix · a ask a question · g got it · n dismiss    │  (G6: verbose keybar teaches the ladder)
└──────────────────────────────────────────────────────────────────────────┘
```

### Rail toggled (`Tab`) — operator mode, card stays live (fixes G3/G13/G14)

```
┌ murshid · hint · borrow-vs-clone ···························· rung 2/3        ┐
│ concepts            ▓  │  ⓘ flagged: recurs 3× in this file                  │
│ › borrow-vs-clone ▓▓▓░ │  │ let name = person.name.clone();                  │
│   string-vs-str   ▓▓░░ │                                                     │
│   match-ergonomics ▓░░ │  Cloning copies the whole String just to read it…   │
│ ─ recent ───────────── │  Rule  prefer &str for read-only  → …str            │
│ · answered clone Q     │                                                     │
│ · nothing to flag 2m   │                                                     │
├────────────────────────┴────────────────────────────────────────────────────┤
│ ↑↓ concept · ⏎ open history · tab hide · e/t/a/g/n card · ? help              │
└──────────────────────────────────────────────────────────────────────────┘
```

The rail is the **master-detail spine** (gitui/lazygit/yazi): mastery-at-a-glance
as compact braille gauges (never a screen you visit), recent activity survives
here, `Enter` drills to full-screen deep reading. **Off by default** (ambient),
toggled with `Tab` — and because it doesn't block the card, mentor-state and "N
waiting" never disappear.

### Ask mode — the coming conversational follow-up (T16c), made legible

```
┌ murshid · ask · borrow-vs-clone ·············································· ┐
│  Cloning copies the whole String just to read it…                            │
│  ───────────────────────────────────────────────────────                    │
│  you › why does &mut fix the second error but & doesn't?                     │
│  murshid › because the callee mutates it — & is read-only, so…               │
│                                                                              │
│  › ask a follow-up▏                                                          │
├──────────────────────────────────────────────────────────────────────────┤
│ ⏎ send · esc back to card                                                    │
└──────────────────────────────────────────────────────────────────────────┘
```

The **mode token flips to `ask`** and the keybar changes — unambiguous that `e`/`t`
are now literal text, not ladder keys (the helix modal-status lesson).

### Transient edit — settings/goal as a which-key popup, not a screen

```
                    ┌ settings ─────────────────────┐
                    │ frequency  ‹ ambient · normal › │
                    │ directness ‹ guide · balanced › │
                    │ ✓ saved to config              │   (G5: persists!)
                    └ ←/→ change · esc close ────────┘
```

Goal and settings stop being full-screen views; they layer over the current focus
pane (magit-transient / which-key style), so you never lose your place — and the
popup *is* the documentation.

---

## How the four disciplines land as concrete rules

- **One frame, contextual contents.** The header keeps an always-visible **mode
  token** (`watching · hint · ask · reading · —`), so mentor-state never vanishes
  when you open something (fixes the disappearing-pulse audit finding).
- **Keybar-as-memory.** Every surface's footer lists exactly its live keys — and
  only live keys. No more "`h`/`E` work over a card but aren't shown" or "`m`/`s`
  shown-but-dead." `?` opens the full contextual cheat-sheet on *every* surface. `:`
  opens a command entry (`:history`, `:settings`, `:concept borrow`) so bare keys
  stop proliferating and new surfaces never need a new global hotkey.
- **Escape is a spring.** WATCHING is the sole home. `Esc` always pops one level
  toward it; a slim breadcrumb shows depth (`hint › ask`). `q` quits **only from the
  resting home** — killing the "does Esc close the popup or the program?" anxiety.
- **Ambient default.** Resting frame is near-empty and doubles as first-run
  orientation (G1): one dimmed example card + one line + one arrow. Offers arrive
  gently and recede; "ignoring is fine" is stated once.

---

## Gap → fix map (so nothing gets lost)

**Tier 0 (abandonment):**
- **G5 settings persistence** — *prerequisite, not layout.* Persist frequency +
  directness (+ goal) to `config.toml`; the transient shows "✓ saved to config."
  Nothing else matters until this is fixed. **Lead the effort with it.**
- **G4 why-it-spoke** — every proactive card gets a one-line trigger rationale
  (`ⓘ flagged because…`), the legibility offers already have. (Fed by T16.)

**Tier 1 (major friction):**
- **G3 dead-while-card-up** — rail + transients open over/beside a live card;
  nothing gated on idle anymore.
- **G14 live events behind overlays** — mode token + "N waiting" persist in
  header/ambient band; secondary views are rail/transients, not blinding full-screens.
- **G2 discoverability** — keybar advertises view-summoning too; `:` command; `?`
  everywhere. Fix the dead `g`→`G` hint and the missing `E` chip.
- **G1/G15 first-run + legible model** — calm one-time orientation on the resting
  frame; mode token + rail make the product's shape self-evident.

**Tier 2/3 (trust, retention, polish):**
- **G9 conversation** — `ask` mode = T16c; makes `k`/`a` real.
- **G10 mastery framing** — rail shows *climbing* concepts + progress, hides
  never-seen ones by default (no wall of empty bars for a beginner).
- **G11 continuity** — "welcome back — yesterday you climbed X" beat on the resting
  frame for a returning session.
- **G8 offer legibility**, **G7 benign parse-wait phrasing**, **G6 ladder language**,
  **G12 goal→behavior signal** (rail shows goal-cluster), **G16 calibration summary**
  (`:calibration` view of T14 outcome-rates for a lead), **G13 card+trend together**
  (the rail); plus the audit bugs: **slug→human-name in the history list**, **relative
  time in events**, consistent `Esc`/dismiss semantics, retire the `k` dead-end into
  `a ask`.

---

## Migration ladder (incremental; each rung ships something usable)

A redesign, not a rewrite — it maps onto the existing 4-region frame and the
`focus_stack`, and dovetails with T16.

- **R0 — settings persistence (G5).** Config write-back + "✓ saved". Small,
  non-visual, unblocks trust. Do first, independent of everything.
- **R1 — keybar + escape + discoverability hygiene.** Honest/complete keybar per
  surface, `?` everywhere, consistent `Esc`-springs-home, fix the dead `g` hint /
  missing `E` / slug+timestamp formatting. Pure cleanup on today's model; immediate
  coherence win, low risk.
- **R2 — the rail (optional split) + persistent mode token.** `Tab`-toggled
  master-detail rail (mastery-glance + recent + "N waiting") beside a live card;
  mentor-state into the always-visible header token. Fixes G3/G13/G14/G15. Biggest
  structural change; gated.
- **R3 — transients for goal + settings; `:` command entry.** Convert the two edit
  overlays to which-key popups; add the command palette. Fixes G2 at the root.
- **R4 — card "why it spoke" + ladder legibility + orientation + continuity.**
  Content/affordance polish (G4/G6/G1/G11), partly fed by T16's perception work.
- **R5 — `ask` mode (= T16c conversational follow-up).** Lands on the finished
  frame, mode token + changed keybar making input-state unambiguous.

**T16 intersection:** R4's "why it spoke" and R5's `ask` mode *are* T16 surfaced —
T16 supplies the perception/why and the conversational answer; this redesign
supplies the coherent place to show them. Build the frame (R0–R3) first so T16's
UI rungs land on it rather than being built twice.

---

## Open decisions for the founder

1. **Rail default:** off (ambient, `Tab` to reveal) — recommended — vs. on for
   "operator" users. (Could be a persisted setting once G5 lands.)
2. **`:` command palette:** now (future-proofs surface growth) or defer until there
   are more surfaces?
3. **Sequencing vs T16:** do R0–R3 (the frame) before T16b/c so the intelligence
   lands on the redesigned surface — recommended — or interleave?
4. **Scope of this pass:** ship R0+R1 immediately (persistence + hygiene — high
   value, low risk) and treat R2+ as the deliberate redesign, or hold all of it as
   one coherent piece?
