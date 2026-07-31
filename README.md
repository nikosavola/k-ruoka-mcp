# k-ruoka-mcp

[![en](https://img.shields.io/badge/lang-en-red.svg)](./README.md)
[![fi](https://img.shields.io/badge/lang-fi-blue.svg)](./README.fi.md)
[![sv](https://img.shields.io/badge/lang-sv-yellow.svg)](./README.sv.md)

An MCP server that manages the shopping cart of one [K-Ruoka](https://www.k-ruoka.fi)
(Finnish grocery) account: read the cart, add items, change quantities, remove items,
clear it.

Checkout is deliberately out of scope. Nothing here can place an order or spend money.

> [!IMPORTANT]
> Use with caution, and only against your own account. K-Ruoka's terms limit the service
> to a customer's *own personal private use*, and Kesko may restrict or close an account
> at its discretion; the risk you take on is your own. Read the
> [terms of service notes](#terms-of-service--use-with-caution) first.

## How it works

K-Ruoka has no public API. The cart lives behind `/kr-api/basket/...`, which is
private, undocumented, and authenticated purely by browser cookies. There is no
bearer token or API key to hold. So this server drives a real, installed Chrome via
the DevTools Protocol ([chromiumoxide](https://github.com/mattsse/chromiumoxide)),
keeps a persistent profile on disk, and makes each API call as a same-origin
`fetch()` from inside the loaded page. The browser attaches the cookies itself.

The site sits behind Cloudflare. Getting through needs exactly one thing: a
User-Agent that doesn't contain the token `HeadlessChrome`. No stealth plugin, no
challenge-solving, no interstitial to wait out. That single fact is why this is pure Rust
with no browser-automation sidecar.

## Requirements

- **Google Chrome** at `/usr/bin/google-chrome` (override with `K_RUOKA_CHROME`).
  Not optional and not bundled: the whole design is "drive a real browser", because
  the cookies are the only credential the private API accepts.
- `xvfb-run`, only for `login` on a machine with no display
- Rust (built with 1.94), only to build from source

## Install

Published to PyPI as a prebuilt binary wheel, so `uvx` fetches and runs it with no
Rust toolchain and nothing to download by hand:

```sh
uvx k-ruoka-mcp login    # once, by hand
```

PyPI is used purely as a distribution channel. There is no Python API and no Python in
the wheel. [maturin](https://github.com/PyO3/maturin)'s `bin` bindings put the
compiled Rust executable straight into the environment's `bin/`, so there is no Python
startup cost on the hot path.

<details>
<summary>Building from source instead</summary>

```sh
cargo build --release      # ./target/release/k-ruoka-mcp
```

Or build the wheel the way CI does:

```sh
uvx maturin build --release
```

Note that a locally built wheel is tagged with *your* glibc, so it may not install
elsewhere. The release workflow builds inside a manylinux container for that reason.

</details>

## Setup

### 1. Log in (once, by hand)

```sh
uvx k-ruoka-mcp login
```

Credentials and MFA are never automated. This only opens a browser and waits for
K-Ruoka to start reporting a signed-in account.

On a headless machine it re-execs itself under `xvfb-run` and prints the exact
commands to drive that browser from your laptop: an `ssh -L` tunnel to Chrome's
debug port (`--port`, default 9222), then `chrome://inspect`, which gives you a live
clickable view of the page. Click *Kirjaudu* and sign in normally. It exits once it
sees the account.

Two k-ruoka.fi tabs appear in `chrome://inspect`. Use the one titled *Tuotteet |
K-Ruoka Verkkokauppa*; the one labelled `[k-ruoka-mcp] poller` is this process
checking whether you're signed in yet, and it gets navigated out from under you.
The printed instructions say this too.

The session is stored in `~/.local/share/k-ruoka-mcp/profile` (mode `0700`,
override with `K_RUOKA_PROFILE`). **It holds a live login, so treat it as a
credential.** It is not in the repo and must not be committed.

### 2. Register the server

```json
{
  "mcpServers": {
    "k-ruoka-cart": {
      "command": "uvx",
      "args": ["k-ruoka-mcp"]
    }
  }
}
```

No subcommand is needed: `serve` is the default, since being an MCP server is the
whole job. `uvx k-ruoka-mcp serve` is equivalent if you prefer it explicit.

<details>
<summary>Using a locally built binary</summary>

```json
{
  "mcpServers": {
    "k-ruoka-cart": {
      "command": "/path/to/target/release/k-ruoka-mcp",
      "args": ["serve"]
    }
  }
}
```

</details>

Chrome launches lazily on the first tool call, so `serve` starts instantly.

On shutdown, whether the client closes stdin or sends `SIGTERM` (both normal), the
browser is closed gracefully so Chrome flushes cookies back into the
profile. Skipping that is how a login silently fails to survive a client restart.

## Tools

Every tool takes a `store_id`, because a cart belongs to a store (e.g. `N137` is
K-Citymarket Helsinki Ruoholahti).

| tool | notes |
|---|---|
| `get_cart(store_id)` | Read-only. The only source of `itemId` values. |
| `add_to_cart(store_id, ean, quantity?, unit?, local_store_id?, allow_substitutes?)` | By EAN. `quantity` is the resulting amount, not an increment. Defaults to 1, `unit` to `kpl`. |
| `update_cart_item(store_id, item_id, quantity, unit?)` | Sets an exact quantity. 0 removes. `unit` defaults to the item's existing one. |
| `remove_from_cart(store_id, item_id)` | |
| `clear_cart(store_id)` | Empties the cart. Not undoable. |
| `auth_status(store_id)` | Whether the stored session is still signed in. |

Two things worth knowing when calling these:

- **`item_id` is not an EAN.** It's the basket's own id for an item and only exists
  once the item is in the cart, so `update_cart_item` and `remove_from_cart` need a
  `get_cart` first. Both validate it and tell you the valid ids if you get it wrong,
  because K-Ruoka itself answers `200` with the cart unchanged for an unknown id, a
  silent no-op that looks like success.
- **This server cannot search for products.** It takes an EAN barcode; finding one is
  someone else's job.
- **`add_to_cart` sets a quantity, it doesn't add to one.** Calling it twice with
  `quantity: 1` leaves 1 in the cart, not 2. K-Ruoka's `ADD-ITEM` replaces the
  amount for an EAN that's already present. Measured, not assumed; the website
  itself never sends that request.
- **`update_cart_item`'s `unit` defaults to whatever the item already uses**, not to
  `kpl`. Passing the wrong one converts the item: 2 kg silently becomes 2 pieces.

Arguments K-Ruoka would accept but that don't mean anything useful are rejected up
front rather than reported as success: a non-positive `quantity` on `add_to_cart` (it
would add nothing and return `200`), a negative one on `update_cart_item` (it would
remove the item, duplicating `0`), and an EAN K-Ruoka has no record of (it would
insert a placeholder item named *Unknown product*, which is rolled back).

### Rate limiting

Requests to `/kr-api/` are spaced at least **500 ms** apart, process-wide.

This is not about throughput. The tool makes a handful of calls when you ask for
something and none the rest of the time, so a ceiling would never bind. It is about
*shape*. MCP clients dispatch tool calls concurrently, and a model working through a
shopping list can issue them in a tight loop; without spacing, that arrives as a burst
which looks nothing like a person using the website. 500 ms is slower than a human
clicking, deliberately.

Concurrent callers queue rather than firing together, and the first request is never
delayed, so an interactive cart read still feels immediate.

```sh
K_RUOKA_MIN_REQUEST_INTERVAL_MS=1000   # gentler
K_RUOKA_MIN_REQUEST_INTERVAL_MS=0      # off
```

### How failures are reported

Everything this server can go wrong with, an expired session, an unknown item id,
a quantity that means nothing, comes back as an ordinary tool result with
`isError: true`, carrying a message meant to be read and acted on. MCP reserves
JSON-RPC protocol errors for the client's problems (unknown tool, arguments that
violate the schema), and a client may reasonably treat one of those as a transport
failure, in which case the model never sees the text. Since the text is the point
("run `login`", "the item ids currently in the cart are …"), it goes on the channel
that reaches the model.

### Not signed in is not an error

An anonymous session gets a perfectly valid, empty cart from K-Ruoka rather than a
`401`. So a cart operation that "works" is not evidence you're logged in, it may
have quietly operated on a throwaway cart. `get_cart` reports `account: null` in
that case, and `auth_status` says so plainly. If results look wrong, check there
first.

## Session lifecycle

Failures are classified rather than retried blindly, because one of them must never
touch the profile:

| condition | response |
|---|---|
| Cloudflare block, `cf-mitigated`, a challenge page, or the shop page itself being refused | Relaunch against the **same** profile, once. Never deleted. |
| `401` / `"Token renewal error - reload"` | Session expired. No retry, profile untouched, told to re-run `login`. |
| `409` `"Client version is too old - reload"` | Re-read the build number from that same response and retry once. |
| Chrome won't launch against the dir at all | Reports that the profile may be corrupt and that deleting it means logging in again. It never deletes anything itself, the dir holds a credential, so that's your call. |

The two `"- reload"` messages look almost identical and mean opposite things, which
is why they're matched explicitly.

Both retries log a line to stderr when they fire (stdout belongs to JSON-RPC), so a
retry is never invisible.

Two things measured rather than assumed:

- The `409` is really a *cold-start* condition, not a deploy one. `X-K-Build-Number`
  has to be present and numeric, but K-Ruoka never compares the value, `1` and
  `99999999` are both accepted. It fires on a process's first call, before the
  header has been learned.
- Losing `cf_clearance` does **not** trigger the Cloudflare branch; requests
  succeed without it. What does trigger it is the browser fingerprint being
  refused outright.

## Development

[`just`](https://github.com/casey/just) has the commands; `just` on its own lists them
grouped by what they do.

```sh
just install      # git hooks (see .pre-commit-config.yaml) and dependencies
just test         # hermetic, ~2s
just test-live    # against the real site, scratch profile, anonymous basket
just pre-commit   # every hook over every file
```

The two recipes that can touch real state say so: `just login` writes a live K-Plussa
session into the profile, and `just test-account` briefly adds and removes one item in
the real cart, so it asks before running.

Four layers, deliberately:

| | what it covers | needs |
|---|---|---|
| **unit** (37, in `src/`) | error classification, the retry *policy* and the relaunch *decision*, event wire format, parsing, JS escaping | nothing |
| **protocol** (28, `tests/mcp_protocol.rs`) | the whole tool surface over a real in-process MCP connection, against a fake K-Ruoka, including its habit of answering `200` while changing nothing | nothing |
| **shutdown** (3, `tests/shutdown.rs`) | that a signalled `serve` exits cleanly instead of being killed | nothing |
| **live, anonymous** (31, `tests/live_e2e.rs`) | the same surface against the real site | network + Chrome |
| **live, account** (14, `tests/account_e2e.rs`) | that the basket reached is the account's, and that writes land in it | network + Chrome + a real login |

The protocol tests run the real `CartServer` over `tokio::io::duplex`, so requests
are serialised to JSON-RPC, framed, routed by rmcp and deserialised into the argument
structs exactly as for a real client, the schemas and error mapping are exercised,
not bypassed. Only the browser is faked, at the `KrApi` seam. That buys determinism,
millisecond runtime, and coverage of states an anonymous session can never reach: a
signed-in account, an expired session, a Cloudflare block.

Where they assert on the *request sent* rather than the cart returned, that is
deliberate, the `update_cart_item` unit bug produced a perfectly plausible cart and
a wrong request.

The live suite is where the empirical claims are pinned, so re-run it
when K-Ruoka changes their frontend rather than trusting the notes indefinitely:

```sh
# Hits the network and mutates a basket, so it is excluded from `cargo test`.
cargo test --test live_e2e -- --ignored --nocapture
```

Two binaries exist for working against the live site, both defaulting to a scratch
profile rather than your real login:

```sh
cargo run --bin spike     # re-runs the baseline browser and Cloudflare checks
cargo run --bin probe -- POST /kr-api/basket/active '{"storeId":"N137"}'

# probe flags for driving the recovery paths deliberately:
cargo run --bin probe -- --drop-clearance POST /kr-api/basket/active '{"storeId":"N137"}'
cargo run --bin probe -- --build=1        POST /kr-api/basket/active '{"storeId":"N137"}'
```

Env vars: `K_RUOKA_PROFILE` (profile dir), `K_RUOKA_CHROME` (Chrome path),
`K_RUOKA_USER_AGENT` (override the derived UA, an escape hatch if Chrome changes
its version format, and the way to provoke a Cloudflare block on purpose).
`K_RUOKA_MIN_REQUEST_INTERVAL_MS` sets the minimum gap between `/kr-api/` requests
(default 500; `0` disables the limit).

`probe` is the tool for re-deriving the API contract when K-Ruoka changes their
frontend. It goes through the same `Session` the
server uses, so what it sees is what the server gets.

## Caveats

- The API contract is empirically derived from K-Ruoka's production JavaScript, not
  documented. It can break at any deploy; `probe` is how you re-derive it.
- K-Plussa session lifetime is unknown; expect to re-run `login` occasionally.
- This reaches a private API through your own session. Read the next section, and use it
  only for your own personal cart.

## Terms of service, use with caution

K-Ruoka has no public API, so this reaches a private one through your own signed-in
browser session. Treat that as something to be careful with rather than something
settled.

K-Ruoka's
[sopimusehdot](https://www.k-ruoka.fi/artikkelit/kayttoehdot/k-ruoka-fi-palvelun-sopimusehdot)
(15.6.2026) limit use of the service's material to

> Asiakkaan omaan henkilökohtaiseen yksityiseen käyttöön

*The customer's own personal private use* (unofficial translation). It also makes the
account holder responsible for everything done under their credentials. Kesko may
restrict service use or close an account at its own discretion.

This tool is built to stay inside that: **one account, your own, and nothing but your own
cart.**

- No other user's data is read, and nothing is scraped or collected in bulk.
- **No checkout.** Nothing here can place an order or spend money.
- Requests are [rate limited](#rate-limiting) and the volume is far below ordinary human
  browsing, a handful of calls when you ask for something, then nothing.
- Nothing is redistributed or resold.

**Read the current terms yourself and decide.** They can change, the date above is when
that document was last revised at the time of writing, and how they apply to a
human-directed assistant acting on your own account is your call to make as the account
holder, you are the one carrying the risk, which is your K-Plussa account. Consider
whether the website simply does the job. None of this is legal advice.

## Trademarks and affiliation

Not affiliated with, endorsed by, or connected to Kesko Oyj in any way. *K-Ruoka*,
*K-Plussa*, *K-Citymarket*, *Pirkka* and *Kesko* are trademarks of Kesko Oyj, used here
only to describe what this software interoperates with.

No K-Ruoka content is redistributed. The notes quote short fragments of API responses
and of the site's public JavaScript where that is the only way to document the wire
format an interoperating client has to match.

Provided without warranty of any kind, see [LICENSE](LICENSE).

## Acknowledgements

- [mcp-ruoka](https://github.com/p18a/mcp-ruoka), an MCP server for searching Finnish
  grocery catalogues (K-Ruoka, S-kaupat, Alko). It pairs naturally with this one, which
  deliberately does no searching: get an EAN there, pass it here. Store ids are the same
  id space. It was also the starting point for the browser-automation approach here.
- [chromiumoxide](https://github.com/mattsse/chromiumoxide), the CDP client that drives
  Chrome.
- [rmcp](https://github.com/modelcontextprotocol/rust-sdk), the official Rust MCP SDK.
- [maturin](https://github.com/PyO3/maturin), packages the binary as a wheel, which is
  what makes `uvx k-ruoka-mcp` work.
