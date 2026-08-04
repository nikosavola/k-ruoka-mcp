#!/usr/bin/env python3
"""Live end-to-end test of the MCP tool surface, over real stdio JSON-RPC.

Every empirical claim about cart behaviour is checked here, so it can be re-run when
K-Ruoka changes their frontend instead of being trusted indefinitely.

Hits the live site and mutates a real basket, so it is NOT part of `cargo test`.
Run it via the ignored integration test:

    cargo test --test live_e2e -- --ignored --nocapture

or directly:

    python3 tests/mcp_e2e.py ./target/debug/k-ruoka-mcp

Uses a scratch profile (an anonymous basket), never the real login: no account is
touched, and everything it does is reversible.
"""

import json
import os
import subprocess
import sys

STORE = "N137"  # K-Citymarket Helsinki Ruoholahti
EAN = "2000818700008"  # Pirkka banaani, a few cents
EAN2 = "2000503600002"  # Chiquita banaani
PROFILE = "/tmp/k-ruoka-e2e-profile"

BINARY = sys.argv[1] if len(sys.argv) > 1 else "./target/debug/k-ruoka-mcp"
failures = []


class Server:
    """One `serve` process, spoken to over stdio JSON-RPC."""

    def __init__(self):
        env = dict(os.environ, K_RUOKA_PROFILE=PROFILE)
        self.p = subprocess.Popen(
            [BINARY, "serve"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
            env=env,
        )
        self._id = 0
        init = self.call(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "e2e", "version": "0"},
            },
        )
        self.server_info = init["result"]["serverInfo"]
        self.notify("notifications/initialized")

    def _send(self, msg):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def notify(self, method):
        self._send({"jsonrpc": "2.0", "method": method})

    def call(self, method, params=None):
        self._id += 1
        msg = {"jsonrpc": "2.0", "id": self._id, "method": method}
        if params is not None:
            msg["params"] = params
        self._send(msg)
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise SystemExit("server closed the connection unexpectedly")
            r = json.loads(line)
            if r.get("id") == self._id:
                return r

    def tool(self, name, **args):
        """Returns the structured result, or {"__error__": msg}."""
        r = self.call("tools/call", {"name": name, "arguments": args})
        if "error" in r:
            return {"__error__": r["error"]["message"]}
        res = r["result"]
        if res.get("isError"):
            return {"__error__": res["content"][0]["text"]}
        return res.get("structuredContent") or json.loads(res["content"][0]["text"])

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=30)


def check(label, condition, detail=""):
    status = "ok  " if condition else "FAIL"
    print(f"  [{status}] {label}" + (f" -- {detail}" if detail and not condition else ""))
    if not condition:
        failures.append(label)


def summarise(cart):
    if "__error__" in cart:
        return f"ERROR {cart['__error__'][:90]}"
    items = ", ".join(f"{i['name']}={i['amount']}{i['unit']}" for i in cart["items"])
    return f"{len(cart['items'])} line(s) total={cart['totals']['grandTotal']} [{items or 'empty'}]"


def amounts(cart):
    return {i["itemId"]: i["amount"] for i in cart.get("items", [])}


print(f"binary:  {BINARY}")
print(f"profile: {PROFILE} (anonymous, scratch)\n")

s = Server()

print("1. tool surface and server identity")
# rmcp's default identity is its own crate name; clients display this string.
check(
    "server names itself, not rmcp", s.server_info.get("name") == "k-ruoka-mcp", str(s.server_info)
)
names = sorted(t["name"] for t in s.call("tools/list", {})["result"]["tools"])
expected = sorted([
    "get_cart",
    "add_to_cart",
    "update_cart_item",
    "remove_from_cart",
    "clear_cart",
    "auth_status",
    "search_products",
    "search_stores",
    "get_personal_offers",
    "set_default_store",
    "start_login",
    "login_status",
    "cancel_login",
])
check(f"all {len(expected)} tools are exposed", names == expected, f"got {names}")

print("\n2. get_cart is read-only and reports no account when anonymous")
cart = s.tool("get_cart", store_id=STORE)
print(f"       {summarise(cart)}")
check("get_cart succeeded", "__error__" not in cart)
check("store echoes back", cart.get("store", {}).get("id") == STORE)
# An anonymous session gets a *valid* basket, not a 401 -- so "it worked" must
# never be read as "we are logged in".
check("account is null when anonymous", cart.get("account") is None)

