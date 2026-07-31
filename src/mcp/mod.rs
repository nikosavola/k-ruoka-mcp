//! MCP protocol wiring: tool registration and the stdio transport.

pub mod tools;

use std::sync::Arc;

use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};
#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};

use crate::browser::{KrApi, LaunchMode, Session, session::default_profile_dir};
use crate::login_flow::ChildLogin;
pub use tools::CartServer;

/// Stderr breadcrumbs for the startup and shutdown path, off unless asked for.
///
/// Which of these appears is what located a Windows-only hang that no local run
/// reproduced: only the first line printed, so the process was still in startup rather
/// than stuck on the transport, which is where it looked like it was.
macro_rules! trace_shutdown {
    ($($arg:tt)*) => {
        if std::env::var_os("K_RUOKA_TRACE_SHUTDOWN").is_some() {
            eprintln!("k-ruoka-mcp[trace]: {}", format_args!($($arg)*));
        }
    };
}

pub async fn serve() -> Result<()> {
    // Before anything else, including startup. Tokio installs the OS handler inside
    // `signal()` rather than on the first poll, so registering here is what shrinks the
    // window in which a SIGTERM is fatal down to almost nothing. Startup is cheap now
    // that the User-Agent is derived lazily, but a signal arriving inside it would still
    // kill the process outright, and that window is the one thing this ordering closes.
    let mut terminate = TerminateSignals::install();
    trace_shutdown!("signals installed");

    // One browser for the life of the server. A profile dir supports a single
    // Chrome instance, and relaunching per tool call would be slow and would
    // fight over the profile lock. The browser is launched lazily on the first
    // tool call, so `serve` starts instantly and a client that only lists tools
    // never pays for it.
    let session = Arc::new(Session::new(default_profile_dir()?, LaunchMode::Headless)?);

    // The login tools drive the `login` subcommand as a child process, which needs the
    // session itself (to hand over the profile), not just the API seam.
    let login = Arc::new(ChildLogin::new(Arc::clone(&session)));
    let login_for_shutdown = Arc::clone(&login);
    let handler = CartServer::with_login(Arc::clone(&session) as Arc<dyn KrApi>, login);
    trace_shutdown!("session built, starting the handshake");

    let serving = async {
        // The handshake is inside the select on purpose: a signal arriving while the
        // server is still waiting for `initialize` must be handled too, and it was
        // not when only `waiting()` was covered.
        let service = handler.serve(stdio()).await?;
        trace_shutdown!("handshake done, serving");
        service.waiting().await?;
        anyhow::Ok(())
    };

    // MCP clients shut a stdio server down by signalling it, so SIGTERM is the
    // *normal* exit path here, not an edge case -- and taking it by default would
    // skip the close below, losing the cookie flush that `login` exists to produce.
    let outcome = tokio::select! {
        result = serving => {
            trace_shutdown!("the service loop ended on its own");
            result
        }
        signal = terminate.recv() => {
            eprintln!("k-ruoka-mcp: {signal}, shutting the browser down cleanly");
            Ok(())
        }
    };

    // Close gracefully so Chrome flushes cookies back into the profile; a killed
    // browser can lose the session and force an unnecessary re-login. This is the
    // whole reason for handling the signal, so it must finish before we go.
    // The login child first: it owns the profile while it runs, and exiting without
    // stopping it leaves a headful Chrome holding the profile's lock.
    // Before the login stop, not after: stopping a login waits on its own lock, which a
    // start_login can be holding while it waits for the `live` lock that a clearance poll
    // owns. Signalling first is what lets that poll release it.
    session.signal_shutdown();
    trace_shutdown!("stopping any login, then closing the browser");
    login_for_shutdown.shutdown().await;
    session.close().await.ok();
    trace_shutdown!("browser closed, exiting");

    // Both paths exit explicitly rather than returning. Returning hands control back to
    // the runtime, which waits on tokio's blocking stdin reader -- and that read does not
    // reliably return even once the client has closed stdin. On the signal path stdin is
    // still open, so it never returns at all; on Windows it does not return on the
    // ordinary path either, which left `serve` running forever after its client
    // disconnected (caught by `closing_stdin_ends_the_session_cleanly` on windows-latest,
    // where it hung until the test's own deadline -- a `cargo check` cannot see this).
    //
    // Exiting is safe precisely because the one thing that must be flushed, the browser
    // profile, was closed and awaited above.
    match outcome {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            // main would have printed this; do it here since we never return to it.
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
    }
}

