# syntax=docker/dockerfile:1

# Build the release binary with the musl toolchain, so it links against the same libc
# the Alpine runtime stage uses. Only the server binary is built: `spike` and `probe`
# are development tools that hit the live site and have no reason to ship.
FROM rust:1-alpine AS builder
# hadolint ignore=DL3018
RUN apk add --no-cache musl-dev
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --bin k-ruoka-mcp

# Runtime: Alpine plus the one thing this server actually needs, a real Chromium to
# drive over CDP. There is no public K-Ruoka API, so this cannot be made lighter.
FROM alpine:3.22
# Versions deliberately unpinned: Alpine's repo only keeps recent package builds, so a
# pinned version here goes stale and breaks the build rather than making it reproducible.
# hadolint ignore=DL3018
RUN apk add --no-cache chromium tini ca-certificates \
    && adduser -D -u 1000 -h /home/k-ruoka k-ruoka \
    # Pre-created and owned by the runtime user so that mounting a volume here (for a
    # persistent login) inherits this ownership on first use rather than the root
    # ownership Docker would otherwise give a fresh mount point.
    && install -d -o k-ruoka -g k-ruoka /home/k-ruoka/.local/share/k-ruoka-mcp
COPY --from=builder /build/target/release/k-ruoka-mcp /usr/local/bin/k-ruoka-mcp

USER 1000
WORKDIR /home/k-ruoka
# Pinned explicitly rather than left to the runtime candidate-path search, since this is
# the one path Alpine's package actually installs.
ENV K_RUOKA_CHROME=/usr/bin/chromium

# tini as PID 1: Chromium is a multi-process app, and something has to reap the zombies
# its child processes leave behind since the container has no init of its own.
ENTRYPOINT ["/sbin/tini", "--", "k-ruoka-mcp"]
CMD ["serve"]
