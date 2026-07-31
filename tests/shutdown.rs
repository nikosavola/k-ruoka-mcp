//! `serve` must shut down gracefully when it is signalled.
//!
//! MCP clients terminate a stdio server by signalling the process, so SIGTERM is the
//! normal exit path, not an edge case. Taking it ungracefully skips the browser
//! close, and with it the cookie flush that makes a login persist -- the one thing
//! `login` goes out of its way to guarantee.
//!
//! These need no network and no Chrome: the browser is launched lazily on the first
//! tool call, and no tool is ever called here.

use std::io::BufRead;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Drain a child stream into a shared buffer as it arrives.
///
/// Reading the pipes only when the test gives up would mean waiting for EOF, and a
/// child that has not exited never delivers one -- which turned the diagnostic itself
/// into the hang it was meant to explain.
fn drain<R: std::io::Read + Send + 'static>(stream: R, label: &'static str) -> Arc<Mutex<String>> {
    let sink = Arc::new(Mutex::new(String::new()));
    let handle = Arc::clone(&sink);
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stream)
            .lines()
            .map_while(Result::ok)
        {
            let mut buf = handle.lock().unwrap();
            buf.push_str(label);
            buf.push_str(": ");
            buf.push_str(&line);
            buf.push('\n');
        }
    });
    sink
}

/// A scratch profile path that works on every platform.
fn scratch_profile(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("k-ruoka-shutdown-test-{name}"))
}

/// Send `signal` to a running `serve` and return how it exited.
///
/// `handshake` chooses whether the client has completed `initialize` first. Both
/// matter: a signal arriving during startup used to kill the process outright,
/// because the handler was only installed after the handshake completed.
#[cfg(unix)]
fn serve_and_signal(signal: &str, handshake: bool) -> (Option<i32>, Duration, String) {
    let profile = scratch_profile(&format!("{signal}-{handshake}"));
    let _ = std::fs::remove_dir_all(&profile);

    let mut child = Command::new(env!("CARGO_BIN_EXE_k-ruoka-mcp"))
        .arg("serve")
        .env("K_RUOKA_PROFILE", &profile)
        // Which phase the shutdown reached is the only evidence available when this
        // fails on a runner rather than here.
        .env("K_RUOKA_TRACE_SHUTDOWN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning serve");

    if handshake {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("piped stdin");
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"t","version":"0"}}}}}}"#
        )
        .unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .unwrap();
        stdin.flush().unwrap();
    }
    let pid = child.id();
    wait_until_handler_installed(pid, signal);

    Command::new("kill")
        .args([&format!("-{signal}"), &pid.to_string()])
        .status()
        .expect("sending the signal");

    let started = Instant::now();
    let deadline = started + Duration::from_secs(10);
    let status = loop {
        match child.try_wait().expect("polling the child") {
            Some(status) => break status,
            None if Instant::now() > deadline => {
                child.kill().ok();
                panic!("{signal}: did not exit within 10s -- the signal was ignored");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let elapsed = started.elapsed();

    let _ = std::fs::remove_dir_all(&profile);
    (status.code(), elapsed, profile.display().to_string())
}

/// Block until the child has actually installed a handler for `signal`.
///
/// This used to be a flat 400 ms sleep, which is a bet on how fast the child gets
/// through startup. On a loaded machine the bet loses: the signal lands before the
/// handler exists, the default action kills the process, and the test reports the
/// graceful-shutdown *logic* as broken when the logic was never reached. Waiting for
/// the precondition instead makes the test measure what it claims to.
///
/// It also still fails loudly if the handler is dropped altogether -- as a timeout
/// here rather than as a confusing exit code below.
#[cfg(unix)]
fn wait_until_handler_installed(pid: u32, signal: &str) {
    let signo = match signal {
        "TERM" => 15,
        "INT" => 2,
        other => panic!("unhandled signal name {other}"),
    };
    let bit = 1u64 << (signo - 1);

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if caught_signal_mask(pid).is_some_and(|mask| mask & bit != 0) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("serve never installed a SIG{signal} handler (SigCgt bit {signo}) within 10s");
}

/// `SigCgt` from /proc: the mask of signals the process has a handler for.
#[cfg(target_os = "linux")]
fn caught_signal_mask(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let field = status.lines().find_map(|l| l.strip_prefix("SigCgt:"))?;
    u64::from_str_radix(field.trim(), 16).ok()
}

/// No /proc outside Linux, so the precondition cannot be observed directly.
///
/// `ps` does not expose the caught-signal mask, and reading it on macOS would mean
/// `proc_pidinfo` and a libc dependency for a test. So this degrades to the timing bet
/// the Linux path exists to avoid, with a generous margin. It is why CI treats the Linux
/// run as the authority for this file: only there does the test measure the precondition
/// rather than assume it.
#[cfg(all(unix, not(target_os = "linux")))]
fn caught_signal_mask(_pid: u32) -> Option<u64> {
    std::thread::sleep(Duration::from_millis(750));
    // Every bit set: "assume installed" once the wait is over.
    Some(u64::MAX)
}

/// The discriminating check: a *code* rather than a signal death. A process killed by
/// SIGTERM reports `code: None` and `signal: 15`; one that handled it exits with a
/// code. So this fails loudly if the handler is removed.
#[cfg(unix)]
#[test]
fn sigterm_exits_cleanly_rather_than_being_killed() {
    for handshake in [true, false] {
        let (code, elapsed, _) = serve_and_signal("TERM", handshake);
        assert_eq!(
            code,
            Some(0),
            "handshake={handshake}: expected a clean exit; `None` means the process was \
             killed by the signal instead of handling it, which skips the cookie flush"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "handshake={handshake}: shutdown took {elapsed:?}; it should be prompt"
        );
    }
}

/// Ctrl-C during `serve` deserves the same treatment.
#[cfg(unix)]
#[test]
fn sigint_exits_cleanly_too() {
    let (code, _, _) = serve_and_signal("INT", true);
    assert_eq!(code, Some(0), "SIGINT should also shut down gracefully");
}

/// Closing stdin is the other way a client ends the session, and it must not be
/// mistaken for a crash.
#[test]
fn closing_stdin_ends_the_session_cleanly() {
    let profile = scratch_profile("stdin");
    let _ = std::fs::remove_dir_all(&profile);

    let mut child = Command::new(env!("CARGO_BIN_EXE_k-ruoka-mcp"))
        .arg("serve")
        .env("K_RUOKA_PROFILE", &profile)
        // Which phase the shutdown reached is the only evidence available when this
        // fails on a runner rather than here.
        .env("K_RUOKA_TRACE_SHUTDOWN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning serve");

    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("piped stdin");
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"t","version":"0"}}}}}}"#
        )
        .unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .unwrap();
        stdin.flush().unwrap();
    }
    let logged = drain(child.stdout.take().expect("piped stdout"), "stdout");
    let logged_err = drain(child.stderr.take().expect("piped stderr"), "stderr");

    std::thread::sleep(Duration::from_millis(400));
    drop(child.stdin.take());

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait().expect("polling the child") {
            Some(status) => break status,
            None if Instant::now() > deadline => {
                child.kill().ok();
                let err = logged_err.lock().unwrap().clone();
                let out = logged.lock().unwrap().clone();
                panic!("closing stdin did not end the process\n{err}{out}");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    let _ = std::fs::remove_dir_all(&profile);
    assert_eq!(
        status.code(),
        Some(0),
        "closing stdin is a normal end of session"
    );
}
