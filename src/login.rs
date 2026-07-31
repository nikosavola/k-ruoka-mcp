//! The `login` subcommand: a visible browser the user signs into by hand.
//!
//! Credentials and MFA are never automated and never touched by this program.
//! All it does is put a real browser in front of the user, wait until K-Ruoka
//! starts reporting an account, and shut Chrome down cleanly so the cookies land
//! in the profile.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::browser::basket::Cart;
use crate::browser::session::{SHOP_URL, default_profile_dir, evaluate};
use crate::browser::{LaunchMode, Session};

/// Long enough for a password manager, an OIDC hop and an MFA prompt.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const POLL_EVERY: Duration = Duration::from_secs(3);

/// Any store works -- `basket/active` reports `userInfo` regardless of which.
pub const DEFAULT_PROBE_STORE: &str = "N137";

/// Cookies are browser-wide, so the poller can sit in its own tab. Re-stamped on
/// every poll, both so React cannot overwrite it and so the tab is unmistakable
/// in `chrome://inspect`, where the user has to pick the right one.
const POLLER_TITLE: &str = "[k-ruoka-mcp] poller - do NOT use this tab";
const USER_TAB_TITLE: &str = "Tuotteet | K-Ruoka Verkkokauppa";

/// Set by the xvfb re-exec on its own child. Not the same thing as
/// `K_RUOKA_NO_XVFB`, which is the user's opt-out.
const UNDER_XVFB_ENV: &str = "K_RUOKA_UNDER_XVFB";

pub async fn run(debug_port: u16, store_id: &str) -> Result<()> {
    let display = Display::detect();
    reexec_under_xvfb_if_headless()?;
    ensure_port_free(debug_port)?;

    let profile = default_profile_dir()?;
    let session = Session::new(&profile, LaunchMode::Headful { debug_port })?;

    println!("Opening a browser against {}", profile.display());

    // The session's own page becomes the poller. The user gets a separate tab, so
    // that nothing this process does ever navigates the page they are typing into.
    let _user_page = session
        .open_extra_page(SHOP_URL)
        .await
        .context("launching the login browser")?;

    print_instructions(display, debug_port);

    let cart = Cart::new(&session);

    // Probe once before telling the user to go and sign in. Without this, a typo'd
    // --store-id (or a Cloudflare block, or a broken Chrome) waits the full 15
    // minutes and then reports "no signed-in account", which misattributes the cause.
    // "Not signed in yet" is the expected answer here and is not an error.
    if let Err(e) = cart.active(store_id).await {
        anyhow::bail!("cannot reach K-Ruoka, so signing in would not be detected: {e}");
    }

    let deadline = Instant::now() + LOGIN_TIMEOUT;
    let mut result = Err(anyhow::anyhow!(
        "timed out after {} minutes without seeing a signed-in account",
        LOGIN_TIMEOUT.as_secs() / 60
    ));

    while Instant::now() < deadline {
        tokio::time::sleep(POLL_EVERY).await;
        mark_poller_tab(&session).await;

        // A failure here is expected and uninteresting while the user is still
        // mid-login (they are off on login.kesko.fi), so keep polling.
        if let Ok(basket) = cart.active(store_id).await
            && let Some(who) = basket.user_info.display()
        {
            println!("\nSigned in as {who}.");
            result = Ok(());
            break;
        }
    }

    // Graceful close, so Chrome flushes cookies into the profile. Without this
    // the login appears to work and then silently isn't there next time.
    session.close().await.ok();

    match result {
        Ok(()) => {
            println!("Session saved to {}.", profile.display());
            println!("`k-ruoka-mcp serve` will now use it. Re-run `login` if it expires.");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Best-effort; a failure here only costs the tab its label.
async fn mark_poller_tab(session: &Session) {
    let js = format!("document.title = {}", serde_json::json!(POLLER_TITLE));
    let _ = session
        .with_page(|page| async move {
            evaluate(&page, &js).await?;
            Ok(())
        })
        .await;
}

/// Whether the user can see the browser we just opened. It decides which set of
/// instructions is true, and the two are completely different -- telling someone
/// with a window in front of them to set up an SSH tunnel is worse than useless.
#[derive(Clone, Copy)]
enum Display {
    /// A real X/Wayland session: the window is on screen.
    Real,
    /// Xvfb, so the only way in is CDP over the debug port.
    Virtual,
}

impl Display {
    /// Called *before* [`reexec_under_xvfb_if_headless`], which replaces the
    /// process; the re-exec'd child detects again and sees the marker.
    fn detect() -> Self {
        if std::env::var_os(UNDER_XVFB_ENV).is_some() {
            Self::Virtual
        } else if std::env::var_os("DISPLAY").is_some() {
            Self::Real
        } else {
            // About to re-exec under xvfb-run.
            Self::Virtual
        }
    }
}

fn print_instructions(display: Display, port: u16) {
    let steps = match display {
        Display::Real => real_display_steps(),
        Display::Virtual => virtual_display_steps(port),
    };
    println!(
        "\n{steps}\n\
         Nothing here types your credentials for you, and this process never sees\n\
         them -- it only watches for K-Ruoka to start reporting an account.\n\
         Waiting (Ctrl-C to give up)...\n"
    );
}

fn real_display_steps() -> String {
    format!(
        "┌─ Sign in by hand ────────────────────────────────────────────────────┐\n\
         │ A Chrome window has just opened on this machine.                     │\n\
         └──────────────────────────────────────────────────────────────────────┘\n\
         \n\
         1. Switch to it and pick the tab titled\n\
         \n         {USER_TAB_TITLE}\n\
         \n   \
            and NOT the one marked \"{POLLER_TITLE}\" -- that one is this process\n   \
            checking whether you are signed in yet, and it gets navigated out from\n   \
            under you every {poll} seconds.\n\
         \n\
         2. Click \"Kirjaudu\" and sign in to K-Plussa as you normally would. It\n   \
            hands off to login.kesko.fi; that is expected.\n",
        poll = POLL_EVERY.as_secs()
    )
}

fn virtual_display_steps(port: u16) -> String {
    let host = hostname();
    format!(
        "┌─ Sign in by hand ────────────────────────────────────────────────────┐\n\
         │ The browser is running on this machine with no screen attached, so   │\n\
         │ reach it over an SSH tunnel and drive it from your own Chrome.       │\n\
         └──────────────────────────────────────────────────────────────────────┘\n\
         \n\
         1. On your laptop:\n\
         \n    ssh -N -L {port}:localhost:{port} {host}\n\
         \n\
         2. Open chrome://inspect in your local Chrome. Under \"Discover network\n   \
            targets\", click Configure and add   localhost:{port}\n\
         \n\
         3. Under \"Remote Target\" there will be two k-ruoka.fi tabs. Click\n   \
            \"inspect\" on the one titled\n\
         \n         {USER_TAB_TITLE}\n\
         \n   \
            and NOT the one marked \"{POLLER_TITLE}\" -- that one is this process\n   \
            checking whether you are signed in yet.\n\
         \n\
         4. In the inspector's screencast view, click \"Kirjaudu\" and sign in to\n   \
            K-Plussa as you normally would. It hands off to login.kesko.fi; that\n   \
            is expected.\n"
    )
}

/// Only used to print a copy-pasteable `ssh` command, so a fallback is harmless.
fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|h| h.trim().to_string())
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|h| !h.is_empty()))
        .or_else(|| std::env::var("COMPUTERNAME").ok().filter(|h| !h.is_empty()))
        .unwrap_or_else(|| "<this-host>".to_string())
}

