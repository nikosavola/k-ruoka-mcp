//! MCP protocol wiring: tool registration and the stdio transport.

pub mod tools;

use std::sync::Arc;

use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};
#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};

use crate::browser::{KrApi, LaunchMode, Session, session::default_profile_dir};
pub use tools::CartServer;

pub async fn serve() -> Result<()> {
    // Before anything else, including startup. Tokio installs the OS handler inside
    // `signal()` rather than on the first poll, so registering here is what shrinks
    // the window in which a SIGTERM is fatal down to almost nothing. Doing it lazily
    // in the `select!` below left `Session::new` inside that window, and it forks
    // `google-chrome --version`, which is not bounded on a loaded machine.
    let mut terminate = TerminateSignals::install();

    // One browser for the life of the server. A profile dir supports a single
    // Chrome instance, and relaunching per tool call would be slow and would
    // fight over the profile lock. The browser is launched lazily on the first
    // tool call, so `serve` starts instantly and a client that only lists tools
    // never pays for it.
    let session = Arc::new(Session::new(default_profile_dir()?, LaunchMode::Headless)?);

    let handler = CartServer::new(Arc::clone(&session) as Arc<dyn KrApi>);
    let serving = async {
        // The handshake is inside the select on purpose: a signal arriving while the
        // server is still waiting for `initialize` must be handled too, and it was
        // not when only `waiting()` was covered.
        let service = handler.serve(stdio()).await?;
        service.waiting().await?;
        anyhow::Ok(())
    };

    // MCP clients shut a stdio server down by signalling it, so SIGTERM is the
    // *normal* exit path here, not an edge case -- and taking it by default would
    // skip the close below, losing the cookie flush that `login` exists to produce.
    let mut signalled = false;
    let outcome = tokio::select! {
        result = serving => result,
        signal = terminate.recv() => {
            eprintln!("k-ruoka-mcp: {signal}, shutting the browser down cleanly");
            signalled = true;
            Ok(())
        }
    };

    // Close gracefully so Chrome flushes cookies back into the profile; a killed
    // browser can lose the session and force an unnecessary re-login. This is the
    // whole reason for handling the signal, so it must finish before we go.
    session.close().await.ok();

    if signalled {
        // Returning here would hang. `tokio::io::stdin` reads on a blocking thread,
        // and on the signal path stdin is still open, so that read never returns and
        // runtime shutdown waits on it forever. (The ordinary path does not hit this:
        // stdin closing is what ended the session, so the read has already returned.)
        //
        // Exiting explicitly is safe precisely because the one thing that must be
        // flushed -- the browser profile -- was just closed and awaited above.
        // `outcome` is unconditionally `Ok` here: it can only have come from the
        // signal arm, which is what set this flag.
        std::process::exit(0);
    }
    outcome
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
