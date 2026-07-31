//! Entry point for the account-level end-to-end test.
//!
//! Doubly gated, and the second gate is the important one. `#[ignore]` keeps it out
//! of `cargo test`, but `--ignored` is a bucket the *anonymous* live suite already
//! lives in, and someone reaching for that has no reason to expect their real
//! shopping cart to be touched. So this also requires `K_RUOKA_ACCOUNT_TEST=1` and
//! skips with a loud message otherwise.
//!
//!     K_RUOKA_ACCOUNT_TEST=1 cargo test --test account_e2e -- --ignored --nocapture
//!
//! Needs `k-ruoka-mcp login` to have been run first. The assertions live in
//! `tests/account_e2e.py`, alongside the reasons each one is the discriminating check.

use std::process::Command;

#[test]
#[ignore = "uses the real login and briefly mutates the real cart; needs K_RUOKA_ACCOUNT_TEST=1"]
fn account_surface_end_to_end() {
    if std::env::var("K_RUOKA_ACCOUNT_TEST").as_deref() != Ok("1") {
        // Skipping rather than failing: a bare `--ignored` run is a reasonable thing
        // to do and should not look broken. Saying so is what keeps the skip honest --
        // a silent pass here would read as "the account path is covered".
        eprintln!(
            "SKIPPED: this test uses the real login and briefly mutates the real cart. \
             Set K_RUOKA_ACCOUNT_TEST=1 to run it."
        );
        return;
    }

    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/account_e2e.py");
    let status = Command::new("python3")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_k-ruoka-mcp"))
        .status()
        .expect("running python3; it is required for this test");
    assert!(
        status.success(),
        "account end-to-end checks failed (see output above). If a rollback failed, \
         the output names the item to remove by hand."
    );
}
