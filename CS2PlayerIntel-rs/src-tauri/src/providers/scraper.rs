//! Reads public profile pages in a hidden webview window, one per site, reused for every lookup and
//! closed after a minute without one. Each site gets its own browser profile, shared with the visible
//! window the user can open, so a verification solved there carries over. It never solves challenges
//! or logs in itself, but gives Cloudflare's automatic check time to finish, as a normal browser would.
//! Provider pages get no access to the app: their windows have no capabilities, and page text is read by
//! evaluating a script from the app side.

use super::Failure;
use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_CLOSE: Duration = Duration::from_secs(60);
const GAP: Duration = Duration::from_millis(800);
const PAUSE_MS: i64 = 60_000;
/// After this many lookups in a row without data, the site is paused instead of retried for every player.
const FAILURES_BEFORE_PAUSE: u32 = 3;
const FAILURE_PAUSE_MS: i64 = 15 * 60_000;
const POLL: Duration = Duration::from_millis(400);
const EVAL_TIMEOUT: Duration = Duration::from_secs(3);

const SNAPSHOT_JS: &str = include_str!("page-snapshot.js");
/// Marks the current document before navigating, so snapshots of it are not mistaken for the next page.
const MARK_OLD_JS: &str = r#"(document.documentElement.setAttribute("data-cs2intel-old", "1"), true)"#;

/// Ad and analytics hosts. The stats pages render without them, and they are what keeps a hidden window
/// busy. Consent and Cloudflare scripts are deliberately not listed.
pub const BLOCKED_HOSTS: [&str; 33] = [
    "doubleclick.net",
    "googlesyndication.com",
    "googletagservices.com",
    "google-analytics.com",
    "googletagmanager.com",
    "googleadservices.com",
    "adservice.google.com",
    "amazon-adsystem.com",
    "adnxs.com",
    "criteo.com",
    "criteo.net",
    "taboola.com",
    "outbrain.com",
    "pubmatic.com",
    "rubiconproject.com",
    "openx.net",
    "casalemedia.com",
    "scorecardresearch.com",
    "quantserve.com",
    "moatads.com",
    "hotjar.com",
    "nitropay.com",
    "playwire.com",
    "ezoic.net",
    "media.net",
    "adsrvr.org",
    "3lift.com",
    "sharethrough.com",
    "teads.tv",
    "smartadserver.com",
    "yieldmo.com",
    "id5-sync.com",
    "crwdcntrl.net",
];

/// Browser arguments for provider windows: the defaults Tauri uses, muted audio, blocked ad hosts, and
/// no throttling of the hidden window (otherwise its pages, and Cloudflare's check, barely run).
pub fn browser_args() -> String {
    // CS2INTEL_PROBE_NO_BLOCK: probe runs without blocking, to tell a blocked host from a page change.
    let hosts: &[&str] = if std::env::var_os("CS2INTEL_PROBE_NO_BLOCK").is_some() { &[] } else { &BLOCKED_HOSTS };
    let rules: Vec<String> = hosts.iter().flat_map(|host| [format!("MAP {host} ~NOTFOUND"), format!("MAP *.{host} ~NOTFOUND")]).collect();
    format!(
        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion --disable-background-timer-throttling --disable-renderer-backgrounding --disable-backgrounding-occluded-windows --mute-audio --host-resolver-rules=\"{}\"",
        rules.join(",")
    )
}

static AUTH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"please log ?in to view").unwrap());
static VERIFY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(verify|confirm)(ing)? (that )?you(?: are|'re) (a )?human|needs to review the security of your connection|cloudflare needs to check|checking (if the site connection is secure|your browser)|performing security verification|enable javascript and cookies to continue").unwrap()
});
static LIMITED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"too many requests|you are being rate limited|error 1015").unwrap());

