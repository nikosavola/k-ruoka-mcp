//! Persistent-profile Chrome session, and the classification of what a failed
//! `/kr-api/` call actually means.
//!
//! Everything here rests on one measured fact: a real Chrome whose
//! User-Agent does not say `HeadlessChrome` clears Cloudflare on k-ruoka.fi
//! unaided, and a same-origin `fetch()` from inside the loaded page carries the
//! session cookies without any manual cookie handling.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::{Page, error::CdpError};
use futures::StreamExt;
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

pub const SHOP_URL: &str = "https://www.k-ruoka.fi/kauppa";
pub const SHOP_ORIGIN: &str = "https://www.k-ruoka.fi";

/// A refusal: we are being turned away and waiting will not change that.
///
/// `Pyyntö estetty` ("request blocked") is K-Ruoka's own WAF page; `Attention
/// Required` is Cloudflare's.
const BLOCK_MARKERS: &[&str] = &["Pyyntö estetty", "Attention Required"];

/// A challenge *in progress*. Not a refusal: a real browser runs the JavaScript and
/// it clears itself, which is the entire premise of this design. During a
/// page load these mean "not ready yet, keep waiting", NOT "give up" -- conflating
/// the two would stop the browser from doing the one thing it is here to do.
const CHALLENGE_MARKERS: &[&str] = &["Just a moment", "cdn-cgi/challenge"];

fn first_marker(text: &str, markers: &[&'static str]) -> Option<&'static str> {
    markers.iter().copied().find(|m| text.contains(m))
}

/// Any Cloudflare fingerprint at all. Correct for classifying an API *response*: a
/// challenge page arriving in place of JSON is a failure for that request, however
/// transient the underlying condition.
fn cloudflare_marker(text: &str) -> Option<&'static str> {
    first_marker(text, BLOCK_MARKERS).or_else(|| first_marker(text, CHALLENGE_MARKERS))
}
const CLEARANCE_TIMEOUT: Duration = Duration::from_secs(45);

/// chromiumoxide's `DEFAULT_ARGS` carry `--enable-automation` (a bot signal) and
/// `--lang=en_US`, and its `ArgsBuilder` *merges* repeated keys instead of
/// overriding them, so `lang=fi-FI` on top would produce `--lang=en_US,fi-FI`.
/// We therefore opt out of the defaults and curate the list.
///
/// No leading `--`: chromiumoxide's `Arg` takes the whole string as the flag key
/// and prepends the dashes itself. `"--foo"` here becomes `----foo`, which Chrome
/// ignores in silence.
const CHROME_ARGS: &[&str] = &[
    "disable-background-networking",
    "disable-background-timer-throttling",
    "disable-backgrounding-occluded-windows",
    "disable-breakpad",
    "disable-client-side-phishing-detection",
    "disable-default-apps",
    "disable-dev-shm-usage",
    "disable-hang-monitor",
    "disable-ipc-flooding-protection",
    "disable-popup-blocking",
    "disable-prompt-on-repost",
    "disable-renderer-backgrounding",
    "disable-sync",
    "metrics-recording-only",
    "no-first-run",
    "password-store=basic",
    "use-mock-keychain",
    "disable-blink-features=AutomationControlled",
    "lang=fi-FI",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Headless, for `serve`.
    Headless,
    /// Headful, for `login`. Needs an X display (see `login`'s xvfb-run re-exec).
    Headful { debug_port: u16 },
}

/// Why a `/kr-api/` call did not produce a usable answer.
///
/// The distinction between the first two is the single most important thing in
/// this file: [`ApiError::Cloudflare`] is recoverable by relaunching the browser
/// against the *same* profile, while [`ApiError::AuthExpired`] must never touch
/// the profile — doing so would destroy a real login over a transient failure.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Bot mitigation. Relaunch against the same profile dir; never delete it.
    ///
    /// Raised both by an API response that carries a Cloudflare fingerprint and by
    /// the page load itself being rejected. Both need the same remedy, so both must
    /// land here -- a block during navigation reported as [`ApiError::Other`] would
    /// silently bypass the relaunch that is the only remedy for it.
    #[error("Cloudflare blocked us: {detail}")]
    Cloudflare { detail: String },

    /// The K-Plussa session is gone. Do not retry, do not touch the profile.
    #[error("K-Plussa session has expired -- run `k-ruoka-mcp login` again")]
    AuthExpired,

    /// `X-K-Build-Number` was absent or unparseable. Recoverable: the 409 that
    /// reports it carries the real value in its own `k-ruoka-build` header.
    ///
    /// Despite K-Ruoka's wording ("Client version is too old"), this does *not* mean
    /// they deployed: the header is presence-checked and must parse as a number, but
    /// the value is never compared. In practice this fires on a process's
    /// first call, before the header has been learned.
    #[error("stale X-K-Build-Number (server wants {wanted:?})")]
    StaleBuild { wanted: Option<String> },

    /// The API understood us and said no.
    #[error("K-Ruoka API error (status {status}): {message}")]
    Api { status: u16, message: String },

    /// *We* rejected the request before sending it -- a bad argument, not a K-Ruoka
    /// failure. Kept separate so the message cannot imply that K-Ruoka was asked and
    /// refused. It still reaches the caller as `isError: true` content like every
    /// other tool failure, deliberately: the model is meant to read it and try
    /// something else, which a JSON-RPC `invalid_params` does not guarantee.
    #[error("{0}")]
    InvalidRequest(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// The seam between cart logic and the browser.
///
/// Everything above this trait -- event construction, validation, the rollback of a
/// phantom add, error mapping onto MCP -- is ordinary logic that has nothing to do
/// with Chrome. Naming it lets the tests drive that logic against a fake K-Ruoka
/// instead of a real browser and a live site, which is what makes it possible to
/// test the signed-in branches at all without a login (see `tests/mcp_protocol.rs`).
///
/// Deliberately narrow: one method, the same shape as the underlying request. A
/// wider trait would start duplicating [`crate::browser::basket::Cart`].
#[async_trait::async_trait]
pub trait KrApi: Send + Sync {
    /// Named `call` rather than `api` so it cannot be confused with, or silently
    /// shadowed by, [`Session::api`].
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, ApiError>;
}

#[async_trait::async_trait]
impl KrApi for Session {
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, ApiError> {
        self.api(method, path, body).await
    }
}

/// What to do about a failed attempt. Extracted from [`Session::api`]'s loop so the
/// policy can be tested exhaustively without a browser.
#[derive(Debug, PartialEq, Eq)]
enum Recovery {
    /// Cache this build number and try again.
    RefreshBuild(String),
    /// Replace the browser and try again.
    Relaunch,
    /// Not recoverable, or the one retry has been spent.
    GiveUp,
}

/// `relaunch_unavailable` / `refreshed_build` record whether that remedy is off the
/// table -- either already spent on this request (each is allowed once, so a permanent
/// failure terminates) or, for the relaunch, never permitted in the first place
/// because a human is using the browser.
fn plan_recovery(error: &ApiError, relaunch_unavailable: bool, refreshed_build: bool) -> Recovery {
    match error {
        // Only actionable when the 409 actually carried a value. Writing `None`
        // through would clobber a known-good build for the rest of the process.
        ApiError::StaleBuild {
            wanted: Some(build),
        } if !refreshed_build => Recovery::RefreshBuild(build.clone()),
        ApiError::Cloudflare { .. } if !relaunch_unavailable => Recovery::Relaunch,
        // AuthExpired especially: retrying cannot help, and the profile must not be
        // touched over it.
        _ => Recovery::GiveUp,
    }
}

/// Smallest gap between two `/kr-api/` requests.
///
/// Not a throughput limit; it is about shape. A model looping over a shopping list, or
/// concurrent tool calls, would otherwise arrive as a burst. Slower than a human clicks,
/// deliberately.
const DEFAULT_MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(500);

/// Spaces requests out so concurrent callers queue instead of bursting.
///
/// The lock is held across the sleep on purpose: that is what serialises the queue
/// instead of letting everyone wake together.
struct RateLimiter {
    min_interval: Duration,
    /// When the next request may go out. `None` until the first one.
    next_allowed: Mutex<Option<Instant>>,
}

impl RateLimiter {
    fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            next_allowed: Mutex::new(None),
        }
    }

    /// Returns once the caller may make its request.
    async fn acquire(&self) {
        if self.min_interval.is_zero() {
            return;
        }
        let mut slot = self.next_allowed.lock().await;
        let now = Instant::now();
        // A caller that arrives late has already waited; only an early one sleeps.
        if let Some(next) = *slot
            && let Some(wait) = next.checked_duration_since(now)
        {
            tokio::time::sleep(wait).await;
        }
        *slot = Some(Instant::now() + self.min_interval);
    }
}

