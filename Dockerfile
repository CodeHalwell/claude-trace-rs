# Multi-stage build → small runtime image.
FROM rust:1-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim
LABEL org.opencontainers.image.source="https://github.com/CodeHalwell/claude-trace-rs" \
      org.opencontainers.image.description="Local-first observability dashboard + trace database for terminal coding agents (Claude Code, Codex, Copilot, Kimi, Cline, Cursor)" \
      org.opencontainers.image.licenses="MIT"

COPY --from=builder /app/target/release/claude-trace-rs /usr/local/bin/claude-trace-rs

# Watch + database live under /data; mount your host's agent logs there.
ENV CLAUDE_TRACE_WATCH_ROOT=/data/projects \
    CLAUDE_TRACE_DB=/data/trace.db
VOLUME ["/data"]
EXPOSE 7779

# NOTE: the server binds 127.0.0.1 by design. To reach it from the host, run
# with host networking and mount your logs (one --watch-root per agent), e.g.:
#   docker run --rm --network host \
#     -v "$HOME/.claude/projects:/data/claude" \
#     -v "$HOME/.codex/sessions:/data/codex" \
#     -v claude-trace-data:/data \
#     ghcr.io/codehalwell/claude-trace-rs:latest \
#     serve --watch-root /data/claude --watch-root /data/codex
ENTRYPOINT ["claude-trace-rs"]
CMD ["serve"]