/// One classifier for both the wait loop and the result, so an early exit cannot be mislabelled.
/// Phrases cover login prompts and Cloudflare's challenge texts.
pub fn classify_page(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase().replace(['\u{2018}', '\u{2019}'], "'");
    if AUTH.is_match(&lower) {
        Some("auth-required")
    } else if VERIFY.is_match(&lower) {
        Some("verification-required")
    } else if LIMITED.is_match(&lower) {
        Some("rate-limited")
    } else {
        None
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Page {
    /// Still the previous document (navigation has not committed yet).
    #[serde(default)]
    pub old: bool,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub heading: String,
    #[serde(default)]
    pub consent_required: bool,
}

impl Page {
    fn blocked_reason(&self) -> Option<&'static str> {
        classify_page(&self.text)
            .or_else(|| self.title.trim().trim_end_matches(['.', '\u{2026}']).eq_ignore_ascii_case("just a moment").then_some("verification-required"))
            .or_else(|| self.consent_required.then_some("consent-required"))
    }
}

pub struct ReadOptions<'a> {
    /// Whether the page has rendered its data.
    pub is_ready: &'a (dyn Fn(&str) -> bool + Sync),
    /// Run once until it returns true, for pages that load stats behind a button.
    pub click_script: Option<&'a str>,
    /// Existing provider-specific optional-cookie refusal; never accepts optional cookies.
    pub consent_script: Option<&'a str>,
    /// Whether everything wanted has rendered. A ready page that is not complete gets a few more seconds
    /// (sections can load after the first data), then is returned as it is.
    pub is_complete: Option<&'a (dyn Fn(&str) -> bool + Sync)>,
}

/// Scrolls a step down the page (wrapping to the top), so sections that load when scrolled into view
/// render in the hidden window too.
const SCROLL_STEP_JS: &str = r#"(() => { const end = document.documentElement.scrollHeight - innerHeight; scrollTo(0, scrollY >= end - 5 ? 0 : scrollY + innerHeight * 0.8); return true; })()"#;

const SETTLE: Duration = Duration::from_secs(10);

/// Stops a site from being retried for every player while it cannot answer (signed out, a check that
/// does not clear, a layout change): each failed lookup keeps a browser page busy for half a minute.
#[derive(Default)]
pub struct Breaker {
    failures: u32,
    paused: Option<(i64, Failure)>,
}

impl Breaker {
    /// The answer to give instead of a lookup while paused.
    pub fn check(&mut self, now: i64) -> Option<Failure> {
        match &self.paused {
            Some((until, failure)) if now < *until => Some(failure.clone()),
            Some(_) => {
                self.paused = None;
                None
            }
            None => None,
        }
    }

    pub fn pause(&mut self, until: i64, failure: Failure) {
        self.paused = Some((until, failure));
    }

    pub fn record(&mut self, result: &Result<Page, Failure>, site: &str, now: i64) {
        match result {
            Ok(_) => self.failures = 0,
            Err(failure) if matches!(failure.status.as_str(), "timeout" | "verification-required" | "auth-required" | "consent-required") => {
                self.failures += 1;
                if self.failures >= FAILURES_BEFORE_PAUSE {
                    self.failures = 0;
                    let hint = if failure.status == "consent-required" {
                        "open a profile, choose your cookie preferences, then refresh"
                    } else if failure.status == "auth-required" || site == "CSStats" {
                        "sign in to it from Settings"
                    } else {
                        "open a profile from the details panel"
                    };
                    let reason = format!(
                        "{site} gave no data for {FAILURES_BEFORE_PAUSE} players in a row ({}). Lookups resume in 15 minutes, or now if you {hint}.",
                        failure.status
                    );
                    self.pause(now + FAILURE_PAUSE_MS, Failure::with("paused", reason));
                }
            }
            Err(_) => {}
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn retry_manually(&mut self) {
        if self.paused.as_ref().is_some_and(|(_, failure)| failure.status == "rate-limited") {
            return;
        }
        self.reset();
    }
}

/// Pop-ups a provider opens to sign in: its own pages, and Steam's sign-in. Anything else (ads) stays shut.
fn is_sign_in_popup(url: &url::Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    let on = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    url.scheme() == "https" && (on("csrep.gg") || on("csstats.gg") || on("steamcommunity.com") || on("steampowered.com"))
}

/// Runs before each page's own scripts. Some sites (csstats.gg after "Load stats") ask "Leave site?" when
/// the window moves on; in a hidden window that dialog pops up on its own and blocks every later lookup.
/// Pages here can't register that prompt.
const NO_LEAVE_PROMPT: &str = r#"(() => {
  const add = EventTarget.prototype.addEventListener;
  EventTarget.prototype.addEventListener = function (type, ...rest) {
    if (type === 'beforeunload') return;
    return add.call(this, type, ...rest);
  };
  try { Object.defineProperty(window, 'onbeforeunload', { configurable: false, get: () => null, set: () => {} }); } catch {}
})();"#;

