//! Baseline live checks, kept as a regression check.
//!
//! Originally proved the two questions that decided the architecture:
//!
//!   2. chromiumoxide can drive the system's installed Chrome against a persistent
//!      `--user-data-dir` profile and get past Cloudflare on k-ruoka.fi.
//!   3. `/kr-api/basket/active` is reachable from inside that page with no
//!      `puppeteer-extra-plugin-stealth`-style patching.
//!
//! It now runs those measurements *through `Session`*, the same code the server
//! uses, rather than its own copy of the launch arguments and User-Agent logic.
//! That keeps "reproduce with `cargo run --bin spike`" honest as
//! `session.rs` evolves — a private copy would drift and quietly stop testing what
//! actually ships.
//!
//!   cargo run --bin spike -- [--head] [profile-dir]
//!
//! `--head` runs headful (wrap in `xvfb-run -a` on a machine with no display) to
//! check the headful-`login` → headless-`serve` clearance handoff against one
//! profile: run it once with `--head`, then again without, against the same dir,
//! and compare the reported `cf_clearance`.

use std::time::Instant;

use anyhow::Result;
use k_ruoka_mcp::browser::{LaunchMode, Session};

const STORE_ID: &str = "N137"; // K-Citymarket Helsinki Ruoholahti

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let headful = args.iter().any(|a| a == "--head");
    let profile = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "/tmp/k-ruoka-spike-profile".to_string());

    let mode = if headful {
        LaunchMode::Headful { debug_port: 9333 }
    } else {
        LaunchMode::Headless
    };
    println!(
        "mode:    {}",
        if headful { "headful" } else { "headless=new" }
    );
    println!("profile: {profile}");

    let session = Session::new(&profile, mode)?;

    // The whole of check 2, in one line. A string containing `HeadlessChrome` is
    // blocked outright; this one is served the real shop. Printed so the evidence
    // for the finding is in the output, not just its consequence -- and so
    // `K_RUOKA_USER_AGENT='...HeadlessChrome...' cargo run --bin spike` visibly
    // demonstrates the negative case.
    println!("user-agent: {}\n", session.user_agent());

    // [check 2] Clearance. `Session` launches lazily, so the first call is what
    // pays for the browser launch, the navigation and the Cloudflare wait.
    let start = Instant::now();
    let cookies = session
        .with_page(|page| async move {
            let title = page.get_title().await?.unwrap_or_default();
            println!("[check 2] title: {title}");
            Ok(page.get_cookies().await?)
        })
        .await?;
    // Launch included, unlike a navigation-only figure: `Session`
    // launches lazily, so this is the whole cold path and is not comparable to them.
    println!(
        "[check 2] browser launch + clearance: {:.1?}",
        start.elapsed()
    );

    let names: Vec<&str> = cookies
        .iter()
        .filter(|c| c.domain.contains("k-ruoka"))
        .map(|c| c.name.as_str())
        .collect();
    println!("[check 2] k-ruoka cookies: {names:?}");

    // Fingerprint the clearance cookie, so a headful->headless handoff can be told
    // apart from a silent re-challenge that also happens to succeed.
    if let Some(c) = cookies.iter().find(|c| c.name == "cf_clearance") {
        let v = &c.value;
        println!(
            "[check 2] cf_clearance: len={} head={} tail={}",
            v.len(),
            &v[..v.len().min(12)],
            &v[v.len().saturating_sub(8)..]
        );
    }

    // [check 3] The private API, from inside the page. Run twice: without
    // `X-K-Build-Number` the API answers 409 and hands back the real build in its
    // own `k-ruoka-build` header, which `Session` then retries with. The retry logs
    // to stderr, so a run that never retried is distinguishable from one that did.
    let body = serde_json::json!({ "storeId": STORE_ID, "substitutionDefault": true });
    for label in ["cold (no build number yet)", "warm (build number known)"] {
        if label.starts_with("cold") {
            session.set_build(None).await;
        }
        let started = Instant::now();
        let basket = session
            .api("POST", "/kr-api/basket/active", Some(&body))
            .await?;
        println!(
            "\n[check 3] {label}: 200 in {:.1?}\n           basket={} store={} items={}",
            started.elapsed(),
            basket["id"].as_str().unwrap_or("?"),
            basket["store"]["name"].as_str().unwrap_or("?"),
            basket["items"].as_array().map_or(0, |a| a.len()),
        );
    }

    // Graceful close: Chrome flushes cookies to the profile on clean shutdown.
    // Killing it can lose them, which would silently break login persistence.
    session.close().await?;
    println!("\n[check 2] closed gracefully; profile at {profile}");
    Ok(())
}
