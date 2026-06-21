#!/usr/bin/env bash
# Build and enter a sandboxed Linux dev environment for this project.
# Requires macOS 26 + Apple silicon and the `container` CLI:
#   brew install --cask container   (or download from github.com/apple/container)
#
# The project is mounted read-write, so edits persist on your Mac.
# Your host Claude Code credentials are mounted, so you don't re-authenticate.
# The container itself is disposable (--rm): nothing pollutes the host.
set -euo pipefail

IMAGE="sdd-go-dev"
PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Start the container service (no-op if already running).
container system start >/dev/null 2>&1 || true

# Build the image (uses the Containerfile in this folder).
container build -t "$IMAGE" -f "$PROJECT_DIR/Containerfile" "$PROJECT_DIR"

# Enter the sandbox.
container run -it --rm \
  --name sdd-go-dev \
  --cpus 4 --memory 4g \
  -v "$PROJECT_DIR":/work \
  -v "${HOME}/.claude":/root/.claude \
  -w /work \
  "$IMAGE" \
  bash -lc 'chmod +x .claude/hooks/run-tests.sh 2>/dev/null; go mod tidy 2>/dev/null; exec bash'

# First run only: inside the container, run `claude` once and complete the login
# URL in your Mac browser. The token persists via the mounted ~/.claude, so
# later runs start already authenticated.
#
# Want a PERSISTENT sandbox instead of disposable? Drop --rm and reuse it:
#   container start sdd-go-dev && container exec -it sdd-go-dev bash
