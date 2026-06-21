# Dockerfile-compatible — builds with Apple `container build` or Docker.
# A sandboxed Linux dev environment: Go + Node + git + Claude Code.
FROM golang:1.23-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl git bash \
    && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# Claude Code (needs Node 18+)
RUN npm install -g @anthropic-ai/claude-code

WORKDIR /work
CMD ["/bin/bash"]
