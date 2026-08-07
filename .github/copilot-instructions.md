# Copilot code review instructions

k-ruoka-mcp is an MCP server that manages one K-Plussa account's K-Ruoka shopping cart.
There is no public API, so it drives a real installed Chrome over CDP and issues each
`/kr-api/` call as a same-origin `fetch()` from inside the loaded page. See `AGENTS.md`
for the full picture; this file is the subset relevant to reviewing a diff.

## Do not flag these as bugs

- **No status-code check on mutations.** K-Ruoka answers `200` with the cart unchanged
  for an unknown item id, and inserts a placeholder for an unknown EAN. Mutations are
  verified by *result* (`confirm_amount`, `confirm_absent` in `browser/basket.rs`), not
  by HTTP status. A PR that adds a status check in place of these guards is removing
  correctness, not adding it.
- **The basket id is re-read on every write**, never cached. That is deliberate: the
  guards read the basket the write itself returned, so a stale cached id would pass the
  guard while mutating the wrong cart.
- **`itemId` used interchangeably with EAN would be a real bug**, not a style nit: they
  are different identifiers. `itemId` exists only once an item is in the cart;
  `update_cart_item` / `remove_from_cart` take the cart's `item_id`; `add_to_cart` takes an EAN.
- **The browser profile directory is never deleted on failure**, including in recovery
  and retry paths. It holds a real login. A block should relaunch against the same profile;
  only an `AuthExpired` result should trigger re-login.
- **The User-Agent never contains `HeadlessChrome`.** Do not suggest restoring a
  default/stealth user agent string; that substring alone is what triggers Cloudflare's
  block, per `K_RUOKA_USER_AGENT`'s doc comment.
- **Requests are throttled to one per 500 ms, process-wide, on purpose** (being gentle
  on K-Ruoka's servers). Don't suggest parallelizing or removing this for "performance."

## Testing

- `KrApi` (`browser/session.rs`) is the seam between the server and K-Ruoka. New logic
  should be reachable through `MockApi` (`tests/support/mod.rs`) rather than requiring a
  real browser; flag a PR that adds Chrome-dependent logic with no hermetic test path.
- Recovery and readiness policy belongs in pure functions (`plan_recovery`,
  `should_replace`, `clearance_step`) precisely so it can be tested without Chrome. A PR
  that inlines new policy decisions into the Chrome-driving code instead of one of these
  functions is harder to test and should be flagged.
- Tests gated `#[ignore]` (`test-live`, `test-account`) hit the real site or mutate the
  real cart. Do not suggest removing an `#[ignore]`, and treat a PR that does as a
  request needing explicit sign-off, not a routine change.

## Conventions

- Comments should explain the non-obvious *why* only, one short line where possible. Flag
  comments that restate what the code already says.
- Distinguish measured behaviour from assumed behaviour. A comment or doc-comment
  asserting how K-Ruoka's API behaves should read as verified, not guessed.
- GitHub Actions in `.github/workflows/` are pinned to commit SHAs with the version in a
  trailing comment (e.g. `uses: foo/bar@<sha> # v1.2.3`). Flag a floating tag or branch
  ref, and check that a pinned SHA is a real commit SHA, not a tag object's SHA.
- Checkout stays out of scope: nothing in this repository may place an order or spend
  money. Flag any change that adds order placement, payment, or checkout capability.

## Not this repo's job

- Do not ask for stealth/anti-detection additions to the browser automation beyond the
  User-Agent fix above; the project's stance is to look like an ordinary browser, not to
  evade detection.
- Do not suggest telemetry, analytics, or usage tracking additions.
