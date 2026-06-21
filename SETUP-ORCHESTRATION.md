# Setup: orchestration + container sandbox

Unzip this over your existing `sdd-go-starter/` folder (it only adds files,
nothing is overwritten). New files:

```
Containerfile                       # sandboxed Linux dev env (Go + Node + Claude Code)
dev.sh                              # build + enter the sandbox (Apple container CLI)
ORCHESTRATION.md                    # the orchestrator loop, explained
.claude/agents/implementer.md       # Sonnet worker subagent
.claude/agents/reviewer.md          # Opus reviewer subagent (read-only)
.claude/ccr-config.example.json     # OPTIONAL Gemini routing
```

## 1. The container sandbox (macOS 26)

Install Apple's container CLI once:

```bash
brew install --cask container       # or download from github.com/apple/container
```

Then, from inside the project folder:

```bash
chmod +x dev.sh
./dev.sh
```

This builds the image and drops you into a Linux sandbox with the repo mounted
at `/work` and your Claude credentials shared. Run `claude` inside it the first
time and finish the login in your Mac browser. Nothing installs on the host.

## 2. The orchestration loop

Inside the sandbox, launch Claude Code, make sure you're on Opus, and paste the
starting prompt from `ORCHESTRATION.md`. The main session plans, the
`implementer` subagent writes code, the `reviewer` subagent checks it against the
spec, the test hook gates, and the orchestrator loops until review passes.

## 3. Gemini

Use Antigravity for whole-repo passes and second-opinion reviews of the repo —
no setup needed beyond your Google AI Pro sub.

Only if you want Gemini *inside* Claude Code to save Claude quota: get a free
API key from aistudio.google.com, install Claude Code Router
(`npm i -g @musistudio/claude-code-router`), copy `.claude/ccr-config.example.json`
to `~/.claude-code-router/config.json`, set `$GEMINI_API_KEY`, and run `ccr code`
instead of `claude`. The example config keeps edits and review on Claude and only
sends throwaway + long-context work to Gemini.
