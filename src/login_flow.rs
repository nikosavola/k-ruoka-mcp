//! Driving `login` from inside `serve`, so an assistant can walk a user through it.
//!
//! The point is that a model can start the flow and relay the instructions itself,
//! rather than telling someone to go and find a terminal. Credentials are still never
//! automated: all this does is put a browser in front of the human and watch for
//! K-Ruoka to start reporting an account.
//!
//! It runs the existing `login` subcommand as a child process rather than growing a
//! second browser mode inside `serve`. Two reasons: a profile directory supports only
//! one Chrome, so `serve` has to let go of it anyway (see
//! [`Session::release_for_login`]), and the subcommand already handles the parts that
//! were awkward to get right -- the xvfb re-exec, the separate tab for the human, the
//! poller, and the graceful close that makes the cookies persist.

use std::process::Stdio;
use std::sync::Arc;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::browser::Session;
use crate::browser::session::ApiError;

/// How long to wait for `login` to print its instructions before answering anyway.
const INSTRUCTIONS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The banner `login` prints once the browser is up and it is waiting for the human.
const READY_MARKER: &str = "Sign in by hand";

// `Deserialize` is only for the test fake, which scripts a progress value from JSON
// rather than reimplementing the struct.
#[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct LoginProgress {
    /// `waiting`, `signedIn`, `failed`, or `notStarted`.
    pub state: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// What `login` printed. For `waiting` these are the steps to give the user
    /// verbatim: they differ between a desktop and a headless host, and only the
    /// running process knows which it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl Default for LoginProgress {
    fn default() -> Self {
        Self::new("notStarted", String::new())
    }
}

impl LoginProgress {
    fn new(state: &str, detail: impl Into<String>) -> Self {
        Self {
            state: state.to_string(),
            detail: detail.into(),
            account: None,
            instructions: None,
        }
    }
}

/// What the login tools need. A trait so the tool surface can be tested without
/// spawning a browser, the same reason [`crate::browser::KrApi`] is one.
#[async_trait::async_trait]
pub trait LoginFlow: Send + Sync {
    /// Open a browser for the user to sign into, and return the instructions to relay.
    async fn start(&self, debug_port: u16) -> Result<LoginProgress, ApiError>;
    async fn status(&self) -> Result<LoginProgress, ApiError>;
    async fn cancel(&self) -> Result<LoginProgress, ApiError>;
}

struct Running {
    child: Child,
    /// Everything the child has printed, stdout and stderr merged in arrival order.
    output: Arc<Mutex<String>>,
}

pub struct ChildLogin {
    session: Arc<Session>,
    running: Mutex<Option<Running>>,
}

impl ChildLogin {
    pub fn new(session: Arc<Session>) -> Self {
        Self {
            session,
            running: Mutex::new(None),
        }
    }

    /// Stop a login that is still running, for `serve`'s own shutdown.
    ///
    /// Not part of [`LoginFlow`]: it is not a tool, and it must not report anything to a
    /// model. `serve` exits with `std::process::exit`, which runs no destructors, so
    /// `kill_on_drop` never fires -- and the child is in its own process group precisely
    /// so that a signal to `serve` does not reach it. Without this a client shutting the
    /// server down mid-login leaves Chrome holding the profile's lock.
    pub async fn shutdown(&self) {
        if let Some(mut running) = self.running.lock().await.take() {
            terminate_group(&mut running.child).await;
        }
    }
}

