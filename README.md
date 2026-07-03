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

Inside `watch`, cards take single-key responses: `a` applied · `g` got it ·
`u` not useful · `n` not now (snooze) · `e` escalate help · `t` tell me ·
`k` ask a follow-up (opens a thread).

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