/// `K_RUOKA_MIN_REQUEST_INTERVAL_MS` overrides the spacing; `0` disables it.
///
/// The live suites deliberately do not set it: a limiter only ever run at a test value is
/// one nobody has exercised. Costs them ~17 s.
fn min_request_interval() -> Duration {
    match std::env::var("K_RUOKA_MIN_REQUEST_INTERVAL_MS") {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(ms) => Duration::from_millis(ms),
            // A typo must not silently remove the limit.
            Err(_) => {
                eprintln!(
                    "k-ruoka-mcp: K_RUOKA_MIN_REQUEST_INTERVAL_MS={raw:?} is not a number \
                     of milliseconds; using the default"
                );
                DEFAULT_MIN_REQUEST_INTERVAL
            }
        },
        Err(_) => DEFAULT_MIN_REQUEST_INTERVAL,
    }
}

/// The browser went away because the process is stopping.
///
/// Reached two ways, and both are ordinary rather than exceptional: asking for a
/// browser after [`Session::close`], or having `close` empty the slot between
/// `ensure_live` returning and the lock being re-acquired. `Other`, so
/// [`plan_recovery`] gives up rather than retrying into a shutdown.
fn closed_underneath_us() -> ApiError {
    ApiError::Other(anyhow::anyhow!(
        "the server is shutting down; no new browser will be started"
    ))
}

/// Whether relaunching would destroy something a person is in the middle of.
///
/// Headful means `login`: the browser *is* the window the human is signing in through.
/// A relaunch closes it and the replacement gets only the poller page back, so a
/// transient block during the 15-minute poll would make a half-finished sign-in vanish
/// with no explanation while `login` kept polling to its timeout. Failing one poll is
/// the mild outcome: the next one is three seconds later. `login` protects the user's
/// *tab* from being navigated for the same reason; this protects it from teardown.
///
/// Pure, so the mapping is pinned by a test: inverting it is silent, and the cost of
/// getting it wrong is only ever paid by a human mid-password.
fn relaunch_costs_a_human_their_login(mode: LaunchMode) -> bool {
    match mode {
        LaunchMode::Headful { .. } => true,
        LaunchMode::Headless => false,
    }
}

/// Whether [`Session::relaunch`] should replace what is in the slot.
///
/// `current` is the generation now live (`None` if nothing is), `blocked` the one the
/// failed attempt used (`None` if it never got a browser). Split out for the same
/// reason as [`plan_recovery`]: the *decision* is what silently regressed, and a check
/// that needs a real browser cannot cover it. `plan_recovery` deciding `Relaunch` is
/// worth nothing if this then declines to do it.
fn should_replace(current: Option<u64>, blocked: Option<u64>) -> bool {
    match (current, blocked) {
        // Nothing live, so there is nothing to preserve and no way to be too late.
        (None, _) => true,
        // The browser that got blocked is still the live one: replace it.
        (Some(current), Some(blocked)) => current == blocked,
        // The caller never had a browser, so whatever is there arrived after it gave
        // up and is by definition fresher.
        (Some(_), None) => false,
    }
}

/// A raw `/kr-api/` response, before classification.
#[derive(Debug, Deserialize)]
struct RawResponse {
    status: u16,
    build: Option<String>,
    #[serde(rename = "cfMitigated")]
    cf_mitigated: Option<String>,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    body: String,
}

/// Shape of K-Ruoka's own error bodies, e.g.
/// `{"error":{"message":"Client version is too old - reload"}}`.
#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    error: ApiErrorInner,
}

#[derive(Debug, Deserialize)]
struct ApiErrorInner {
    message: String,
}

struct Live {
    browser: Browser,
    page: Page,
    handler: JoinHandle<()>,
    /// Which incarnation of the browser this is. Lets a caller that took a page,
    /// then hit a Cloudflare block, tell "the browser I used is still current, so
    /// I should replace it" from "someone already replaced it, so I should just
    /// retry". rmcp dispatches tool calls concurrently (measured), so without this
    /// N simultaneously-blocked calls each tear down the browser the previous one
    /// just built.
    generation: u64,
}

impl Live {
    /// Close the browser so Chrome flushes cookies into the profile.
    ///
    /// Dropping a `Live` instead kills Chrome in the background with no timing
    /// guarantee, which loses the flush *and* can leave a stale `SingletonLock` for
    /// the next launch against the same profile to trip over. Every *deliberate*
    /// teardown path goes through here; a `Live` dropped because its task was
    /// cancelled mid-launch still takes the ugly route, which is why the launch error
    /// names a leftover Chrome as the likely cause rather than a corrupt profile.
    async fn shutdown(mut self) {
        self.browser.close().await.ok();
        self.browser.wait().await.ok();
        self.handler.abort();
    }
}

