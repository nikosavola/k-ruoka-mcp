# syntax=docker/dockerfile:1

# Build the release binary with the musl toolchain, so it links against the same libc
# the Alpine runtime stage uses. Only the server binary is built: `spike` and `probe`
# are development tools that hit the live site and have no reason to ship.
FROM rust:1-alpine AS builder
# hadolint ignore=DL3018
RUN apk add --no-cache musl-dev
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
# Keyed only on Cargo.toml/Cargo.lock so this layer survives every source-only commit.
# cargo builds every dependency for the target bin regardless of what a dummy main.rs calls.
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --bin k-ruoka-mcp \
    && rm -rf src
COPY src ./src
# touch forces a fingerprint change even if COPY lands in the same on-disk second as the
# dummy main.rs above.
RUN touch src/main.rs && cargo build --release --bin k-ruoka-mcp

# Runtime: Alpine plus the one thing this server actually needs, a real Chromium to
# drive over CDP. There is no public K-Ruoka API, so this cannot be made lighter.
#
# xvfb-run and xvfb are here for `login` only: it launches a headful (not --headless)
# Chrome, because the point is a human clicking through a real sign-in flow, and a
# container has no display of its own to render that into. `login`'s own xvfb-run
# re-exec (see src/login.rs) needs the wrapper script, which Alpine packages separately
# from the `xvfb` binary itself. socat is docker-entrypoint.sh's relay for the debug
# port -- see the comment there for why a relay, rather than just a wider Chrome bind,
# is what makes it reachable from outside the container.
FROM alpine:3.22
# Versions deliberately unpinned: Alpine's repo only keeps recent package builds, so a
# pinned version here goes stale and breaks the build rather than making it reproducible.
# hadolint ignore=DL3018
RUN apk add --no-cache chromium tini ca-certificates xvfb xvfb-run socat \
    && adduser -D -u 1000 -h /home/k-ruoka k-ruoka \
    # Pre-created and owned by the runtime user so that mounting a volume here (for a
    # persistent login) inherits this ownership on first use rather than the root
    # ownership Docker would otherwise give a fresh mount point.
    && install -d -o k-ruoka -g k-ruoka /home/k-ruoka/.local/share/k-ruoka-mcp
COPY --from=builder /build/target/release/k-ruoka-mcp /usr/local/bin/k-ruoka-mcp
COPY --chmod=755 docker/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

USER 1000
WORKDIR /home/k-ruoka
# Pinned explicitly rather than left to the runtime candidate-path search, since this is
# the one path Alpine's package actually installs.
ENV K_RUOKA_CHROME=/usr/bin/chromium
# Which port the entrypoint's relay watches for a login browser. Publish this port to the
# Docker host's loopback to reach a login started by the start_login tool.
ENV K_RUOKA_DEBUG_PORT=9222

# tini as PID 1: Chromium is a multi-process app, and something has to reap the zombies
# its child processes leave behind since the container has no init of its own.
ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/docker-entrypoint.sh"]
CMD ["serve"]