/// A leftover Chrome holding the debug port makes `Browser::launch` fail in a way
/// that says nothing useful, so check first and name the real problem.
fn ensure_port_free(port: u16) -> Result<()> {
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
        anyhow::bail!(
            "something is already listening on port {port} -- most likely a Chrome left \
             over from an earlier `login`. Close it (pkill -f remote-debugging-port={port}) \
             or pass --port with a different one."
        );
    }
    Ok(())
}

/// Windows and macOS always have a window server, so there is nothing to arrange.
///
/// `xvfb-run` is a Linux answer to a Linux problem (a server with no display). A Mac or a
/// Windows box running this has a desktop by definition, so `login` just opens a window.
#[cfg(not(target_os = "linux"))]
fn reexec_under_xvfb_if_headless() -> Result<()> {
    Ok(())
}

/// A headful Chrome needs an X display. On a headless VM there isn't one, so
/// re-exec the whole process under `xvfb-run`, which is already the standard
/// tool for exactly this and saves managing an Xvfb child by hand.
#[cfg(target_os = "linux")]
fn reexec_under_xvfb_if_headless() -> Result<()> {
    use std::os::unix::process::CommandExt;

    if std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var_os("K_RUOKA_NO_XVFB").is_some()
    {
        return Ok(());
    }
    let xvfb = which("xvfb-run").context(
        "no DISPLAY and `xvfb-run` is not installed. Install it (apt install xvfb) or run \
         `login` somewhere with a display.",
    )?;

    let exe = std::env::current_exe()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!("No DISPLAY set; re-running under xvfb-run.");

    // execve: replace this process rather than supervising a child, so signals
    // and the exit status pass through untouched.
    let err = std::process::Command::new(xvfb)
        .arg("-a")
        .arg(exe)
        .args(args)
        .env("K_RUOKA_NO_XVFB", "1")
        // xvfb-run sets DISPLAY, so the child would otherwise be indistinguishable
        // from a laptop and print the wrong half of the instructions.
        .env(UNDER_XVFB_ENV, "1")
        .exec();
    Err(anyhow::anyhow!("failed to exec xvfb-run: {err}"))
}

/// Only the xvfb re-exec needs this, and that is Linux-only.
#[cfg(target_os = "linux")]
fn which(bin: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| p.join(bin))
            .find(|p| p.is_file())
    })
}
