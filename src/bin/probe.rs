//! Exploration tool: issue an arbitrary `/kr-api/` call through a real session.
//!
//! K-Ruoka's API is undocumented and reverse-engineered, so shapes
//! have to be observed rather than recalled. This drives the same `Session` the
//! server uses, so what it observes is what the server will get.
//!
//!   cargo run --bin probe -- POST /kr-api/basket/active '{"storeId":"N137"}'
//!
//! Flags, both for exercising the recovery paths in `Session::api`:
//!
//!   --drop-clearance   delete cf_clearance/__cf_bm from the live page first, so
//!                      the next API call is made by a legitimate browser with no
//!                      Cloudflare clearance -- the "long-running serve whose
//!                      clearance aged out" case, which is otherwise unwaitable.
//!   --stale-build      seed a wrong X-K-Build-Number, to watch the 409 self-heal.
//!
//! Defaults to a scratch profile, not the real login. Override with K_RUOKA_PROFILE.

use anyhow::Result;
use chromiumoxide::cdp::browser_protocol::network::DeleteCookiesParams;
use k_ruoka_mcp::browser::{LaunchMode, Session};

/// Cloudflare's clearance pair. `cf_clearance` proves the challenge was passed;
/// `__cf_bm` is the bot-management cookie.
const CF_COOKIES: [&str; 2] = ["cf_clearance", "__cf_bm"];

#[tokio::main]
async fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let drop_clearance = argv.iter().any(|a| a == "--drop-clearance");
    // `--build=VALUE` seeds an arbitrary X-K-Build-Number; `--build=` sends none.
    let forced_build = argv
        .iter()
        .find_map(|a| a.strip_prefix("--build="))
        .map(str::to_string);
    let mut positional = argv.iter().filter(|a| !a.starts_with("--"));

    let method = positional.next().cloned().unwrap_or_else(|| "POST".into());
    let path = positional
        .next()
        .cloned()
        .unwrap_or_else(|| "/kr-api/basket/active".into());
    let body: Option<serde_json::Value> = match positional.next() {
        Some(b) => Some(serde_json::from_str(b)?),
        None => None,
    };

    let profile =
        std::env::var("K_RUOKA_PROFILE").unwrap_or_else(|_| "/tmp/k-ruoka-probe-profile".into());
    let session = Session::new(&profile, LaunchMode::Headless)?;

    if let Some(build) = &forced_build {
        session.set_build(Some(build.clone())).await;
        println!("seeded X-K-Build-Number={build}");
    }

    if drop_clearance {
        let before = cookie_names(&session).await?;
        session
            .with_page(|page| async move {
                // Derive the delete params from the cookies actually present rather
                // than guessing domain/path. `cf_clearance` is a partitioned (CHIPS)
                // cookie, and CDP will not delete it unless the partitionKey matches
                // -- a guessed domain+path silently no-ops.
                let params: Vec<DeleteCookiesParams> = page
                    .get_cookies()
                    .await?
                    .into_iter()
                    .filter(|c| CF_COOKIES.contains(&c.name.as_str()))
                    .map(|c| {
                        let mut b = DeleteCookiesParams::builder()
                            .name(&c.name)
                            .domain(&c.domain)
                            .path(&c.path);
                        if let Some(pk) = c.partition_key {
                            b = b.partition_key(pk);
                        }
                        b.build().expect("name is set")
                    })
                    .collect();
                println!("deleting {} clearance cookie(s)", params.len());
                page.delete_cookies(params).await?;
                Ok(())
            })
            .await?;
        let after = cookie_names(&session).await?;
        println!("cookies before: {before:?}");
        println!("cookies after : {after:?}\n");
    }

    let result = session.api(&method, &path, body.as_ref()).await;
    if drop_clearance {
        // Distinguishes "Cloudflare did not need the cookie" from "Cloudflare
        // silently re-issued it" -- both explain a 200, but they are different
        // facts about how reachable the challenge path actually is.
        println!(
            "cookies after the call: {:?}\n",
            cookie_names(&session).await?
        );
    }
    session.close().await.ok();

    match result {
        // Printed in full: this is a tool for reading undocumented responses, and
        // truncating produced invalid JSON that could not be piped into anything.
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
    Ok(())
}

async fn cookie_names(session: &Session) -> Result<Vec<String>> {
    session
        .with_page(|page| async move {
            let cookies = page.get_cookies().await?;
            Ok(cookies
                .into_iter()
                .filter(
                    |c: &chromiumoxide::cdp::browser_protocol::network::Cookie| {
                        c.domain.contains("k-ruoka")
                    },
                )
                .map(|c| {
                    format!(
                        "{} (domain={} path={} partition={:?})",
                        c.name, c.domain, c.path, c.partition_key
                    )
                })
                .collect())
        })
        .await
}