#[async_trait::async_trait]
impl LoginFlow for ChildLogin {
    async fn start(&self, debug_port: u16) -> Result<LoginProgress, ApiError> {
        let mut slot = self.running.lock().await;
        if let Some(running) = slot.as_mut() {
            // Already going: report it rather than starting a second browser on the
            // same profile, which cannot work.
            if running.child.try_wait().map_err(wrap)?.is_none() {
                let output = running.output.lock().await.clone();
                let mut progress = LoginProgress::new(
                    "waiting",
                    "A login is already in progress. Give the user these instructions.",
                );
                progress.instructions = Some(output);
                return Ok(progress);
            }
            // The child is gone but nothing has observed that yet, so the session is
            // still holding the profile for it. Hand it back before asking for it again:
            // `release_for_login` refuses while the flag is set, and forgetting the
            // handle here without clearing it left every tool refusing until a restart.
            *slot = None;
            self.session.resume_after_login().await;
        }

        // Hand the profile over before spawning: the child needs the SingletonLock this
        // session is holding.
        self.session.release_for_login().await?;

        let exe = std::env::current_exe().map_err(|e| {
            ApiError::Other(anyhow::anyhow!(
                "cannot find this executable to re-run it: {e}"
            ))
        })?;
        let mut command = Command::new(&exe);
        command
            .arg("login")
            .arg("--port")
            .arg(debug_port.to_string())
            // stdin must be null and both streams piped: this process's stdout is the
            // MCP JSON-RPC channel, and anything the child wrote to it would corrupt
            // the protocol.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Its own process group, so cancelling can signal the whole tree. Chrome is a
        // *grandchild* here (and `login` may re-exec itself under xvfb-run in between),
        // so killing only the direct child leaves Chrome running and holding the
        // profile's SingletonLock -- which then blocks `serve` from ever launching again.
        #[cfg(unix)]
        command.process_group(0);
        let spawned = command.spawn();

        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                self.session.resume_after_login().await;
                return Err(ApiError::Other(anyhow::anyhow!(
                    "could not start `{} login`: {e}",
                    exe.display()
                )));
            }
        };

        let output = Arc::new(Mutex::new(String::new()));
        for stream in [
            child.stdout.take().map(Pipe::Out),
            child.stderr.take().map(Pipe::Err),
        ]
        .into_iter()
        .flatten()
        {
            let sink = Arc::clone(&output);
            tokio::spawn(async move {
                let mut lines = match stream {
                    Pipe::Out(s) => BufReader::new(Box::pin(s) as PinnedRead).lines(),
                    Pipe::Err(s) => BufReader::new(Box::pin(s) as PinnedRead).lines(),
                };
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut buf = sink.lock().await;
                    buf.push_str(&line);
                    buf.push('\n');
                }
            });
        }

        // Wait for the instructions rather than returning immediately, so the caller
        // gets something to show the user in the same turn.
        let deadline = tokio::time::Instant::now() + INSTRUCTIONS_TIMEOUT;
        loop {
            if output.lock().await.contains(READY_MARKER) {
                break;
            }
            if child.try_wait().map_err(wrap)?.is_some() {
                // Died before printing anything useful, e.g. no display and no xvfb-run.
                let detail = output.lock().await.clone();
                self.session.resume_after_login().await;
                return Err(ApiError::Other(anyhow::anyhow!(
                    "login exited before it was ready:\n{}",
                    detail.trim()
                )));
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }

        let instructions = output.lock().await.clone();
        *slot = Some(Running { child, output });

        let mut progress = LoginProgress::new(
            "waiting",
            "A browser is open and waiting for the user to sign in. Give them the \
             instructions verbatim, then poll login_status. Nothing here sees their \
             credentials.",
        );
        progress.instructions = Some(instructions);
        Ok(progress)
    }

    async fn status(&self) -> Result<LoginProgress, ApiError> {
        let mut slot = self.running.lock().await;
        let Some(running) = slot.as_mut() else {
            return Ok(LoginProgress::new(
                "notStarted",
                "No login is in progress. Call start_login to begin one, or auth_status \
                 to check whether the stored session is already signed in.",
            ));
        };

        let exited = running.child.try_wait().map_err(wrap)?;
        let output = running.output.lock().await.clone();
        let Some(status) = exited else {
            let mut progress = LoginProgress::new(
                "waiting",
                "Still waiting for the user to finish signing in.",
            );
            progress.instructions = Some(output);
            return Ok(progress);
        };

        *slot = None;
        self.session.resume_after_login().await;

        if status.success() {
            let mut progress = LoginProgress::new(
                "signedIn",
                "Signed in. The session is stored in the browser profile and the cart \
                 tools will use it from now on.",
            );
            progress.account = signed_in_account(&output);
            Ok(progress)
        } else {
            let mut progress = LoginProgress::new(
                "failed",
                "The login did not complete. The stored profile was left untouched, so \
                 any previous session is still there.",
            );
            progress.instructions = Some(output);
            Ok(progress)
        }
    }

    async fn cancel(&self) -> Result<LoginProgress, ApiError> {
        let mut slot = self.running.lock().await;
        let Some(mut running) = slot.take() else {
            // Unconditional: cancel_login is the documented escape hatch, so it has to
            // work even when the handle is already gone and only the flag is left.
            self.session.resume_after_login().await;
            return Ok(LoginProgress::new(
                "notStarted",
                "No login was in progress.",
            ));
        };
        // SIGTERM the group first so `login` can close Chrome cleanly, then insist.
        // A cancelled login has nothing to flush, but Chrome left running would keep the
        // profile locked.
        terminate_group(&mut running.child).await;
        self.session.resume_after_login().await;
        Ok(LoginProgress::new(
            "notStarted",
            "Login cancelled and the browser closed. The cart tools work again.",
        ))
    }
}