pub struct PageScraper {
    app: AppHandle,
    name: &'static str,
    data_dir: PathBuf,
    title: &'static str,
    queue: tokio::sync::Mutex<()>,
    breaker: Mutex<Breaker>,
    /// Counts lookup starts and ends; the idle timer closes the hidden window only if it is unchanged.
    activity: Arc<AtomicU64>,
    hidden: Arc<Mutex<Option<WebviewWindow>>>,
    visible: Mutex<Option<WebviewWindow>>,
    /// Numbers each hidden window: one closed when idle can still be shutting down when the next opens.
    windows_made: AtomicU64,
}

/// Runs on normal completion and when an awaiting task is aborted.
struct IdleCleanup<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for IdleCleanup<F> {
    fn drop(&mut self) {
        if let Some(cleanup) = self.0.take() {
            cleanup();
        }
    }
}

fn take_if_idle<T>(hidden: &Mutex<Option<T>>, activity: &AtomicU64, seen: u64) -> Option<T> {
    let mut hidden = hidden.lock().unwrap();
    (activity.load(Ordering::Relaxed) == seen).then(|| hidden.take()).flatten()
}

impl PageScraper {
    pub fn new(app: AppHandle, name: &'static str, data_dir: PathBuf, title: &'static str) -> Self {
        Self {
            app,
            name,
            data_dir,
            title,
            queue: tokio::sync::Mutex::new(()),
            breaker: Mutex::new(Breaker::default()),
            activity: Arc::new(AtomicU64::new(0)),
            hidden: Arc::new(Mutex::new(None)),
            visible: Mutex::new(None),
            windows_made: AtomicU64::new(0),
        }
    }

