# Contributing

## Development Setup

```bash
just install
```

That installs the git hooks and fetches dependencies. `just` on its own lists every
recipe, grouped.

## Running tests

```bash
just test        # hermetic: no network, no Chrome, ~2s
just test-live   # against the real site, scratch profile, anonymous basket
just coverage    # summary, with a floor so it fails on regression
```

`just test-account` also exists. It uses a real K-Plussa login and briefly adds and
removes one item in a real cart, so it asks for confirmation and is gated behind
`K_RUOKA_ACCOUNT_TEST=1`. You do not need it to contribute.

## Running linting & formatting (pre-commit)

```bash
just pre-commit
```

## Things worth knowing before you change anything

Several things that look like bugs are deliberate, and the comments say why. Three that
catch people out:

1. **"It returned 200" proves nothing.** This API answers `200` for adding a quantity of
   zero, removing an item that does not exist, and adding an EAN it has never heard of.
   Check the *result*, not the status. Every guard in `basket.rs` exists because of that.
2. **The Cloudflare recovery path has rotted three times.** Each time it looked correct
   and each time the recovery silently did not happen. If you refactor `Session::api`,
   keep its three safeguards: the tuple return so a stray `?` cannot skip the match, the
   `Option<u64>` generation so "no browser" cannot be mistaken for one, and the launch
   log so a no-op relaunch is visible.
3. **Never delete the browser profile.** It holds the user's login.

## Tests

New tests belong in one of four layers: unit, protocol (against the fake), live
anonymous, live account. Two habits worth keeping:

- **Assert on the request sent, not just the result returned.** A bug that set the wrong
  unit produced a perfectly plausible cart and a wrong request.
- **Check that a new test can fail.** Reintroduce the bug and watch it catch it. Every
  guard here was verified that way.

## Scope

Checkout is deliberately out of scope and will stay that way. Nothing in this project
should be able to place an order or spend money.

Product search is also out of scope. This server takes an EAN.

## AI Usage Policy

The use of AI tools to accelerate your development workflow, whether for prototyping,
writing tests, or improving documentation, is **encouraged**.

However, as a contributor, you remain **fully responsible** for the code and content you
submit. Please ensure the following:

1. **No "AI Slop"**: Do not submit unreviewed, low-quality, or redundant AI-generated
   content.
1. **Verify & Test**: All AI-generated code must be reviewed, tested, and verified to work
   as intended.
1. **Maintainability**: The content must be clear, idiomatic, and maintainable by a human.

One project-specific addition: this codebase distinguishes what was **measured** from what
was assumed, and that distinction is load-bearing. Do not state a behaviour of K-Ruoka's
API that you have not verified against the live site, and do not let a model infer one
that nobody checked.