pub struct Session {
    profile: PathBuf,
    mode: LaunchMode,
    /// Derived on first use, not in `new`. Deriving it reads Chrome's version, and
    /// doing that eagerly put a Chrome probe in front of `serve`'s startup, which is
    /// meant to be instant and to not need Chrome at all until a tool is called.
    user_agent: std::sync::OnceLock<String>,
    live: Mutex<Option<Live>>,
    /// Set by [`Session::close`]. Stops a tool call that is still running from
    /// launching a browser the process is about to abandon.
    ///
    /// Only ever read or written while holding the `live` lock, which is what makes
    /// `Relaxed` sufficient and the check race-free.
    closed: AtomicBool,
    /// Set while an interactive login owns the profile. Same lock discipline as
    /// `closed`: only touched under the `live` lock.
    login_in_progress: AtomicBool,
    /// Next value for `Live::generation`.
    next_generation: AtomicU64,
    /// `X-K-Build-Number`. Learned from any `/kr-api/` response's `k-ruoka-build`
    /// header, including the 409 we get for not having sent it yet.
    build: Mutex<Option<String>>,
    /// Keeps request volume well below ordinary browsing, whatever the caller does.
    limiter: RateLimiter,
}

impl Session {
    pub fn new(profile: impl Into<PathBuf>, mode: LaunchMode) -> Result<Self> {
        let profile = profile.into();
        ensure_private_dir(&profile)?;
        Ok(Self {
            profile,
            mode,
            user_agent: std::sync::OnceLock::new(),
            live: Mutex::new(None),
            closed: AtomicBool::new(false),
            login_in_progress: AtomicBool::new(false),
            next_generation: AtomicU64::new(0),
            build: Mutex::new(None),
            limiter: RateLimiter::new(min_request_interval()),
        })
    }

    /// The User-Agent this session presents to Cloudflare.
    ///
    /// Exposed because it is the single load-bearing fact here: a string
    /// containing `HeadlessChrome` is blocked outright, one without it is served the
    /// real shop. The spike prints it so a reader can see *why* the page loaded, and
    /// so a regression in `user_agent()` shows up as evidence rather than as a bare
    /// "Cloudflare blocked us".
    pub fn user_agent(&self) -> Result<&str> {
        if let Some(ua) = self.user_agent.get() {
            return Ok(ua);
        }
        // Two callers racing here both derive the same string, and `get_or_init` keeps
        // whichever lands first. Failure is deliberately not cached: a missing Chrome is
        // worth retrying once the user installs one.
        let derived = user_agent()?;
        Ok(self.user_agent.get_or_init(|| derived))
    }

    /// Seed or clear the cached `X-K-Build-Number`.
    ///
    /// Normally it is learned automatically from any `/kr-api/` response header.
    /// This exists so the stale-build retry can be exercised deliberately (see
    /// `probe --build=<value>`) rather than only on a cold start.
    pub async fn set_build(&self, build: Option<String>) {
        *self.build.lock().await = build;
    }

    /// Launch the browser and park it on the shop page with Cloudflare cleared.
    /// Idempotent; a session that is already live and still on k-ruoka.fi is left
    /// alone, because relaunching per call would be slow and would fight over the
    /// profile's single-instance lock.
    async fn ensure_live(&self) -> Result<(), ApiError> {
        let mut guard = self.live.lock().await;
        self.refuse_if_unavailable()?;
        if let Some(live) = guard.as_ref() {
            // Cheap liveness probe: a dead browser fails this, a live one on the
            // wrong URL just needs re-navigating.
            match live.page.url().await {
                // Prefix, not `contains`. A substring test accepts anything merely
                // *mentioning* the domain -- `login.kesko.fi/?redirect=k-ruoka.fi` is
                // the realistic one, since signing in goes exactly there. That would
                // skip the re-navigation and then run the same-origin fetch against
                // the wrong origin, surfacing as an inexplicable HTML 404.
                Ok(Some(url)) if url.starts_with(SHOP_ORIGIN) => return Ok(()),
                Ok(_) => {
                    // Tear the browser down if re-navigating fails. Leaving a blocked
                    // one in the slot is what broke the relaunch: `attempt_once` has
                    // no generation to attribute the failure to, so `relaunch` would
                    // find a browser it could not match and no-op, spending the one
                    // permitted retry on nothing. Matches the fresh-launch path below.
                    if let Err(e) = navigate_and_clear(&live.page).await {
                        if let Some(dead) = guard.take() {
                            dead.shutdown().await;
                        }
                        return Err(e);
                    }
                    return Ok(());
                }
                Err(_) => {
                    // Browser is gone; fall through and relaunch.
                    if let Some(dead) = guard.take() {
                        dead.handler.abort();
                    }
                }
            }
        }

        let live = self.launch().await.map_err(ApiError::Other)?;
        if let Err(e) = navigate_and_clear(&live.page).await {
            live.shutdown().await;
            return Err(e);
        }
        *guard = Some(live);
        Ok(())
    }