    /// `sign_in_popups`: the visible window lets sign-in pop-ups open (CSRep opens Steam sign-in in one);
    /// hidden lookup windows never open anything.
    fn builder(&self, label: &str, url: url::Url, sign_in_popups: bool) -> WebviewWindowBuilder<'_, tauri::Wry, AppHandle> {
        WebviewWindowBuilder::new(&self.app, label, WebviewUrl::External(url))
            .data_directory(self.data_dir.clone())
            .additional_browser_args(&browser_args())
            .initialization_script_for_all_frames(NO_LEAVE_PROMPT)
            .on_new_window(move |url, _| if sign_in_popups && is_sign_in_popup(&url) { NewWindowResponse::Allow } else { NewWindowResponse::Deny })
    }

    /// Visible window for the user (profile pages, sign-in, verification). Shares the hidden windows' profile.
    /// `over_game`: opened from the overlay, so the window is shown on top of CS2 until you click away.
    pub fn open(&self, url: &str, over_game: bool) -> Result<(), String> {
        let url: url::Url = url.parse().map_err(|e: url::ParseError| e.to_string())?;
        // The user is signing in or solving a check there, so lookups may work again.
        self.breaker.lock().unwrap().reset();
        let mut visible = self.visible.lock().unwrap();
        if let Some(window) = visible.as_ref().filter(|w| self.app.get_webview_window(w.label()).is_some()) {
            window.navigate(url).map_err(|e| e.to_string())?;
            bring_forward(window, over_game);
            return Ok(());
        }
        let label = format!("{}-view", self.name);
        let window = self.builder(&label, url, true).title(self.title).inner_size(1180.0, 860.0).build().map_err(|e| e.to_string())?;
        drop_on_top_when_left(&window);
        bring_forward(&window, over_game);
        *visible = Some(window);
        Ok(())
    }

    fn site(&self) -> &'static str {
        match self.name {
            "csrep" => "CSRep",
            "csstats" => "CSStats",
            other => other,
        }
    }

    pub fn retry_manually(&self) {
        self.breaker.lock().unwrap().retry_manually();
    }

    /// Lookups are serialized and spaced. They pause for a minute after a rate limit, and for 15 minutes
    /// after three players in a row without data.
    pub async fn read(&self, url: &str, options: ReadOptions<'_>) -> Result<Page, Failure> {
        let _turn = self.queue.lock().await;
        if let Some(paused) = self.breaker.lock().unwrap().check(crate::model::now_ms()) {
            return Err(paused);
        }
        {
            // Idle timers take the same lock, so one cannot close a window after a new lookup starts.
            let _hidden = self.hidden.lock().unwrap();
            self.activity.fetch_add(1, Ordering::Relaxed);
        }
        let _cleanup = IdleCleanup(Some(|| self.close_when_idle()));
        let result = self.load(url, &options).await;
        self.breaker.lock().unwrap().record(&result, self.site(), crate::model::now_ms());
        tokio::time::sleep(GAP).await;
        result
    }

    /// Closes the hidden window after a minute without lookups, so it costs nothing between matches.
    fn close_when_idle(&self) {
        let seen = {
            let _hidden = self.hidden.lock().unwrap();
            self.activity.fetch_add(1, Ordering::Relaxed) + 1
        };
        let (activity, hidden) = (self.activity.clone(), self.hidden.clone());
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(IDLE_CLOSE).await;
            if let Some(window) = take_if_idle(&hidden, &activity, seen) {
                let _ = window.destroy();
            }
        });
    }

    /// The hidden window, navigated to `url` (created on first use or after it was closed).
    async fn window_for(&self, url: url::Url) -> Result<WebviewWindow, Failure> {
        let existing = self.hidden.lock().unwrap().clone().filter(|w| self.app.get_webview_window(w.label()).is_some());
        if let Some(window) = existing {
            // If marking fails, no snapshot can safely be attributed to the new player. Drop the
            // old document rather than allowing its statistics to satisfy the next lookup.
            if evaluate::<bool>(&window, MARK_OLD_JS).await != Some(true) {
                let removed = self.hidden.lock().unwrap().take();
                if let Some(removed) = removed {
                    let _ = removed.destroy();
                }
                return Err(Failure::with("error", "Could not retire the previous profile page; retry the lookup."));
            }
            window.navigate(url).map_err(|e| Failure::with("error", e.to_string()))?;
            return Ok(window);
        }
        let window = self
            .builder(&format!("{}-scrape-{}", self.name, self.windows_made.fetch_add(1, Ordering::Relaxed)), url, false)
            .visible(false)
            .focused(false)
            .focusable(false)
            .skip_taskbar(true)
            .inner_size(1200.0, 900.0)
            .build()
            .map_err(|e| Failure::with("error", e.to_string()))?;
        *self.hidden.lock().unwrap() = Some(window.clone());
        Ok(window)
    }

    async fn load(&self, url: &str, options: &ReadOptions<'_>) -> Result<Page, Failure> {
        let parsed: url::Url = url.parse().map_err(|e: url::ParseError| Failure::with("error", e.to_string()))?;
        let window = self.window_for(parsed).await?;
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        let mut previous = String::new();
        let mut clicked = false;
        let mut consent_chosen = false;
        let mut verifying = false;
        let mut ready_since: Option<tokio::time::Instant> = None;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(POLL).await;
            // The script runs in whatever document is loaded; during navigation there may be no answer.
            let Some(page) = evaluate::<Page>(&window, SNAPSHOT_JS).await else { continue };
            if page.old {
                continue;
            }
            let blocked = page.blocked_reason();
            if matches!(blocked, None | Some("consent-required")) {
                if let (Some(script), false) = (options.consent_script, consent_chosen) {
                    consent_chosen = evaluate::<bool>(&window, script).await.unwrap_or(false);
                    if consent_chosen {
                        previous.clear();
                        continue;
                    }
                }
            }
            verifying = false;
            match blocked {
                // Cloudflare's automatic check usually finishes within seconds; keep waiting for it.
                Some("verification-required") => {
                    verifying = true;
                    previous.clear();
                    continue;
                }
                Some(blocked) => {
                    if blocked == "consent-required" {
                        return Err(Failure::with(
                            "consent-required",
                            "Open this profile in the app, choose your cookie preferences yourself, then refresh the player.",
                        ));
                    }
                    if blocked == "rate-limited" {
                        let failure = Failure::with("rate-limited", "Paused after a rate limit; try again in a minute.");
                        self.breaker.lock().unwrap().pause(crate::model::now_ms() + PAUSE_MS, failure);
                    }
                    return Err(Failure::new(blocked));
                }
                None => {}
            }
            if let (Some(script), false) = (options.click_script, clicked) {
                clicked = evaluate::<bool>(&window, script).await.unwrap_or(false);
                if clicked {
                    previous.clear();
                    continue;
                }
            }
            let complete = options.is_complete.is_none_or(|complete| complete(&page.text));
            if !complete && (options.is_ready)(&page.text) {
                let _ = evaluate::<bool>(&window, SCROLL_STEP_JS).await;
            }
            if (options.is_ready)(&page.text) && page.text == previous {
                let settled = ready_since.get_or_insert_with(tokio::time::Instant::now).elapsed() >= SETTLE;
                if complete || settled {
                    return Ok(page);
                }
            }
            previous = page.text;
        }
        if verifying {
            return Err(Failure::with(
                "verification-required",
                "Open this profile in the app, complete the site's security check yourself, then refresh the player.",
            ));
        }
        Err(Failure::with("timeout", "The page did not show statistics in time; open the profile to check it."))
    }
}

