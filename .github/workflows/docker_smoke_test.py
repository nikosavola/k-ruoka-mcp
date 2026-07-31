#!/usr/bin/env python3
"""Run a built Docker image and hold an MCP handshake with it.

Companion to smoke_test.py, same handshake and idea against a different artefact. The
image launches Chrome lazily, only once a tool call needs it, so `initialize` and
`tools/list` are answered before any browser or network is touched, which is what makes
this safe to run in CI against an image nothing has ever executed before (the arm64
build, in particular).

Usage: python3 .github/workflows/docker_smoke_test.py <image-ref>

<image-ref> is anything `docker run` accepts: a local tag for a pull-request build that
was loaded rather than pushed, or `name@sha256:...` for an image already pulled by
digest.
"""

from __future__ import annotations

import json
import subprocess
import sys

# A floor, not an exact count: an earlier exact-match assertion went stale and blocked a release.
MIN_TOOLS = 11

# Printed by docker-entrypoint.sh when it can't learn the container's own address.
FALLBACK_MESSAGE = "could not determine this container's address"

if len(sys.argv) != 2:
    sys.exit(f"usage: {sys.argv[0]} <image-ref>")
image = sys.argv[1]
print(f"image: {image}")

requests = [
    {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "docker-smoke", "version": "0"},
        },
    },
    {"jsonrpc": "2.0", "method": "notifications/initialized"},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
]
stdin = "".join(json.dumps(r) + "\n" for r in requests)

proc = subprocess.run(
    ["docker", "run", "-i", "--rm", image],
    input=stdin,
    capture_output=True,
    text=True,
    timeout=120,
    # Non-zero exit is a finding for the checks below, not an exception to raise.
    check=False,
)

if proc.stderr.strip():
    print("stderr:", proc.stderr.strip()[:2000])

responses = {}
for raw in proc.stdout.splitlines():
    line = raw.strip()
    if not line:
        continue
    try:
        message = json.loads(line)
    except json.JSONDecodeError:
        sys.exit(f"non-JSON on stdout, which must carry only JSON-RPC: {line[:200]!r}")
    if "id" in message:
        responses[message["id"]] = message

failures = []


def check(label: str, ok: bool, detail: str = "") -> None:
    print(f"  [{'ok  ' if ok else 'FAIL'}] {label}{'' if ok else f' -- {detail}'}")
    if not ok:
        failures.append(label)


init = responses.get(1)
check("answered initialize", init is not None, repr(proc.stdout[:500]))

# The handshake never launches Chrome, so on its own it would pass an image with no
# Chromium at all, or with K_RUOKA_CHROME pointing at a path that does not exist.
chrome = subprocess.run(
    ["docker", "run", "--rm", "--entrypoint", "sh", image, "-c", 'test -x "$K_RUOKA_CHROME"'],
    capture_output=True,
    text=True,
    timeout=60,
    check=False,
)
check("K_RUOKA_CHROME points at an executable", chrome.returncode == 0)
if init:
    result = init.get("result", {})
    name = result.get("serverInfo", {}).get("name")
    # rmcp defaults this to its own crate name, which clients would then display.
    check("introduces itself as k-ruoka-mcp", name == "k-ruoka-mcp", f"got {name!r}")
    check("reports a protocol version", bool(result.get("protocolVersion")), repr(result))

listed = responses.get(2)
check("answered tools/list", listed is not None)
if listed:
    tools = listed.get("result", {}).get("tools", [])
    check(f"exposes at least {MIN_TOOLS} tools (got {len(tools)})", len(tools) >= MIN_TOOLS)

check("entrypoint found its own address", FALLBACK_MESSAGE not in proc.stderr)

check("exited cleanly", proc.returncode == 0, f"returncode {proc.returncode}")

print("\nPASSED" if not failures else f"\nFAILED: {failures}")
sys.exit(0 if not failures else 1)
