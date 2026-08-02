# AGENTS.md

MCP server that manages one K-Plussa account's K-Ruoka shopping cart. There is no public
API, so it drives a real installed Chrome over CDP and issues each `/kr-api/` call as a
same-origin `fetch()` from inside the loaded page. README.md has the tool surface,
CONTRIBUTING.md the setup.

## Things that will break if you "fix" them

- **The browser profile is a live credential.** It holds a real login (platform data
  dir, or `K_RUOKA_PROFILE`). Never delete it on failure, never commit it. A block
  relaunches against the *same* profile; only `AuthExpired` means re-login.
- **The User-Agent must not contain `HeadlessChrome`.** That single fact is what clears
  Cloudflare. No stealth plugin, nothing else.
- **One Chrome per profile directory.** That is why an interactive login makes `serve`
  hand its browser over rather than starting a second one.
- **K-Ruoka answers `200` with the cart unchanged** for an unknown item id, and inserts a
  placeholder for an unknown EAN. Mutations are therefore verified by *result*
  (`confirm_amount`, `confirm_absent` in `browser/basket.rs`), not by status. Do not
  replace those checks with a status check, and do not cache the basket id: the guards
  read the basket the write itself returned, so a stale id would pass them while
  mutating the wrong cart.
- **`itemId` is not an EAN.** It exists only once an item is in the cart.
- Requests are spaced 500 ms apart process-wide, deliberately, to stay gentle on their
  servers.

## Commands

`just` lists everything. The ones that matter:

| | |
| --- | --- |
| `just test` | hermetic suite: no network, no Chrome. Use this. |
| `just lint` / `just pre-commit` | clippy with `-D warnings` / the full hook set |
| `just coverage` | enforces the line floor CI uses |
| `just probe POST /kr-api/... '{...}'` | re-derive an endpoint against the live site |

Without `just`, the first two are `cargo test --all-targets` and
`cargo clippy --all-targets --all-features -- -D warnings`.

**Do not run `just test-live` or `just test-account` casually.** They hit the real site,
and `test-account` mutates the real cart with the real login. Both are `#[ignore]`d;
`test-account` also needs `K_RUOKA_ACCOUNT_TEST=1`. Never remove an `#[ignore]`.

## Testing without a browser

Almost everything is testable hermetically, and should be:

- `KrApi` (`browser/session.rs`) is the seam. `MockApi` in `tests/support/mod.rs` fakes
  K-Ruoka, records the requests sent, and can be told to return malformed or silent-200
  responses.
- Recovery and readiness policy live in pure functions on purpose: `plan_recovery`,
  `should_replace`, `clearance_step`. Test policy there, not through Chrome.
- `LoginFlow` fakes the login subprocess.

## Environment variables

Read by the server: `K_RUOKA_CHROME` (binary path), `K_RUOKA_PROFILE`,
`K_RUOKA_USER_AGENT` (escape hatch, and the only way to provoke a block on purpose),
`K_RUOKA_MIN_REQUEST_INTERVAL_MS`, `K_RUOKA_IDLE_TIMEOUT_SECS`,
`K_RUOKA_TRACE_SHUTDOWN` (stderr breadcrumbs for startup and shutdown hangs). Read by the tests: `K_RUOKA_ACCOUNT_TEST`,
`K_RUOKA_TEST_STORE`. Docker adds `K_RUOKA_DEBUG_PORT`.

## Conventions

- Comments are sparse and explain the non-obvious **why** only. One short line beats a
  block. No em-dashes anywhere, plain ASCII.
- Distinguish what was **measured** from what was assumed. Do not state a behaviour of
  K-Ruoka's API you have not verified against the live site.
- Actions in workflows are pinned to commit SHAs with the version in a trailing comment.
- A new Python harness under `.github/workflows/` or `tests/` needs an entry in
  `[tool.ruff.lint.per-file-ignores]`, or lint fails on rules the other harnesses waive.
- Checkout stays out of scope. Nothing here may place an order or spend money.