/// Evaluates a script in the page and decodes its JSON result; None when the page gave no answer.
async fn evaluate<T: serde::de::DeserializeOwned>(window: &WebviewWindow, script: &str) -> Option<T> {
    let (sender, receiver) = tokio::sync::oneshot::channel::<String>();
    let sender = Mutex::new(Some(sender));
    window
        .eval_with_callback(script, move |json| {
            if let Some(sender) = sender.lock().unwrap().take() {
                let _ = sender.send(json);
            }
        })
        .ok()?;
    let json = tokio::time::timeout(EVAL_TIMEOUT, receiver).await.ok()?.ok()?;
    serde_json::from_str(&json).ok()
}

/// Shows a window in front. Opened from the overlay, it goes on top of CS2 (which keeps the foreground
/// otherwise, so the window would open unseen behind the game).
pub fn bring_forward(window: &WebviewWindow, over_game: bool) {
    let _ = window.unminimize();
    let _ = window.show();
    if over_game {
        let _ = window.set_always_on_top(true);
    }
    let _ = window.set_focus();
}

/// Once you click back into the game, the window stops staying on top, so it can never cover CS2.
pub fn drop_on_top_when_left(window: &WebviewWindow) {
    let handle = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::Focused(false) = event {
            let _ = handle.set_always_on_top(false);
        }
    });
}

pub fn release_profile_windows(app: &AppHandle) {
    for label in ["profile-view", "csrep-view", "csstats-view"] {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.set_always_on_top(false);
        }
    }
}

