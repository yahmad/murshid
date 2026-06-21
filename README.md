# SDD Go Starter — loop smoke test

A minimal spec-driven Go project that exercises the full agentic loop:
**spec → plan → implement → verify**, with hooks that run `go test` on every
edit and feed failures back to the agent automatically.

If this runs end to end, your toolchain and the loop are working, and you can
scale up to real specs.

---

## 1. One-time machine setup (macOS)

```bash
# Homebrew (skip if already installed)
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# Toolchain
brew install go git node

# Claude Code (needs Node 18+)
npm install -g @anthropic-ai/claude-code

# First run authenticates with your Claude subscription
claude
```

Verify:

```bash
go version && node --version && claude --version
```

## 2. Set up this project

```bash
# from the unzipped folder
cd sdd-go-starter
git init && git add -A && git commit -m "chore: SDD starter scaffold"
chmod +x .claude/hooks/run-tests.sh
go mod tidy
```

Confirm the gate fails as expected (the implementation is an unsolved stub):

```bash
go test ./...   # expect FAIL — NormalizeWeight is not implemented yet
```

## 3. Run the loop

```bash
claude
```

Then, inside the session, drive it spec-first. A good opening prompt:

> Read specs/INDEX.md and the active spec it points to. Make a short plan, then
> implement it. Do not add scope beyond the spec. The test hook runs
> automatically after each edit.

What you should observe:

1. **SessionStart hook** prints the spec index when the session opens.
2. The agent reads `specs/0001-weight-normalization.md` and plans.
3. It edits `internal/units/units.go`.
4. The **PostToolUse hook** runs `go test`; while tests fail, the failures are
   fed back and the agent keeps fixing — no copy-paste from you.
5. When `go test ./...` passes, the **Stop hook** asks a fresh check of whether
   the spec is actually complete before wrapping up.

Success = tests green, no `TODO` left, and you understood the diff. Read the
generated code; if any Go idiom is unfamiliar, ask the agent to explain it
before you accept. That is the point of using these as learning projects.

---

## What's in here

| Path                                   | Role                                          |
|----------------------------------------|-----------------------------------------------|
| `specs/INDEX.md`                       | Pointer to the active spec (loaded at startup)|
| `specs/0001-weight-normalization.md`   | The spec — source of truth, six SDD elements  |
| `internal/units/units.go`              | Stub the agent must implement                 |
| `internal/units/units_test.go`         | Verification gate (encodes the spec's cases)  |
| `CLAUDE.md`                            | Project conventions, as factual statements    |
| `.claude/settings.json`                | Hooks: SessionStart / PostToolUse / Stop      |
| `.claude/hooks/run-tests.sh`           | Test runner; exit 2 on failure to self-correct|

## Notes

- The `Stop` hook uses a `prompt`-type hook (LLM completion check). Prompt hooks
  must be added by editing `settings.json` directly — the `/hooks` menu only
  handles command hooks.
- The test runner is synchronous, which is fine for fast Go suites. For slower
  suites, switch it to write the log asynchronously and tail it via a
  `UserPromptSubmit` hook so the agent loop never stalls.
- Everything here is committed to the repo, so hooks and conventions are shared
  if this ever becomes a team project.