print("\n2b. search_stores finds a store id, and search_products finds an EAN")
stores = s.tool("search_stores", query="Ruoholahti", limit=5)
check("search_stores succeeded", "__error__" not in stores, str(stores)[:200])
if "__error__" not in stores:
    found = stores.get("results", [])
    print(
        f"       {len(found)} store(s), e.g. "
        + ", ".join(f"{r['storeId']}={r['name']}" for r in found[:2])
    )
    check("every store has an id", all(r.get("storeId") for r in found))
    # The store this suite uses must be discoverable, or the tool is not much use.
    check(f"{STORE} is among them", any(r["storeId"] == STORE for r in found), str(found)[:200])

hits = s.tool("search_products", store_id=STORE, query="banaani", limit=5)
check("search_products succeeded", "__error__" not in hits, str(hits)[:200])
if "__error__" not in hits:
    results = hits.get("results", [])
    print(f"       totalHits={hits.get('totalHits')}, showing {len(results)}")
    check("respected the limit", len(results) <= 5, str(len(results)))
    check(
        "every hit has an EAN and a name",
        all(r.get("ean") and r.get("name") for r in results),
        str(results)[:200],
    )
    check(
        "at least one hit is priced",
        any(r.get("price") is not None for r in results),
        str(results)[:200],
    )
    # The point of the tool: an EAN it returns has to be one add_to_cart accepts.
    if results:
        searched_ean = results[0]["ean"]
        added = s.tool("add_to_cart", store_id=STORE, ean=searched_ean, quantity=1)
        check(
            "a searched EAN is addable",
            "__error__" not in added
            and any(i["ean"] == searched_ean for i in added.get("items", [])),
            str(added)[:200],
        )
        s.tool("clear_cart", store_id=STORE)

# Non-ASCII and spaces have to survive being put into the request URL.
odd = s.tool("search_products", store_id=STORE, query="pirkka päärynä", limit=2)
check("a Finnish term with spaces and diacritics works", "__error__" not in odd, str(odd)[:200])

print("\n3. auth_status distinguishes anonymous from signed-in")
st = s.tool("auth_status", store_id=STORE)
check("reports not logged in", st.get("loggedIn") is False)
check("tells the caller to run login", "login" in st.get("detail", ""))

print("\n4. clear_cart, to start from a known state")
cart = s.tool("clear_cart", store_id=STORE)
check("cart is empty", not cart.get("items"), summarise(cart))

print("\n5. add_to_cart")
cart = s.tool("add_to_cart", store_id=STORE, ean=EAN, quantity=2)
print(f"       {summarise(cart)}")
check("one line item", len(cart.get("items", [])) == 1)
check("amount is 2", amounts(cart).get(EAN) == 2)
check("total is now non-zero", cart["totals"]["grandTotal"] > 0)

print("\n6. add_to_cart SETS the quantity, it does not accumulate")
# K-Ruoka's own frontend never sends ADD-ITEM for an item already in the basket
# (it switches to SET-ITEM-AMOUNT), so this is off the path the site exercises.
cart = s.tool("add_to_cart", store_id=STORE, ean=EAN, quantity=2)
check("re-adding 2 leaves 2, not 4", amounts(cart).get(EAN) == 2, summarise(cart))
cart = s.tool("add_to_cart", store_id=STORE, ean=EAN, quantity=3)
check("adding 3 sets it to 3", amounts(cart).get(EAN) == 3, summarise(cart))

print("\n7. update_cart_item")
cart = s.tool("update_cart_item", store_id=STORE, item_id=EAN, quantity=5)
check("quantity becomes 5", amounts(cart).get(EAN) == 5, summarise(cart))

print("\n8. update_cart_item with quantity 0 removes the item")
cart = s.tool("update_cart_item", store_id=STORE, item_id=EAN, quantity=0)
check("item is gone", EAN not in amounts(cart), summarise(cart))

print("\n9. remove_from_cart")
s.tool("add_to_cart", store_id=STORE, ean=EAN, quantity=1)
cart = s.tool("remove_from_cart", store_id=STORE, item_id=EAN)
check("cart is empty again", not cart.get("items"), summarise(cart))

