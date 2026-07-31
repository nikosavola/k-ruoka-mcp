#!/usr/bin/env python3
"""Account-level end-to-end checks: the claims a scratch profile cannot reach.

The rest of the test suite deliberately never touches a real account, so nothing else
covers these claims. Re-run it after a K-Ruoka change.

    K_RUOKA_ACCOUNT_TEST=1 cargo test --test account_e2e -- --ignored --nocapture

THIS ONE USES THE REAL LOGIN AND BRIEFLY MUTATES THE REAL CART. That is the whole
point -- proving the write path reaches the account's basket and not an anonymous
one cannot be done any other way. It adds one banana and removes it again, and
asserts the cart is back to its exact starting fingerprint. If the removal fails it
says so loudly and names the item to clean up by hand, rather than exiting quietly.

Needs `k-ruoka-mcp login` to have been run first.
"""

import json
import os
import signal
import subprocess
import sys

BINARY = sys.argv[1] if len(sys.argv) > 1 else "./target/debug/k-ruoka-mcp"
STORE = os.environ.get("K_RUOKA_TEST_STORE", "N137")
MARKER_EAN = "2000818700008"  # Pirkka banaani, a few cents

failures = []


class Server:
    """One `serve` process against the real profile, over stdio JSON-RPC.

    Deliberately does NOT set K_RUOKA_PROFILE: unlike every other test here, this
    one is supposed to reach the account.
    """

    def __init__(self):
        self.p = subprocess.Popen(
            [BINARY, "serve"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self._id = 0
        init = self.call(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "account-e2e", "version": "0"},
            },
        )
        self.server_info = init["result"]["serverInfo"]
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _send(self, msg):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def call(self, method, params=None):
        self._id += 1
        self._send({"jsonrpc": "2.0", "id": self._id, "method": method, "params": params or {}})
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise SystemExit("server closed stdout unexpectedly")
            msg = json.loads(line)
            if msg.get("id") == self._id:
                return msg

    def tool(self, name, args):
        res = self.call("tools/call", {"name": name, "arguments": args})["result"]
        if res.get("isError"):
            return {"__error__": res["content"][0]["text"]}
        return res.get("structuredContent") or json.loads(res["content"][0]["text"])

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=60)


def check(label, cond):
    print(f"  [{'ok  ' if cond else 'FAIL'}] {label}")
    if not cond:
        failures.append(label)


def fingerprint(cart):
    """Everything that must be identical before and after a round-trip mutation."""
    return {
        "account": cart.get("account"),
        "basketId": cart.get("basketId"),
        "total": cart["totals"]["grandTotal"],
        "items": sorted((i["itemId"], i["name"], i["amount"], i["unit"]) for i in cart["items"]),
    }


if os.environ.get("K_RUOKA_ACCOUNT_TEST") != "1":
    raise SystemExit(
        "refusing to run: this test uses the REAL login and briefly mutates the REAL\n"
        "cart, so it is not part of `--ignored` by accident. Set K_RUOKA_ACCOUNT_TEST=1."
    )

print(f"store: {STORE}   profile: the real one (no K_RUOKA_PROFILE override)\n")

print("1. auth_status reports the account")
s = Server()
for args, label in (({}, "without store_id"), ({"store_id": STORE}, f"with store {STORE}")):
    st = s.tool("auth_status", args)
    if "__error__" in st:
        check(f"{label}: {st['__error__']}", False)
        continue
    check(f"{label}: loggedIn", st.get("loggedIn") is True)
    check(f"{label}: names an account", bool(st.get("account")))
if failures:
    s.close()
    raise SystemExit("not signed in -- run `k-ruoka-mcp login` first; nothing was mutated")

print("\n2. get_cart reaches the account's cart, not an anonymous one")
before = s.tool("get_cart", {"store_id": STORE})
if "__error__" in before:
    s.close()
    raise SystemExit(f"cannot read the cart: {before['__error__']}")
fp_before = fingerprint(before)
print(f"     {len(before['items'])} item(s), total {fp_before['total']}")
# The discriminator: an anonymous basket has empty strings throughout userInfo, so a
# populated account is the only thing that proves this is not a throwaway basket.
check("account is populated", bool(fp_before["account"]))
check("marker is not already in the cart", all(i["ean"] != MARKER_EAN for i in before["items"]))
if failures:
    s.close()
    raise SystemExit("preconditions not met; refusing to mutate")

print("\n3. the login survives SIGTERM, which is how MCP clients stop a stdio server")
os.kill(s.p.pid, signal.SIGTERM)
rc = s.p.wait(timeout=60)
# A code rather than a signal death: killed by the signal means the graceful browser
# close was skipped, and with it the cookie flush that keeps the login alive.
check(f"clean exit (rc={rc}, negative means killed by the signal)", rc == 0)

s = Server()
after_term = s.tool("get_cart", {"store_id": STORE})
check(
    "still signed in afterwards",
    "__error__" not in after_term and after_term.get("account") == fp_before["account"],
)
check("same basket afterwards", after_term.get("basketId") == fp_before["basketId"])

print("\n4. the write path reaches the account's basket")
added = s.tool("add_to_cart", {"store_id": STORE, "ean": MARKER_EAN, "quantity": 1})
if "__error__" in added:
    check(f"add succeeded ({added['__error__']})", False)
    s.close()
    raise SystemExit("add failed; nothing to roll back")

marker = next((i for i in added["items"] if i["ean"] == MARKER_EAN), None)
check("marker is in the returned cart", marker is not None)
check("exactly one more line than before", len(added["items"]) == len(before["items"]) + 1)
# The load-bearing assertion: an anonymous session would have been handed a
# *different* basket, so an unchanged id is what proves the write hit the account's.
check("same basketId across the write", added.get("basketId") == fp_before["basketId"])
check(
    "pre-existing items untouched",
    {i["itemId"] for i in before["items"]} <= {i["itemId"] for i in added["items"]},
)

print("\n5. and it rolls back to exactly where it started")
if marker is None:
    print("  [FAIL] no marker to remove -- CHECK THE CART BY HAND")
    failures.append("nothing to remove")
else:
    removed = s.tool("remove_from_cart", {"store_id": STORE, "item_id": marker["itemId"]})
    if "__error__" in removed:
        print(f"  [FAIL] REMOVAL FAILED: {removed['__error__']}")
        print(
            f"         *** {marker['name']} (itemId {marker['itemId']}) IS STILL IN "
            f"THE REAL CART -- remove it by hand ***"
        )
        failures.append("rollback failed")
    else:
        fp_after = fingerprint(removed)
        check("identical to the baseline fingerprint", fp_after == fp_before)
        if fp_after != fp_before:
            print(f"     before: {json.dumps(fp_before, ensure_ascii=False)}")
            print(f"     after : {json.dumps(fp_after, ensure_ascii=False)}")

s.close()

print("\nALL CHECKS PASSED" if not failures else f"\nFAILED: {failures}")
raise SystemExit(0 if not failures else 1)
