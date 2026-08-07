//! Internals of the `k-ruoka-mcp` binary. If you landed here looking for a library to
//! depend on: there isn't one. Nothing in this crate is a Rust API to program against --
//! these modules are `pub` only so the integration tests can reach them, and they change
//! shape without a major version bump.
//!
//! What you actually want is the binary, an MCP server that manages one K-Plussa
//! account's [K-Ruoka](https://www.k-ruoka.fi) shopping cart: read the cart, add items,
//! change quantities, remove items, clear it. It drives a real, installed Chrome over
//! the DevTools Protocol, because K-Ruoka has no public API and the cart lives behind a
//! private one authenticated purely by browser cookies.
//!
//! # Install and run it
//!
//! Published to PyPI as a prebuilt binary wheel, so `uvx` fetches and runs it with no
//! Rust toolchain:
//!
//! ```sh
//! uvx k-ruoka-mcp login    # once, by hand, to sign in
//! ```
//!
//! `cargo install k-ruoka-mcp`, `cargo binstall k-ruoka-mcp` and a Docker image at
//! `ghcr.io/nikosavola/k-ruoka-mcp` all work too.
//!
//! # Register it with an MCP client
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "k-ruoka-cart": {
//!       "command": "uvx",
//!       "args": ["k-ruoka-mcp"]
//!     }
//!   }
//! }
//! ```
//!
//! `serve` is the default subcommand, and Chrome only starts on the first tool call, so
//! registering it costs nothing until it's actually used.
//!
//! # Tools
//!
//! Every cart tool takes a `store_id` -- find one with `search_stores`, or call
//! `set_default_store` once and omit it afterwards.
//!
//! - `search_stores`, `search_products` -- read-only lookups. Search in Finnish; the
//!   catalogue is.
//! - `get_cart`, `add_to_cart`, `update_cart_item`, `remove_from_cart`, `clear_cart` --
//!   the cart itself. `add_to_cart` takes an EAN and *sets* the quantity, it does not
//!   add to it.
//! - `get_personal_offers` -- the account's OmaPlussa-edut offers, read-only.
//! - `auth_status`, `start_login`, `login_status`, `cancel_login` -- signing in through
//!   the assistant instead of a terminal.
//!
//! [README.md](https://github.com/nikosavola/k-ruoka-mcp#readme) has the full tool
//! reference (argument-by-argument notes, error handling, rate limiting) and the terms
//! of service this is built to stay inside: **one account, your own, and nothing but
//! your own cart. No checkout -- nothing here can place an order or spend money.**
//!
//! # Contributing
//!
//! [CONTRIBUTING.md](https://github.com/nikosavola/k-ruoka-mcp/blob/main/CONTRIBUTING.md)
//! covers the development setup; the module docs below are for that audience, not for
//! programming against this crate as a dependency.

pub mod browser;
pub mod login;
pub mod login_flow;
pub mod mcp;
pub mod types;