print("\n10. two distinct items, then clear_cart")
s.tool("add_to_cart", store_id=STORE, ean=EAN, quantity=1)
cart = s.tool("add_to_cart", store_id=STORE, ean=EAN2, quantity=1)
print(f"       {summarise(cart)}")
check("two line items", len(cart.get("items", [])) == 2)
cart = s.tool("clear_cart", store_id=STORE)
check("cleared", not cart.get("items") and cart["totals"]["grandTotal"] == 0, summarise(cart))

print("\n11. an unknown itemId fails loudly rather than silently no-opping")
# K-Ruoka answers 200-with-cart-unchanged for a REMOVE-ITEM it does not
# recognise, so without a guard a caller who passed an EAN would see success.
r = s.tool("remove_from_cart", store_id=STORE, item_id="not-a-real-item")
check("returns an error", "__error__" in r, str(r)[:120])
check("names the offending id", "not-a-real-item" in str(r.get("__error__", "")))

print("\n12. inputs K-Ruoka would accept but that mean nothing useful")
# All four of these are cases where K-Ruoka answers 200 and does something
# unhelpful, so a silent success would be the wrong contract.
s.tool("clear_cart", store_id=STORE)
r = s.tool("add_to_cart", store_id=STORE, ean=EAN, quantity=0)
check("add quantity=0 is rejected", "__error__" in r, str(r)[:100])
r = s.tool("add_to_cart", store_id=STORE, ean=EAN, quantity=-5)
check("add quantity=-5 is rejected", "__error__" in r, str(r)[:100])
check("cart untouched by the rejected adds", not s.tool("get_cart", store_id=STORE).get("items"))

# ADD-ITEM accepts any barcode and inserts "Tuntematon tuote"/"Unknown product".
r = s.tool("add_to_cart", store_id=STORE, ean="0000000000000", quantity=1)
check("unknown EAN is rejected", "__error__" in r, str(r)[:100])
check(
    "...and rolled back out of the cart",
    not s.tool("get_cart", store_id=STORE).get("items"),
    summarise(s.tool("get_cart", store_id=STORE)),
)

# The 999 cap is K-Ruoka's, and it is claimed in the tool descriptions, so pin it.
cart = s.tool("add_to_cart", store_id=STORE, ean=EAN, quantity=1e9)
check(
    "absurd quantity is clamped to 999, and the cart shows the truth",
    amounts(cart).get(EAN) == 999,
    summarise(cart),
)
s.tool("clear_cart", store_id=STORE)

s.tool("add_to_cart", store_id=STORE, ean=EAN, quantity=1)
r = s.tool("update_cart_item", store_id=STORE, item_id=EAN, quantity=-3)
check("update quantity=-3 is rejected", "__error__" in r, str(r)[:100])
check("...and the item is still there", amounts(s.tool("get_cart", store_id=STORE)).get(EAN) == 1)
s.tool("clear_cart", store_id=STORE)

r = s.tool("get_cart", store_id="ZZZZ9")
check("bogus store id names the id back", "ZZZZ9" in str(r.get("__error__", "")), str(r)[:120])
r = s.tool("get_cart", store_id="")
# K-Ruoka's own message is "Invalid store ID undefined" for both empty and bogus.
check(
    "empty store id is a clear error",
    "__error__" in r and "undefined" not in r["__error__"],
    str(r)[:120],
)

print("\n13. concurrent tool calls share the one browser safely")
# rmcp dispatches tool calls in parallel, and they all go through a single Chrome.
for i in range(4):
    s._id += 1
    s._send({
        "jsonrpc": "2.0",
        "id": 900 + i,
        "method": "tools/call",
        "params": {"name": "get_cart", "arguments": {"store_id": STORE}},
    })
got = []
for _ in range(4):
    r = json.loads(s.p.stdout.readline())
    got.append("error" not in r and not r["result"].get("isError"))
check("4 parallel get_cart calls all succeeded", all(got), str(got))

print("\n14. the session survives a process restart")
s.tool("add_to_cart", store_id=STORE, ean=EAN, quantity=3)
s.close()
s = Server()
cart = s.tool("get_cart", store_id=STORE)
print(f"       {summarise(cart)}")
check("cart persisted across restart", amounts(cart).get(EAN) == 3)

s.tool("clear_cart", store_id=STORE)
s.close()

print()
if failures:
    print(f"FAILED ({len(failures)}): " + "; ".join(failures))
    sys.exit(1)
print("ALL CHECKS PASSED")