    async fn launch(&self) -> Result<Live> {
        let mut builder = BrowserConfig::builder()
            .chrome_executable(chrome_path())
            .user_data_dir(&self.profile)
            .no_sandbox();
        builder = match self.mode {
            LaunchMode::Headless => builder.new_headless_mode(),
            LaunchMode::Headful { debug_port } => builder.with_head().port(debug_port),
        };
        let config = builder
            .disable_default_args()
            .args(CHROME_ARGS.iter().copied())
            .arg(format!("user-agent={}", self.user_agent()?))
            .window_size(1440, 900)
            .build()
            .map_err(|e| anyhow::anyhow!("building BrowserConfig: {e}"))?;

        let (browser, mut handler) = Browser::launch(config).await.with_context(|| {
            format!(
                "launching Chrome against profile {}. A Chrome left over from an \
                 earlier run can hold the profile's lock -- check for one \
                 (pkill -f {}) before considering the profile itself broken, because \
                 it holds your login and re-running `login` is the only way back.",
                self.profile.display(),
                self.profile.display()
            )
        })?;
        let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });
        let page = browser.new_page("about:blank").await?;
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        // Same reasoning as the retry lines: a relaunch that silently no-ops is
        // otherwise indistinguishable from one that worked, and that is exactly how
        // the generation-sentinel bug survived. "relaunching..." with no launch after
        // it is now visibly a lie.
        eprintln!("k-ruoka-mcp: launched browser generation {generation}");
        Ok(Live {
            browser,
            page,
            handler,
            generation,
        })
    }

    /// The page to run a fetch on, plus the browser generation it belongs to.
    /// Launches if needed.
    ///
    /// The slot can legitimately be empty by the time the lock is re-acquired: a
    /// concurrent [`Session::close`] is exactly the interleaving `refuse_if_closed`
    /// handles in the other order. This used to `expect`, which turned that ordering
    /// into a panic inside a spawned tool task -- and asserted an invariant that
    /// `close` can break by design.
    async fn current_page(&self) -> Result<(Page, u64), ApiError> {
        self.ensure_live().await?;
        let guard = self.live.lock().await;
        let live = guard.as_ref().ok_or_else(closed_underneath_us)?;
        Ok((live.page.clone(), live.generation))
    }

    /// Open an additional tab that this `Session` does not manage.
    ///
    /// `login` needs this. The session's own page is where API calls run, and
    /// `ensure_live` will navigate it back to the shop whenever it finds it on
    /// another origin. Signing in goes via `login.kesko.fi`, a different origin --
    /// so if the human were driving the session's page, the poller would yank it
    /// back to `/kauppa` every few seconds, mid-login. Give them their own tab.
    pub async fn open_extra_page(&self, url: &str) -> Result<Page> {
        self.ensure_live().await?;
        let guard = self.live.lock().await;
        // See `current_page`: a concurrent `close` can empty the slot legitimately.
        let browser = &guard.as_ref().ok_or_else(closed_underneath_us)?.browser;
        Ok(browser.new_page(url).await?)
    }

    /// The session's own page, for callers that need to poke at the DOM.
    pub async fn with_page<T, F, Fut>(&self, f: F) -> Result<T>
    where
        F: FnOnce(Page) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        self.ensure_live().await?;
        let page = {
            let guard = self.live.lock().await;
            // See `current_page`: a concurrent `close` can empty the slot legitimately.
            guard
                .as_ref()
                .ok_or_else(closed_underneath_us)?
                .page
                .clone()
        };
        f(page).await
    }

    /// Tear down the browser so Chrome flushes cookies into the profile.
    ///
    /// Dropping it instead kills the process, and an unflushed profile is exactly
    /// how a login silently fails to persist.
    pub async fn close(&self) -> Result<()> {
        let mut guard = self.live.lock().await;
        // Before the teardown, so a tool call still in flight cannot slip a fresh
        // launch in behind us. On the signal path `serve` calls this and then
        // `std::process::exit(0)`, which runs no destructors -- so a Chrome launched
        // after this point is simply orphaned, holding the profile's SingletonLock
        // and making the next `serve` fail to launch.
        self.closed.store(true, Ordering::Relaxed);
        if let Some(live) = guard.take() {
            live.shutdown().await;
        }
        Ok(())
    }

    /// Hand the profile over to an interactive login, and stop serving until it is done.
    ///
    /// A profile directory supports exactly one Chrome (`SingletonLock`), so a headful
    /// login browser cannot coexist with this session's headless one. Rather than teach
    /// `serve` to run headful, it lets go entirely: shut the browser down, refuse to
    /// launch another, and let the login process own the profile meanwhile. The login
    /// writes its cookies there, and the next tool call relaunches into them.
    pub async fn release_for_login(&self) -> Result<(), ApiError> {
        let mut guard = self.live.lock().await;
        self.refuse_if_unavailable()?;
        // Set before the teardown so a concurrent tool call cannot relaunch into the gap.
        self.login_in_progress.store(true, Ordering::Relaxed);
        if let Some(live) = guard.take() {
            live.shutdown().await;
        }
        Ok(())
    }

    /// Start serving again after [`Session::release_for_login`], whatever the outcome.
    ///
    /// Deliberately does not relaunch: the browser comes back lazily on the next tool
    /// call, which is also when a fresh login would be picked up.
    pub async fn resume_after_login(&self) {
        let _guard = self.live.lock().await;
        self.login_in_progress.store(false, Ordering::Relaxed);
    }

    /// Must be called while holding the `live` lock.
    fn refuse_if_unavailable(&self) -> Result<(), ApiError> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(closed_underneath_us());
        }
        if self.login_in_progress.load(Ordering::Relaxed) {
            return Err(ApiError::InvalidRequest(
                "a login is in progress and it owns the browser profile until it \
                 finishes. Call login_status to see how it is going, or cancel_login to \
                 give up, then retry."
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// Relaunch against the same profile directory. This is the Cloudflare-block
    /// remedy. It deliberately does not delete anything: the profile holds a real
    /// credential, so it must survive a block rather than being cleared.
    /// Replace the browser generation `blocked` -- unless someone already has.
    /// `None` means the caller had no browser to blame, so anything currently in the
    /// slot is by definition newer and there is nothing to do.
    ///
    /// Holds the lock across the whole teardown-and-relaunch so two callers cannot
    /// interleave, and no-ops when the current browser is already newer than the
    /// one that got blocked. Together those turn a wave of N concurrent blocked
    /// calls into exactly one relaunch.
    async fn relaunch(&self, blocked: Option<u64>) -> Result<(), ApiError> {
        let mut guard = self.live.lock().await;
        self.refuse_if_unavailable()?;
        if !should_replace(guard.as_ref().map(|live| live.generation), blocked) {
            return Ok(());
        }
        if let Some(mut dead) = guard.take() {
            dead.browser.close().await.ok();
            dead.browser.wait().await.ok();
            dead.handler.abort();
        }
        let live = self.launch().await.map_err(ApiError::Other)?;
        if let Err(e) = navigate_and_clear(&live.page).await {
            live.shutdown().await;
            return Err(e);
        }
        *guard = Some(live);
        Ok(())
    }

    /// Call a `/kr-api/` endpoint from inside the page, retrying the two failures
    /// that are known to be recoverable.
    pub async fn api(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, ApiError> {
        let mut relaunched = false;
        let mut refreshed_build = false;
        let relaunch_would_hurt = relaunch_costs_a_human_their_login(self.mode);
        loop {
            let (generation, result) = self.attempt_once(method, path, body).await;
            let error = match result {
                Ok(value) => return Ok(value),
                Err(e) => e,
            };
            match plan_recovery(&error, relaunched || relaunch_would_hurt, refreshed_build) {
                Recovery::RefreshBuild(build) => {
                    refreshed_build = true;
                    // stderr is free -- `serve` owns stdout for JSON-RPC. Retries
                    // are invisible otherwise, which makes a green test that never
                    // actually exercised one indistinguishable from a real pass.
                    eprintln!("k-ruoka-mcp: build number was stale, retrying with {build}");
                    *self.build.lock().await = Some(build);
                }
                Recovery::Relaunch => {
                    relaunched = true;
                    eprintln!(
                        "k-ruoka-mcp: {error}; relaunching the browser against the \
                         same profile (never deleting it) and retrying once"
                    );
                    self.relaunch(generation).await?;
                }
                Recovery::GiveUp => return Err(error),
            }
        }
    }

    /// One attempt: get a page, then make the request on it.
    ///
    /// Returns a tuple rather than a `Result` **on purpose**. Getting the page can
    /// fail with a Cloudflare block -- that is where a refused page load is detected,
    /// and it is the only trigger for the relaunch branch ever observed -- so this
    /// signature is what stops a future `?` from routing that failure past the retry
    /// loop. It has happened twice, by two different routes, both times
    /// while the classification itself was fully unit-tested. With no `Result` to
    /// return early from, the mistake is no longer expressible here.
    ///
    /// The generation is the browser incarnation the attempt used, so a block can be
    /// attributed to it and a relaunch can no-op if someone else already replaced it.
    /// `None` means there was no live browser to attribute the failure to -- which is
    /// deliberately *not* a number, because every number is a real generation. It was
    /// spelled `0` once, and since `next_generation` starts at 0 that is the first
    /// browser: after a single relaunch, `relaunch(0)` compared 1 against 0, decided
    /// someone else had already replaced the browser, and returned having done
    /// nothing -- while the "relaunching" line had already gone to stderr. The retry
    /// was then spent on the same blocked browser.
    async fn attempt_once(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> (Option<u64>, Result<serde_json::Value, ApiError>) {
        match self.current_page().await {
            Ok((page, generation)) => (
                Some(generation),
                self.api_once(&page, method, path, body).await,
            ),
            Err(e) => (None, Err(e)),
        }
    }

    async fn api_once(
        &self,
        page: &Page,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, ApiError> {
        let build = self.build.lock().await.clone();

        // Immediately before the request leaves, so a retry is spaced from the attempt
        // it is retrying rather than firing straight back at a server that just refused.
        self.limiter.acquire().await;

        let expr = fetch_script(method, path, body, build.as_deref());
        let value = evaluate(page, &expr).await.map_err(ApiError::Other)?;
        let raw: RawResponse = serde_json::from_value(value)
            .context("unexpected shape from the in-page fetch helper")
            .map_err(ApiError::Other)?;

        // Learn the build number from any response that carries it, so the very
        // first call of a process bootstraps itself off its own 409.
        if let Some(b) = &raw.build {
            let mut slot = self.build.lock().await;
            if slot.as_deref() != Some(b.as_str()) {
                *slot = Some(b.clone());
            }
        }

        classify(raw)
    }
}

/// Turn a raw response into either a parsed body or a typed failure.
///
/// The discriminator, established empirically: Cloudflare answers with
/// a `cf-mitigated` header or an HTML body, while the application answers with
/// JSON even when it is refusing. A 403 with a JSON body is the app; a 409 with a
/// JSON body is the app; an HTML body is not.
fn classify(raw: RawResponse) -> Result<serde_json::Value, ApiError> {
    let looks_html = raw
        .content_type
        .as_deref()
        .is_some_and(|c| c.contains("text/html"))
        || raw.body.trim_start().starts_with('<');

    // "HTML body" alone is too loose a test: K-Ruoka serves an ordinary HTML 404
    // for an unknown /kr-api/ path, and calling that a block would send us into a
    // pointless browser relaunch. Require an actual Cloudflare fingerprint --
    // either the header, a known challenge marker, or one of the statuses
    // Cloudflare itself serves.
    let challenge_marker = cloudflare_marker(&raw.body).is_some();
    // Statuses where an *unmarked* HTML body is most likely an edge block and where
    // relaunching is actually the right remedy. 429 is deliberately excluded: a
    // relaunch does not fix rate limiting, and retrying straight away makes it
    // worse, so that should surface as a plain API error instead.
    let cf_status = matches!(raw.status, 403 | 503);

    // Gated on non-2xx deliberately. A successful basket response embeds a
    // multi-KB `productDetails` blob per item -- marketing copy, category names --
    // and a challenge marker appearing in that free text would otherwise turn a
    // perfectly good 200 into a bogus block plus a pointless browser relaunch.
    let success = (200..300).contains(&raw.status);
    if raw.cf_mitigated.is_some() || (!success && (challenge_marker || (looks_html && cf_status))) {
        let mitigated = raw
            .cf_mitigated
            .as_deref()
            .map(|m| format!(", cf-mitigated: {m}"))
            .unwrap_or_default();
        return Err(ApiError::Cloudflare {
            detail: format!("API response, status {}{mitigated}", raw.status),
        });
    }

    let message = serde_json::from_str::<ApiErrorBody>(&raw.body)
        .ok()
        .map(|b| b.error.message);

    if (200..300).contains(&raw.status) {
        return serde_json::from_str(&raw.body)
            .context("K-Ruoka returned a success status with a body that is not JSON")
            .map_err(ApiError::Other);
    }

    // Both of these are "<something> - reload" messages with near-identical shape
    // and completely different meanings, so match on them before falling back to
    // the status code.
    if let Some(msg) = &message {
        if msg.contains("Client version is too old") {
            return Err(ApiError::StaleBuild { wanted: raw.build });
        }
        if msg.contains("Token renewal error") {
            return Err(ApiError::AuthExpired);
        }
    }
    if raw.status == 401 {
        return Err(ApiError::AuthExpired);
    }
    Err(ApiError::Api {
        status: raw.status,
        message: message.unwrap_or_else(|| raw.body.chars().take(300).collect()),
    })
}

/// Build the in-page `fetch` call.
///
/// Doing the request inside the page rather than from Rust is what makes it
/// same-origin, so the browser attaches the session and Cloudflare cookies itself
/// and we never handle them by hand.
fn fetch_script(
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
    build: Option<&str>,
) -> String {
    let body_js = match body {
        Some(b) => format!("JSON.stringify({b})"),
        None => "undefined".to_string(),
    };
    format!(
        r#"(async () => {{
             const headers = {{ 'Accept': 'application/json' }};
             const build = {build};
             if (build) headers['X-K-Build-Number'] = build;
             const body = {body_js};
             if (body !== undefined) headers['Content-Type'] = 'application/json';
             const r = await fetch({path}, {{
               method: {method},
               headers,
               body,
               credentials: 'include',
             }});
             return {{
               status: r.status,
               build: r.headers.get('k-ruoka-build'),
               cfMitigated: r.headers.get('cf-mitigated'),
               contentType: r.headers.get('content-type'),
               body: await r.text(),
             }};
           }})()"#,
        build = serde_json::json!(build),
        path = serde_json::json!(path),
        method = serde_json::json!(method),
    )
}

pub(crate) async fn evaluate(page: &Page, expr: &str) -> Result<serde_json::Value> {
    let params = EvaluateParams::builder()
        .expression(expr)
        .await_promise(true)
        .return_by_value(true)
        .build()
        .map_err(|e| anyhow::anyhow!("building EvaluateParams: {e}"))?;
    let result = match page.evaluate(params).await {
        Ok(r) => r,
        Err(CdpError::Timeout) => anyhow::bail!("page evaluation timed out"),
        Err(e) => return Err(e.into()),
    };
    Ok(result.value().cloned().unwrap_or(serde_json::Value::Null))
}

/// Navigate to the shop and wait for Cloudflare, polling rather than sleeping a
/// fixed duration -- a fixed sleep makes failures indistinguishable from slowness.
async fn navigate_and_clear(page: &Page) -> Result<(), ApiError> {
    page.goto(SHOP_URL)
        .await
        .map_err(|e| ApiError::Other(anyhow::anyhow!("navigating to {SHOP_URL}: {e}")))?;
    let deadline = Instant::now() + CLEARANCE_TIMEOUT;
    loop {
        // Ask for the origin as well as the text: readiness is "we are on
        // k-ruoka.fi and nothing is blocking us", not "the SPA has finished
        // hydrating". The same-origin `fetch` only needs the document's origin.
        let probe = evaluate(
            page,
            "({ origin: location.origin, text: document.body ? document.body.innerText : null })",
        )
        .await
        .unwrap_or(serde_json::Value::Null);
        let origin = probe["origin"].as_str().unwrap_or_default();
        let text = probe["text"].as_str();

        // A refusal is terminal for this attempt. Typed as Cloudflare so it gets
        // the relaunch rather than surfacing as a bare failure.
        if let Some(marker) = text.and_then(|t| first_marker(t, BLOCK_MARKERS)) {
            return Err(ApiError::Cloudflare {
                detail: format!(
                    "page load rejected ({marker:?}) -- the browser fingerprint is being \
                     refused; a UA containing HeadlessChrome is the usual cause"
                ),
            });
        }

        // A challenge, by contrast, is just "not yet": real Chrome executes it and
        // it clears on its own. Keep polling until the deadline.
        let challenged = text.is_some_and(|t| first_marker(t, CHALLENGE_MARKERS).is_some());

        // Deliberately not `text.contains("Tuotteet")`. Keying readiness off two
        // Finnish nav labels made every API call depend on third-party UI copy for
        // no benefit: a label rename, an A/B test or a locale flip would have cost
        // 45s of polling and then been misreported as a Cloudflare block. The
        // same-origin fetch only needs the document's origin.
        if !challenged && origin == SHOP_ORIGIN && text.is_some_and(|t| !t.trim().is_empty()) {
            return Ok(());
        }
        if Instant::now() > deadline {
            // A challenge that never resolves is also bot mitigation, just a
            // quieter form of it, and the same remedy applies.
            return Err(ApiError::Cloudflare {
                detail: format!(
                    "challenge did not clear within {}s",
                    CLEARANCE_TIMEOUT.as_secs()
                ),
            });
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Where Chrome usually lives, per platform. `K_RUOKA_CHROME` overrides.
///
/// First existing entry wins; if none exist the first is returned anyway, so the failure
/// names a concrete path.
const CHROME_CANDIDATES: &[&str] = if cfg!(target_os = "macos") {
    &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ]
} else if cfg!(target_os = "windows") {
    &[
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    ]
} else {
    &[
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/snap/bin/chromium",
    ]
};

fn chrome_path() -> String {
    if let Ok(explicit) = std::env::var("K_RUOKA_CHROME") {
        return explicit;
    }
    CHROME_CANDIDATES
        .iter()
        .find(|candidate| Path::new(candidate).is_file())
        .unwrap_or(&CHROME_CANDIDATES[0])
        .to_string()
}

/// The installed Chrome's own UA with the `HeadlessChrome` token normalised away.
///
/// This is the *only* thing standing between us and a Cloudflare
/// block -- no stealth patching needed. It is derived at runtime rather than
/// hardcoded so it cannot drift from the actual browser at the next Chrome
/// update, and so headful `login` and headless `serve` present byte-identical
/// strings. That last part is load-bearing: `cf_clearance` is UA-bound, and the
/// handoff between the two modes only works because the strings match.
fn user_agent() -> Result<String> {
    // Escape hatch for when the derived string stops working (Chrome changes its
    // format, or Cloudflare starts wanting client hints to match). Also the only
    // way to deliberately provoke a block, which is how the Cloudflare recovery
    // path gets exercised at all.
    if let Ok(ua) = std::env::var("K_RUOKA_USER_AGENT") {
        return Ok(ua);
    }
    let version = chrome_version()?;
    Ok(format!(
        "Mozilla/5.0 ({UA_PLATFORM}) AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/{version} Safari/537.36"
    ))
}

/// The platform token real Chrome puts in its UA here.
///
/// Must match the actual OS: Chrome also sends `sec-ch-ua-platform`, which we do not
/// control, and UA consistency is the one thing Cloudflare cares about here.
/// `10_15_7` is Chrome's own frozen value on macOS, Apple Silicon included.
const UA_PLATFORM: &str = if cfg!(target_os = "macos") {
    "Macintosh; Intel Mac OS X 10_15_7"
} else if cfg!(target_os = "windows") {
    "Windows NT 10.0; Win64; x64"
} else {
    "X11; Linux x86_64"
};

/// The installed Chrome's version number, e.g. `150.0.7871.181`.
fn chrome_version() -> Result<String> {
    let path = chrome_path();

    // Not asked on Windows at all. `chrome.exe --version` there does not print a version
    // and does not exit, so `output()` waits for a process that never finishes: this hung
    // startup outright rather than falling through to the directory read below.
    #[cfg(not(windows))]
    if let Ok(out) = std::process::Command::new(&path).arg("--version").output()
        && let Ok(stdout) = String::from_utf8(out.stdout)
        && let Some(version) = first_version_token(&stdout)
    {
        return Ok(version);
    }

    // Chrome keeps a version-named directory next to the executable. On Windows this is
    // the only way; elsewhere it is the fallback.
    if let Some(version) = version_from_install_dir(Path::new(&path)) {
        return Ok(version);
    }

    anyhow::bail!(
        "could not determine the Chrome version from `{path} --version` or from the \
         install directory. Set K_RUOKA_USER_AGENT to a full User-Agent string to bypass \
         this, or K_RUOKA_CHROME if that path is wrong."
    )
}

fn first_version_token(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|token| {
            token.chars().next().is_some_and(|c| c.is_ascii_digit()) && token.contains('.')
        })
        .map(str::to_string)
}

/// A `150.0.7871.181`-shaped sibling directory of the executable.
fn version_from_install_dir(exe: &Path) -> Option<String> {
    let dir = exe.parent()?;
    let mut best: Option<String> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // Four dot-separated numbers: Chrome's scheme, and unlikely to collide.
        let looks_like_a_version = name.split('.').count() == 4
            && name
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
        if looks_like_a_version && best.as_deref() < Some(name.as_str()) {
            best = Some(name);
        }
    }
    best
}

/// The profile holds a live login. Treat it like a credential: 0700, and refuse
/// to use it if it is readable by anyone else.
#[cfg(unix)]
fn ensure_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path)
        .with_context(|| format!("creating profile dir {}", path.display()))?;
    let mut perms = std::fs::metadata(path)?.permissions();
    if perms.mode() & 0o077 != 0 {
        perms.set_mode(0o700);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("tightening permissions on {}", path.display()))?;
    }
    Ok(())
}

