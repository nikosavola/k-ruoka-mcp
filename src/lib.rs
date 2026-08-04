//! Internals of the `k-ruoka-mcp` binary, which is the thing to use: it speaks MCP over
//! stdio, and README.md documents its tools. Nothing here is a Rust API to program
//! against. These modules are `pub` only so the integration tests can reach them, and
//! they change shape without a major version bump.

pub mod browser;
pub mod login;
pub mod login_flow;
pub mod mcp;
pub mod types;
