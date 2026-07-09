# Murshid — Local-First AI Coding Mentor

`murshid` is a local-first pedagogical agent for software engineers: an expert
"looking over your shoulder" while you work in languages and tooling you're
still learning. It watches your project, intercepts compiler diagnostics as
you save, and surfaces short mentor cards — flagging bugs, non-idiomatic
code, and improvable structure — with help that scales from direct answers
down to Socratic questions, depending on your configured directness and your
measured mastery of each concept. It never writes your code.

Everything runs on your machine. Code and diagnostics leave it only for the
model endpoint you configure (bring your own key, or a fully local model).

## Commands

```text
murshid setup [path]     Onboard a project (.env key import → OS keyring, .gitignore)
murshid watch [path]     Watch a project; mentor cards appear as you save
murshid goal [text]      Print or set the session goal
murshid review [path]    Solicited batched review of the session diff
murshid progress         Per-concept mastery meter (help level, staleness)
```

`watch` opens a full-screen terminal UI. Home is a scrollable **stream** of
hint cards — the live hint pinned on top, everything older browsable below
(`↑/↓` to browse, `⏎` opens a card in a split detail pane with its
conversation thread). Hints arrive directly and passively: spaced by a
single `min_gap` cooldown (default 8m; `off` mutes), one at a time, never
stealing focus. Card responses are single keys — `a` applied · `g` got it ·
`y` 👍 useful · `u` not useful · `n` not now (snooze) · `e` explain more ·
`t` show the fix · `k` ask a follow-up (a threaded conversation on any
card, current or past). `Tab` reveals a side rail (mastery-at-a-glance,
recent activity, what's queued); `s` adjusts cadence and directness (saved
to config); `G` sets a session goal; `:` is a command palette; `?` lists
every key for the current view. The anti-nag mechanism is learning, not
consent dialogs: what you dismiss stays dismissed — across sessions and
code sites — and what you've mastered goes quiet until it's genuinely due
again.

## Language support

Languages are data-driven packs (`packs/<lang>/`): diagnostics adapter,
concept taxonomy, canon references, prompts, and a tree-sitter grammar.
Shipped: **Rust** (`cargo check`) and **Go** (`go build` / `go vet`).
Adding a language requires no engine changes.

## Models

Two independent slots in config — a cheap/fast **screen** (triage: finds
candidate teaching moments) and a strong **judge** (writes the actual card,
grades responses):

```toml
# .murshid/config.toml (project) or ~/.config/murshid/config.toml (user)
[models.screen]
provider = "gemini"          # gemini | claude | ollama | lmstudio | openai
model = "gemini-3.1-flash-lite"

[models.judge]
provider = "gemini"
model = "gemini-3.5-flash"
```

Cloud providers resolve keys from the OS keyring (populated by
`murshid setup` from your `.env`) or environment variables. Local providers
(`ollama`, `lmstudio`, or any OpenAI-compatible server via `base_url`) need
no key. Without a usable key, `watch` runs in degraded observe-only mode.

## Development

Specifications in `specs/` are the source of truth; the active task is
tracked in `specs/INDEX.md`. Standard library plus the crates pinned in
`Cargo.toml` only.

```bash
cargo test    # verification — the whole suite must stay green
```

## License

Copyright (C) 2026 Yasir Ahmad. Licensed under the **GNU Affero General Public
License v3.0** — see [`LICENSE`](LICENSE). Network use counts as distribution.

Open-core: this repository (the engine, CLI, and terminal UI) is AGPL and free
to use, study, modify, and redistribute under those terms. A future desktop
application built on this core may be offered separately under a proprietary
license; as the sole copyright holder, the author reserves the right to
dual-license. (Contributor terms / CLA will accompany the first external
contributions.)