/// `login` prints `Signed in as <name> <email>` on success.
fn signed_in_account(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|l| l.trim().strip_prefix("Signed in as "))
        .map(|who| who.trim_end_matches('.').to_string())
}

/// Stop the child and everything it started, Chrome included.
async fn terminate_group(child: &mut Child) {
    // Chrome is a grandchild, and Windows has no process groups to signal: terminating
    // only the direct child would leave a headful Chrome holding the profile lock while
    // the tool reported the browser closed. taskkill /T takes the tree.
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // Negative pid means "the process group", which is why it was spawned into one.
        // SIGTERM lets `login` close Chrome gracefully; SIGKILL is the backstop for
        // anything that ignores it.
        unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
        for _ in 0..20 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

fn wrap(e: std::io::Error) -> ApiError {
    ApiError::Other(anyhow::anyhow!("watching the login process: {e}"))
}

type PinnedRead = std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>;

enum Pipe {
    Out(tokio::process::ChildStdout),
    Err(tokio::process::ChildStderr),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session with nothing launched. Cheap and hermetic: `Session::new` touches no
    /// Chrome, and `release_for_login` with no live browser only moves the flag.
    fn scratch_session(name: &str) -> Arc<Session> {
        let profile = std::env::temp_dir().join(format!("k-ruoka-login-flow-{name}"));
        let _ = std::fs::remove_dir_all(&profile);
        Arc::new(Session::new(&profile, crate::browser::LaunchMode::Headless).unwrap())
    }

    /// The flag that makes the cart tools refuse is only cleared by whoever set it, so
    /// `cancel_login` has to clear it even when there is no child left to kill. Without
    /// this, a login whose child had already gone left every tool refusing until the
    /// process was restarted, while telling the caller to run the very tool that could
    /// not help.
    #[tokio::test]
    async fn cancelling_frees_the_profile_even_with_no_child_left() {
        let session = scratch_session("cancel");
        session.release_for_login().await.unwrap();
        assert!(
            session.release_for_login().await.is_err(),
            "a second login must be refused while one owns the profile"
        );

        ChildLogin::new(Arc::clone(&session))
            .cancel()
            .await
            .unwrap();

        session
            .release_for_login()
            .await
            .expect("cancel_login must hand the profile back");
    }

    #[test]
    fn the_account_is_read_out_of_logins_own_output() {
        let output = "Opening a browser against /x\n\nSigned in as Niko Savola <a@b.c>.\n\
                      Session saved to /x.\n";
        assert_eq!(
            signed_in_account(output).as_deref(),
            Some("Niko Savola <a@b.c>")
        );
    }

    #[test]
    fn no_account_line_is_not_an_account() {
        assert_eq!(signed_in_account("timed out after 15 minutes\n"), None);
    }
}