/// Profile pages that need no special browser profile (Leetify, Steam, FACEIT), opened from the overlay in
/// an app window over the game. From the dashboard they open in your default browser instead.
pub fn open_profile_window(app: &AppHandle, data_dir: PathBuf, url: &str) -> Result<(), String> {
    let url: url::Url = url.parse().map_err(|e: url::ParseError| e.to_string())?;
    if let Some(window) = app.get_webview_window("profile-view") {
        window.navigate(url).map_err(|e| e.to_string())?;
        bring_forward(&window, true);
        return Ok(());
    }
    let window = WebviewWindowBuilder::new(app, "profile-view", WebviewUrl::External(url))
        .data_directory(data_dir)
        .additional_browser_args(&browser_args())
        .initialization_script_for_all_frames(NO_LEAVE_PROMPT)
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .title("Profile - CS2 Player Intel")
        .inner_size(1180.0, 860.0)
        .center()
        .build()
        .map_err(|e| e.to_string())?;
    drop_on_top_when_left(&window);
    bring_forward(&window, true);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sign_in_popups_open() {
        let url = |u: &str| url::Url::parse(u).unwrap();
        assert!(is_sign_in_popup(&url("https://csrep.gg/api/auth/steam")));
        assert!(is_sign_in_popup(&url("https://steamcommunity.com/openid/login?openid.mode=checkid_setup")));
        assert!(is_sign_in_popup(&url("https://www.csstats.gg/login")));
        assert!(!is_sign_in_popup(&url("https://ads.example.com/csrep.gg")));
        assert!(!is_sign_in_popup(&url("https://evilcsrep.gg/")), "a look-alike domain is not CSRep");
        assert!(!is_sign_in_popup(&url("http://csrep.gg/api/auth/steam")), "https only");
    }

    #[tokio::test]
    async fn aborted_lookup_still_schedules_idle_cleanup() {
        let cleaned = Arc::new(AtomicU64::new(0));
        let copy = cleaned.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _guard = IdleCleanup(Some(|| {
                copy.fetch_add(1, Ordering::Relaxed);
            }));
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(cleaned.load(Ordering::Relaxed), 1);
        let hidden = Mutex::new(Some("new profile"));
        let activity = AtomicU64::new(3);
        assert_eq!(take_if_idle(&hidden, &activity, 2), None, "old timers cannot close a newer lookup");
        assert_eq!(take_if_idle(&hidden, &activity, 3), Some("new profile"));
    }

    #[test]
    fn classifies_blocked_pages() {
        assert_eq!(classify_page("Please log in to view this profile"), Some("auth-required"));
        assert_eq!(classify_page("Verifying you are human. This may take a few seconds."), Some("verification-required"));
        assert_eq!(classify_page("Performing security verification"), Some("verification-required"));
        assert_eq!(classify_page("Error 1015 You are being rate limited"), Some("rate-limited"));
        assert_eq!(classify_page("TRUST SCORE\n87\n%"), None);
        assert_eq!(classify_page("Enable JavaScript and cookies to continue"), Some("verification-required"));
    }

    #[test]
    fn cookie_choices_and_challenge_titles_block_partial_statistics() {
        let consent = Page { text: "TRUST SCORE\n87\n%".into(), consent_required: true, ..Page::default() };
        assert_eq!(consent.blocked_reason(), Some("consent-required"));
        let challenge = Page { title: "Just a moment...".into(), ..Page::default() };
        assert_eq!(challenge.blocked_reason(), Some("verification-required"));
        let normal = Page { text: "TRUST SCORE\n87\n%\nCookie policy".into(), ..Page::default() };
        assert_eq!(normal.blocked_reason(), None, "a cookie-policy footer is not a consent prompt");
    }

    #[test]
    fn consent_failures_pause_until_the_user_opens_the_shared_profile() {
        let mut breaker = Breaker::default();
        for _ in 0..FAILURES_BEFORE_PAUSE {
            breaker.record(&Err(Failure::new("consent-required")), "CSRep", 0);
        }
        let paused = breaker.check(1).unwrap();
        assert!(paused.error.unwrap().contains("choose your cookie preferences"));
        breaker.reset();
        assert!(breaker.check(1).is_none());
    }

    #[test]
    fn manual_refresh_resumes_challenge_failures_but_preserves_rate_limits() {
        let mut breaker = Breaker::default();
        breaker.pause(1000, Failure::with("paused", "Cookie preferences need a decision."));
        breaker.retry_manually();
        assert!(breaker.check(1).is_none());
        breaker.pause(1000, Failure::new("rate-limited"));
        breaker.retry_manually();
        assert_eq!(breaker.check(1).unwrap().status, "rate-limited");
    }

    #[test]
    fn repeated_failures_pause_the_site_until_reset() {
        let mut breaker = Breaker::default();
        let timeout: Result<Page, Failure> = Err(Failure::new("timeout"));
        breaker.record(&timeout, "CSStats", 0);
        breaker.record(&Ok(Page::default()), "CSStats", 0);
        breaker.record(&timeout, "CSStats", 0);
        breaker.record(&timeout, "CSStats", 0);
        assert!(breaker.check(1).is_none(), "a success in between resets the count");
        breaker.record(&timeout, "CSStats", 0);
        let paused = breaker.check(1).expect("three in a row pause the site");
        assert_eq!(paused.status, "paused");
        assert!(paused.error.unwrap().contains("sign in"));
        assert!(breaker.check(FAILURE_PAUSE_MS + 1).is_none(), "the pause ends by itself");
        for _ in 0..3 {
            breaker.record(&Err(Failure::new("verification-required")), "CSRep", 0);
        }
        assert!(breaker.check(1).is_some());
        breaker.reset();
        assert!(breaker.check(1).is_none(), "opening the site resumes lookups");
        breaker.record(&Err(Failure::new("not-found")), "CSRep", 0);
        breaker.record(&Err(Failure::new("not-found")), "CSRep", 0);
        breaker.record(&Err(Failure::new("not-found")), "CSRep", 0);
        assert!(breaker.check(1).is_none(), "definitive answers are not failures");
    }

    #[test]
    fn browser_args_block_ad_hosts() {
        let args = browser_args();
        assert!(args.contains("MAP *.doubleclick.net ~NOTFOUND") && args.contains("--mute-audio"));
        assert!(args.contains("CalculateNativeWinOcclusion") && args.contains("--disable-renderer-backgrounding"));
        assert!(!args.contains("cloudflare"));
    }
}
