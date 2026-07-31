//! Entry point for the live end-to-end test.
//!
//! Ignored by default: it drives a real Chrome against k-ruoka.fi and mutates a
//! real (anonymous, scratch-profile) basket, so it must not run in a plain
//! `cargo test`. The assertions live in `tests/mcp_e2e.py`, which speaks the MCP
//! stdio protocol properly rather than calling Rust functions directly -- what it
//! exercises is what a client actually gets.
//!
//!     cargo test --test live_e2e -- --ignored --nocapture

use std::process::Command;

#[test]
#[ignore = "hits the live k-ruoka.fi site and needs Chrome"]
fn mcp_tool_surface_end_to_end() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mcp_e2e.py");
    let status = Command::new("python3")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_k-ruoka-mcp"))
        .status()
        .expect("running python3; it is required for this test");
    assert!(
        status.success(),
        "live end-to-end checks failed (see output above)"
    );
}