/// No mode bits on Windows. `%LOCALAPPDATA%` is already per-user and inherits an ACL
/// that excludes others; the gap is that a `K_RUOKA_PROFILE` pointed somewhere
/// world-readable goes unwarned. Fixing that properly needs the Windows API.
#[cfg(windows)]
fn ensure_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("creating profile dir {}", path.display()))?;
    Ok(())
}

/// Where the login is stored, per platform convention. `K_RUOKA_PROFILE` overrides, which
/// is how the tests and the spike get a scratch profile instead of the real login.
pub fn default_profile_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("K_RUOKA_PROFILE") {
        return Ok(PathBuf::from(dir));
    }
    Ok(platform_data_dir()?.join("k-ruoka-mcp/profile"))
}

#[cfg(target_os = "linux")]
fn platform_data_dir() -> Result<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("neither XDG_DATA_HOME nor HOME is set")
}

#[cfg(target_os = "macos")]
fn platform_data_dir() -> Result<PathBuf> {
    // Honour a deliberate XDG_DATA_HOME, else the platform convention.
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
        })
        .context("HOME is not set")
}

#[cfg(windows)]
fn platform_data_dir() -> Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData/Local")))
        .context("neither LOCALAPPDATA nor USERPROFILE is set")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(status: u16, body: &str, ct: &str, cf: Option<&str>) -> RawResponse {
        RawResponse {
            status,
            build: Some("31844".into()),
            cf_mitigated: cf.map(str::to_string),
            content_type: Some(ct.into()),
            body: body.into(),
        }
    }

    const JSON: &str = "application/json; charset=utf-8";

    #[test]
    fn success_returns_parsed_body() {
        let v = classify(raw(200, r#"{"id":"abc"}"#, JSON, None)).unwrap();
        assert_eq!(v["id"], "abc");
    }

    #[test]
    fn stale_build_is_recoverable_and_carries_the_wanted_value() {
        let body = r#"{"error":{"message":"Client version is too old - reload"}}"#;
        match classify(raw(409, body, JSON, None)) {
            Err(ApiError::StaleBuild { wanted }) => assert_eq!(wanted.as_deref(), Some("31844")),
            other => panic!("expected StaleBuild, got {other:?}"),
        }
    }

    /// The two "- reload" messages look alike and mean opposite things: one is a
    /// retry, the other must never retry or touch the profile.
    #[test]
    fn token_renewal_is_auth_expiry_not_a_stale_build() {
        let body = r#"{"error":{"message":"Token renewal error - reload"}}"#;
        assert!(matches!(
            classify(raw(409, body, JSON, None)),
            Err(ApiError::AuthExpired)
        ));
        assert!(matches!(
            classify(raw(401, "{}", JSON, None)),
            Err(ApiError::AuthExpired)
        ));
    }

    #[test]
    fn html_body_or_cf_header_is_a_cloudflare_block() {
        let html = "<!DOCTYPE html><title>Just a moment...</title>";
        assert!(matches!(
            classify(raw(403, html, "text/html", None)),
            Err(ApiError::Cloudflare { .. })
        ));
        // The `cf-mitigated` header alone is enough, even with a JSON body.
        match classify(raw(403, "{}", JSON, Some("challenge"))) {
            Err(ApiError::Cloudflare { detail }) => {
                assert!(detail.contains("cf-mitigated: challenge"), "{detail}")
            }
            other => panic!("expected Cloudflare, got {other:?}"),
        }
    }

    /// Relaunching does not fix rate limiting, and retrying immediately makes it
    /// worse, so a 429 must not take the Cloudflare branch.
    #[test]
    fn html_429_is_not_treated_as_a_relaunchable_block() {
        let html = "<html><body>Too many requests</body></html>";
        assert!(matches!(
            classify(raw(429, html, "text/html", None)),
            Err(ApiError::Api { status: 429, .. })
        ));
    }

    /// Anonymous calls to auth-only endpoints (`/kr-api/user/...`, `/kr-api/cards/getAll`,
    /// `/kr-api/v2/shoppinghistory`) were all observed returning 401 live, and all
    /// classified here -- so this branch is verified, not merely written.
    #[test]
    fn bare_401_is_auth_expiry() {
        assert!(matches!(
            classify(raw(401, "", "application/json", None)),
            Err(ApiError::AuthExpired)
        ));
    }

    /// A successful basket embeds a multi-KB `productDetails` blob of marketing and
    /// category text per item. A challenge marker turning up in that free text must
    /// not turn a good 200 into a bogus block and a pointless browser relaunch.
    #[test]
    fn a_2xx_whose_body_mentions_a_challenge_marker_is_not_a_block() {
        let body = r#"{"id":"b1","items":[{"id":"1","name":{"finnish":"Just a moment"}}]}"#;
        let v = classify(raw(200, body, JSON, None)).expect("a 200 must stay a success");
        assert_eq!(v["id"], "b1");
    }

    /// Observed live: an unknown `/kr-api/` path returns an ordinary HTML 404.
    /// Treating that as a block would trigger a useless browser relaunch.
    #[test]
    fn html_404_is_not_a_cloudflare_block() {
        let html = "<!DOCTYPE html><html><body>Not found</body></html>";
        assert!(matches!(
            classify(raw(404, html, "text/html", None)),
            Err(ApiError::Api { status: 404, .. })
        ));
    }

    /// A JSON 4xx is the application refusing, and must not trigger a relaunch.
    #[test]
    fn json_error_is_an_api_error_not_a_block() {
        let body = r#"{"error":{"message":"Basket not found"}}"#;
        match classify(raw(404, body, JSON, None)) {
            Err(ApiError::Api { status, message }) => {
                assert_eq!(status, 404);
                assert_eq!(message, "Basket not found");
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    /// A challenge is transient and must be waited out during a page load, not
    /// treated as a refusal -- letting real Chrome clear it is the whole design.
    /// In an API *response* it is still a failure for that request.
    #[test]
    fn a_challenge_is_distinguished_from_a_refusal() {
        assert!(first_marker("Just a moment...", CHALLENGE_MARKERS).is_some());
        assert!(first_marker("Just a moment...", BLOCK_MARKERS).is_none());

        assert!(first_marker("Pyyntö estetty (CF/WB)", BLOCK_MARKERS).is_some());
        assert!(first_marker("Pyyntö estetty (CF/WB)", CHALLENGE_MARKERS).is_none());

        // Both are Cloudflare as far as response classification goes.
        assert!(cloudflare_marker("Just a moment...").is_some());
        assert!(cloudflare_marker("Pyyntö estetty (CF/WB)").is_some());
        // The real shop page is neither.
        assert!(cloudflare_marker("Tuotteet Kaupat Reseptit Ostoskori").is_none());
    }

    fn cf() -> ApiError {
        ApiError::Cloudflare {
            detail: "blocked".into(),
        }
    }

    /// A Cloudflare block gets exactly one relaunch, from either failure site.
    #[test]
    fn a_cloudflare_block_is_relaunched_once_then_given_up_on() {
        assert_eq!(plan_recovery(&cf(), false, false), Recovery::Relaunch);
        assert_eq!(plan_recovery(&cf(), true, false), Recovery::GiveUp);
    }

    #[test]
    fn a_stale_build_is_refreshed_once_then_given_up_on() {
        let stale = ApiError::StaleBuild {
            wanted: Some("31844".into()),
        };
        assert_eq!(
            plan_recovery(&stale, false, false),
            Recovery::RefreshBuild("31844".into())
        );
        assert_eq!(plan_recovery(&stale, false, true), Recovery::GiveUp);
    }

    /// Cold start sends no build header at all, which is the common way the
    /// stale-build retry actually fires -- not a K-Ruoka deploy. Without a value
    /// there is nothing to heal with, and writing `None` through would clobber a
    /// known-good build number for the rest of the process.
    #[test]
    fn a_stale_build_carrying_no_value_is_not_retryable() {
        let e = ApiError::StaleBuild { wanted: None };
        assert_eq!(plan_recovery(&e, false, false), Recovery::GiveUp);
    }

    /// The first request must not be delayed -- a person waiting on a cart read should
    /// not pay for a limit that exists to stop bursts.
    #[tokio::test]
    async fn the_first_request_is_not_delayed() {
        let limiter = RateLimiter::new(Duration::from_millis(500));
        let start = Instant::now();
        limiter.acquire().await;
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "{:?}",
            start.elapsed()
        );
    }

    /// The property that matters: concurrent callers queue instead of firing together.
    /// rmcp dispatches tool calls in parallel, so this is the realistic shape.
    #[tokio::test]
    async fn concurrent_callers_are_spaced_out_not_batched() {
        let limiter = std::sync::Arc::new(RateLimiter::new(Duration::from_millis(50)));
        let start = Instant::now();
        let mut handles = Vec::new();
        for _ in 0..4 {
            let limiter = std::sync::Arc::clone(&limiter);
            handles.push(tokio::spawn(async move { limiter.acquire().await }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        // Four requests, three gaps: the last cannot have gone out before 150 ms.
        assert!(
            start.elapsed() >= Duration::from_millis(150),
            "went out as a burst: {:?}",
            start.elapsed()
        );
    }

    /// Zero has to mean off rather than "sleep for zero", so the live suites can opt out.
    #[tokio::test]
    async fn a_zero_interval_disables_the_limiter() {
        let limiter = RateLimiter::new(Duration::ZERO);
        let start = Instant::now();
        for _ in 0..20 {
            limiter.acquire().await;
        }
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "{:?}",
            start.elapsed()
        );
    }

    /// `serve` must be able to recover from a block; `login` must not, because the
    /// browser is the window someone is typing a password into.
    #[test]
    fn only_headless_may_relaunch_out_from_under_the_browser() {
        assert!(!relaunch_costs_a_human_their_login(LaunchMode::Headless));
        assert!(relaunch_costs_a_human_their_login(LaunchMode::Headful {
            debug_port: 9222
        }));

        // And the flag really does suppress the relaunch once set.
        let block = ApiError::Cloudflare {
            detail: "page load rejected".into(),
        };
        assert_eq!(plan_recovery(&block, false, false), Recovery::Relaunch);
        assert_eq!(plan_recovery(&block, true, false), Recovery::GiveUp);
    }

    /// The de-duplication that turns a wave of N blocked calls into one relaunch.
    #[test]
    fn only_the_browser_that_got_blocked_is_replaced() {
        // The blocked browser is still live: this caller is the one that should act.
        assert!(should_replace(Some(7), Some(7)));
        // Someone else already replaced it, so doing it again would tear down the
        // browser they just built. This is the whole point of the generation.
        assert!(!should_replace(Some(8), Some(7)));
        // Nothing live: launch, whoever was blocked.
        assert!(should_replace(None, Some(7)));
        assert!(should_replace(None, None));
    }

    /// The regression this signature exists to prevent, pinned.
    ///
    /// "No browser to blame" used to be spelled `0`. Since generations start at 0 that
    /// is the *first* browser, so after a single relaunch the comparison was 1 against
    /// 0 -- read as "someone else already replaced it" -- and the relaunch silently
    /// did nothing, having already announced itself on stderr. The retry was then
    /// spent against the same blocked browser.
    ///
    /// With `Option` the two cases cannot be confused: generation 0 is a browser,
    /// `None` is the absence of one, and they behave differently here.
    #[test]
    fn no_browser_to_blame_is_not_the_same_as_generation_zero() {
        // The old sentinel, read as a real generation: correctly declines, because
        // generation 0 really has been superseded by 1.
        assert!(!should_replace(Some(1), Some(0)));
        // What that case actually meant. Nothing live is the state `ensure_live` now
        // leaves behind when navigation is blocked, so the relaunch does happen.
        assert!(should_replace(None, None));
        // And generation 0 is an ordinary generation in every other respect.
        assert!(should_replace(Some(0), Some(0)));
    }

    /// Retrying cannot fix an expired session, and the profile must not be touched
    /// over one. This is the distinction the whole error enum exists for.
    #[test]
    fn auth_expiry_and_plain_api_errors_are_never_retried() {
        assert_eq!(
            plan_recovery(&ApiError::AuthExpired, false, false),
            Recovery::GiveUp
        );
        assert_eq!(
            plan_recovery(
                &ApiError::Api {
                    status: 404,
                    message: "nope".into()
                },
                false,
                false
            ),
            Recovery::GiveUp
        );
        assert_eq!(
            plan_recovery(&ApiError::InvalidRequest("bad".into()), false, false),
            Recovery::GiveUp
        );
    }

    #[test]
    fn fetch_script_escapes_its_inputs() {
        let s = fetch_script("POST", "/kr-api/basket/active", None, Some("31844"));
        assert!(s.contains(r#"const build = "31844""#));
        assert!(s.contains(r#"fetch("/kr-api/basket/active""#));
        assert!(s.contains("const body = undefined"));
    }

    /// The body is interpolated into JS source too, and `item_id` reaches it from
    /// caller-supplied input. A quote or backslash must not be able to break out of
    /// the string and become code.
    #[test]
    fn fetch_script_escapes_the_body() {
        let hostile = r#"a"); alert('x'); //\"#;
        let body = serde_json::json!([{ "type": "REMOVE-ITEM", "itemId": hostile }]);
        let s = fetch_script("PATCH", "/kr-api/basket/by-id/1", Some(&body), None);

        // The dangerous characters survive only in escaped form.
        assert!(
            !s.contains(r#"itemId":"a");"#),
            "raw injection present:\n{s}"
        );
        assert!(
            s.contains(r#"\"); alert('x'); //\\"#),
            "not escaped as expected:\n{s}"
        );

        // And the escaped form round-trips back to the original through JSON.
        let line = s
            .lines()
            .find(|l| l.contains("JSON.stringify("))
            .expect("the body line");
        let json = line
            .trim()
            .trim_start_matches("const body = JSON.stringify(")
            .trim_end_matches(';')
            .trim_end_matches(')');
        let parsed: serde_json::Value = serde_json::from_str(json).expect("valid JSON literal");
        assert_eq!(parsed[0]["itemId"], hostile);
    }
}
