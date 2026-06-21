# Cross-harness orchestration: Opus orchestrates, Gemini implements

Opus (Claude Code) keeps the judgment work; Gemini (Antigravity) does the
high-volume implementation on your Google sub. The shared git repo is the bus.
This keeps most tokens off your Claude quota.

## The loop

1. **Opus (orchestrator)** reads the active spec, plans it into units, and for
   each unit writes a precise task file in `specs/tasks/NNNN-*.md` (use
   TEMPLATE.md). Commits it.
2. **Gemini (implementer)** receives the task and edits the repo to satisfy it.
3. **Opus (reviewer)** reads the `git diff`, runs `go test ./...`, and either
   approves or writes a revised task and re-dispatches.
4. Loop until approved and tests are green.

Opus never writes implementation code. Gemini never decides a unit is done.

## Two ways to do step 2

### A. Manual hand-off (start here — robust, zero new wiring)

Opus writes the task file. You switch to Antigravity (desktop or `agy`), tell
Gemini to implement `specs/tasks/NNNN-*.md`, let it work, then switch back and
have Opus review the diff. You are the courier; nothing can silently break.

### B. Automated hand-off (slicker, more setup)

The Opus orchestrator shells out to `./handoff.sh specs/tasks/NNNN-*.md`, which
dispatches the task to Gemini via the Antigravity CLI on your Google OAuth.
Gemini edits the repo; Opus reads the diff and reviews. One Claude Code session
drives the whole loop.

Give the orchestrator this instruction so it uses the script:

> For each unit, write the task to specs/tasks/, then run
> `./handoff.sh <that file>` to have Gemini implement it. After it returns,
> review the git diff against the task, run `go test ./...`, and approve or
> revise. Do not implement code yourself.

## Caveats for the automated path

- **Quota:** runs on your Google sub (good), but consumer OAuth has daily
  request limits. Fine for discrete task hand-offs; heavy automated volume
  needs a metered AI Studio key, which defeats the purpose.
- **Stdout bug:** the CLI can drop its final stdout under a non-TTY. handoff.sh
  works around it with a pseudo-TTY AND by reading the git diff, not stdout.
- **Flags churn:** the Antigravity CLI is young. Verify `antigravity run` flags
  with `agy --help` / `agy changelog` and adjust handoff.sh.
- **Containers:** if you run inside the sandbox, the Antigravity CLI needs its
  own auth there. Authenticate once and mount its credential directory like you
  do for `~/.claude`, or run the orchestrator on the host.

## Recommendation

Do A first on the weight-normalization spec to prove the loop shape. Move to B
once it works and the courier step gets tedious.
