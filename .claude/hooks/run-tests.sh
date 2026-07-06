#!/usr/bin/env bash
# PostToolUse test runner.
# Runs the Rust test suite (`cargo test`) after Claude writes/edits a file. On failure it
# writes the output to stderr and exits 2, which makes Claude read the
# failures and keep working — no copy-paste needed. On success it exits 0.
set -uo pipefail

PROJECT_DIR="${CLAUDE_PROJECT_DIR:-$(pwd)}"
cd "$PROJECT_DIR" || exit 0

mkdir -p .claude/logs

if cargo test >.claude/logs/test.log 2>&1; then
  echo "cargo test: all passing"
  exit 0
else
  {
    echo "cargo test FAILED. Make the implementation satisfy the active spec in specs/."
    echo "--- test output ---"
    cat .claude/logs/test.log
  } >&2
  exit 2
fi