/// The signals that mean "stop", registered up front.
///
/// Held as a value rather than awaited as a one-shot future so that installation
/// and waiting are separate moments: a signal delivered between the two is queued
/// by tokio and delivered to `recv`, which is the whole point.
#[cfg(unix)]
struct TerminateSignals {
    /// `None` if the handler could not be installed. That leaves the `select!`
    /// waiting on the service exactly as it would have anyway -- a shutdown that is
    /// merely ungraceful is much better than refusing to start.
    term: Option<Signal>,
    int: Option<Signal>,
}

#[cfg(unix)]
impl TerminateSignals {
    fn install() -> Self {
        Self {
            term: signal(SignalKind::terminate()).ok(),
            int: signal(SignalKind::interrupt()).ok(),
        }
    }

    /// Resolves on the first SIGTERM or SIGINT, naming which arrived.
    async fn recv(&mut self) -> &'static str {
        match (&mut self.term, &mut self.int) {
            (Some(term), Some(int)) => tokio::select! {
                _ = term.recv() => "SIGTERM",
                _ = int.recv() => "SIGINT",
            },
            (Some(term), None) => {
                term.recv().await;
                "SIGTERM"
            }
            (None, Some(int)) => {
                int.recv().await;
                "SIGINT"
            }
            (None, None) => std::future::pending().await,
        }
    }
}

/// The Windows equivalents.
///
/// Same contract and the same reason for existing: the browser must be closed cleanly so
/// Chrome flushes cookies into the profile, and the default action for these events does
/// not do that. `ctrl_close` is the console-window close and `ctrl_shutdown` is system
/// shutdown, which together stand in for SIGTERM; both give the process only a short
/// grace period, so the close has to be prompt.
#[cfg(windows)]
struct TerminateSignals {
    ctrl_c: Option<tokio::signal::windows::CtrlC>,
    ctrl_close: Option<tokio::signal::windows::CtrlClose>,
    ctrl_shutdown: Option<tokio::signal::windows::CtrlShutdown>,
}

#[cfg(windows)]
impl TerminateSignals {
    fn install() -> Self {
        Self {
            ctrl_c: tokio::signal::windows::ctrl_c().ok(),
            ctrl_close: tokio::signal::windows::ctrl_close().ok(),
            ctrl_shutdown: tokio::signal::windows::ctrl_shutdown().ok(),
        }
    }

    async fn recv(&mut self) -> &'static str {
        // A helper per field would need three types; awaiting `Option`s directly is
        // simpler, and `pending()` for an absent one keeps the select well-formed.
        async fn wait<T>(slot: &mut Option<T>, name: &'static str) -> &'static str
        where
            T: TerminateEvent,
        {
            match slot {
                Some(stream) => {
                    stream.recv().await;
                    name
                }
                None => std::future::pending().await,
            }
        }
        tokio::select! {
            name = wait(&mut self.ctrl_c, "Ctrl-C") => name,
            name = wait(&mut self.ctrl_close, "console close") => name,
            name = wait(&mut self.ctrl_shutdown, "system shutdown") => name,
        }
    }
}

/// The one method the three Windows event streams share; they have no common trait.
#[cfg(windows)]
trait TerminateEvent {
    async fn recv(&mut self) -> Option<()>;
}

#[cfg(windows)]
macro_rules! impl_terminate_event {
    ($($t:ty),*) => {
        $(impl TerminateEvent for $t {
            async fn recv(&mut self) -> Option<()> {
                <$t>::recv(self).await
            }
        })*
    };
}

#[cfg(windows)]
impl_terminate_event!(
    tokio::signal::windows::CtrlC,
    tokio::signal::windows::CtrlClose,
    tokio::signal::windows::CtrlShutdown
);
