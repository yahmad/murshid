#!/usr/bin/env bash
# Cross-harness hand-off: dispatch ONE implementation task to Gemini via the
# Antigravity CLI. It runs on your Google AI Pro subscription (OAuth), so it
# does NOT consume Claude quota. The Opus orchestrator (in Claude Code) calls
# this, then reviews the resulting git diff.
#
#   Usage:  ./handoff.sh specs/tasks/0007-height-normalization.md
#
# Prereqs:
#   - Antigravity CLI installed and authenticated once with your Google account.
#   - Run from the repo root; Gemini edits files in place here.
#   - The CLI is young and its flags change — verify against `agy --help`
#     and `agy changelog` for your installed version, then adjust below.
set -euo pipefail

TASK_FILE="${1:?usage: handoff.sh <task-spec-file>}"
MODEL="${HANDOFF_MODEL:-gemini-3.5-flash}"

[ -f "$TASK_FILE" ] || { echo "task file not found: $TASK_FILE" >&2; exit 1; }

echo ">> Dispatching $TASK_FILE to Gemini ($MODEL) via Antigravity..."

# Wrap in a pseudo-TTY (`script`) to dodge the agy non-TTY stdout-drop bug,
# and pass --yes plus </dev/null so no confirmation prompt can hang the run.
# We do NOT rely on captured stdout — Gemini's real output is the file edits.
script -q /dev/null antigravity run \
  --prompt-file "$TASK_FILE" \
  --model "$MODEL" \
  --yes \
  < /dev/null || { echo "handoff failed (check auth / flags with: agy --help)" >&2; exit 1; }

echo ">> Gemini finished. Changes on disk:"
git --no-pager diff --stat
echo
echo ">> Orchestrator: review this diff against $TASK_FILE, run 'go test ./...',"
echo "   then APPROVE or write a revised task and re-dispatch."
