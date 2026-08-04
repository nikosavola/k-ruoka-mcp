#!/usr/bin/env python3
"""Install the built wheel with uvx and hold an MCP handshake with it.

The point is the *bare* invocation. What users put in their client config is

    {"command": "uvx", "args": ["k-ruoka-mcp"]}

with no subcommand, so `serve` has to be the default. That is a one-line default in
`main.rs` and nothing else in the test suite covers it -- the other suites all invoke
the binary as `k-ruoka-mcp serve`. Regressing it would ship a package that fails for
every user with a clap usage error on a stream the client does not display.

No network beyond the install and no Chrome: `initialize` and `tools/list` are
answered before the browser is ever launched (it is created lazily on the first tool
call). That is what makes this runnable in CI.

Usage: python3 .github/workflows/smoke_test.py [wheel-dir]
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

# Sorted, and the count comes from this list rather than the message: hard-coding "six"
# is how this went stale, blocking a release over a wheel that was fine.
EXPECTED_TOOLS = sorted([
    "add_to_cart",
    "auth_status",
    "cancel_login",
    "clear_cart",
    "get_cart",
    "get_personal_offers",
    "login_status",
    "remove_from_cart",
    "search_products",
    "search_stores",
    "set_default_store",
    "start_login",
    "update_cart_item",
])

wheel_dir = Path(sys.argv[1] if len(sys.argv) > 1 else "dist")
wheels = sorted(wheel_dir.glob("*.whl"))
if not wheels:
    sys.exit(f"no wheel in {str(wheel_dir)!r}; the build job should have produced one")
wheel = wheels[-1]
print(f"wheel: {wheel}")

requests = [
    {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "smoke", "version": "0"},
        },
    },
    {"jsonrpc": "2.0", "method": "notifications/initialized"},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
]
stdin = "".join(json.dumps(r) + "\n" for r in requests)

# `--from <wheel>` pins what is installed; the trailing name is the command to run,
# with no subcommand after it. That absence is the whole test.
proc = subprocess.run(
    ["uvx", "--from", str(wheel), "k-ruoka-mcp"],
    input=stdin,
    capture_output=True,
    text=True,
    timeout=300,
    # A non-zero exit is a finding to report, not an exception to raise: the checks
    # below say *which* expectation failed, which a traceback would not.
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
check("answered initialize with no subcommand given", init is not None, repr(proc.stdout[:500]))
if init:
    result = init.get("result", {})
    name = result.get("serverInfo", {}).get("name")
    # rmcp defaults this to its own crate name, which clients would then display.
    check("introduces itself as k-ruoka-mcp", name == "k-ruoka-mcp", f"got {name!r}")
    check("reports a protocol version", bool(result.get("protocolVersion")), repr(result))

listed = responses.get(2)
check("answered tools/list", listed is not None)
if listed:
    names = sorted(t["name"] for t in listed.get("result", {}).get("tools", []))
    check(
        f"exposes all {len(EXPECTED_TOOLS)} tools (got {len(names)})",
        names == EXPECTED_TOOLS,
        f"got {names}",
    )

# A stdio server ends when stdin closes; a non-zero exit would mean it crashed.
check("exited cleanly", proc.returncode == 0, f"returncode {proc.returncode}")

print("\nPASSED" if not failures else f"\nFAILED: {failures}")
sys.exit(0 if not failures else 1)
