//! Ties the services together: GSI posts, the Steam roster, console sightings, provider lookups, the
//! overlay and keyboard, and pushing state to the windows.
//!
//! Shared state lives behind short-lived std mutexes that are never held across an await. Lock order,
//! where more than one is needed: publisher, settings, gsi, state, console, overlay, misc.

use crate::console_roster::{ConsoleTailer, Poll};
use crate::diagnostics::{Diagnostics, Level};
use crate::gsi::{GsiState, GsiSummary, Receipt};
use crate::gsi_config;
use crate::key_watcher::{KeyEvent, KeyWatcher};
use crate::match_controller::MatchState;
use crate::model::{now_ms, now_secs};
use crate::overlay::{self, KeyView, OverlayState};
use crate::player_data::{noteworthy, Force};
use crate::providers::{LookupContext, Providers};
use crate::settings::{to_hotkey, Settings, SettingsStore};
use crate::steam::SteamSession;
use crate::updater::UpdateService;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{watch, Notify};

const CS2_WATCH_INTERVAL: Duration = Duration::from_secs(5);
const STEAM_POLL_INTERVAL: Duration = Duration::from_millis(3500);
const PUBLISH_INTERVAL: Duration = Duration::from_millis(150);
const SETTINGS_SHORTCUT: &str = "Control+Shift+O";
/// How long a key press may change the menu state before CS2's next report confirms it.
const MENU_REPORT_GRACE: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hotkey {
    /// Pins or unpins the overlay.
    Pin,
    /// Makes the overlay clickable or click-through.
    Interact,
}
/// Installed copies update from the latest GitHub release. A build can point elsewhere (a test feed or a
/// local folder) with CS2INTEL_DEFAULT_UPDATE_SOURCE, and Settings can override it.
pub const DEFAULT_UPDATE_SOURCE: &str = match option_env!("CS2INTEL_DEFAULT_UPDATE_SOURCE") {
    Some(source) => source,
    None => "https://github.com/KKNZ89/cs2-player-intelligence-overlay/releases/latest/download/latest.json",
};
const WINDOWS: [&str; 2] = ["main", overlay::LABEL];

/// The Steam session as the dashboard shows it.
#[derive(Clone, Debug, Default, PartialEq)]
enum SteamStatus {
    #[default]
    Waiting,
    Starting,
    Ready,
    Unavailable(String),
}

impl SteamStatus {
    fn view(&self) -> Value {
        let (state, label, detail) = match self {
            Self::Waiting => ("waiting", "Waiting for CS2", "Starts with CS2, so Steam can still launch the game.".to_string()),
            Self::Starting => ("starting", "Starting", "Starting the Steam session.".to_string()),
            Self::Ready => ("ready", "Ready", "Reading Steam's recently played list to find the players in your match.".to_string()),
            Self::Unavailable(reason) => ("unavailable", "Unavailable", reason.clone()),
        };
        json!({ "state": state, "label": label, "detail": detail })
    }
}

#[derive(Default)]
struct Misc {
    steam: SteamStatus,
    cfg_found: bool,
    gsi_installed: bool,
    cs2_running: Option<bool>,
    connected: bool,
    shortcut_ready: bool,
    registered_hotkey: String,
    interact_ready: bool,
    registered_interact: String,
    /// CS2 has reported its Esc menu ("menu" activity) during a match, so its reports can be trusted.
    gsi_reports_menu: bool,
    /// When Esc or the clickable-overlay shortcut last changed the menu state.
    menu_toggled_at: Option<std::time::Instant>,
    /// Notification keys already sent this match.
    notified: std::collections::HashSet<String>,
    last_activity: String,
    gsi_config_notice: String,
    game_notice: String,
    displays: Vec<Value>,
    steam_runtime: Option<PathBuf>,
    steam_generation: u64,
    /// CSStats sign-in, as the last lookup showed it: stats loaded (signed in) or not, and when.
    csstats_signed_in: Option<(bool, i64)>,
    /// A check is set to run when the CSStats sign-in window closes.
    csstats_login_watched: bool,
    /// CS2's scoreboard is open (Tab held with CS2 in front).
    scoreboard_open: bool,
    scoreboard_scanning: bool,
    scoreboard_scanned_at: Option<std::time::Instant>,
}

#[derive(Default)]
struct Publisher {
    last: HashMap<&'static str, String>,
    last_flush: Option<tokio::time::Instant>,
}

pub struct Engine {
    pub app: AppHandle,
    settings: Mutex<SettingsStore>,
    gsi: Mutex<GsiState>,
    state: Mutex<MatchState>,
    console: Mutex<ConsoleTailer>,
    overlay: Mutex<OverlayState>,
    misc: Mutex<Misc>,
    publisher: Mutex<Publisher>,
    pub diagnostics: Diagnostics,
    pub providers: Providers,
    pub updates: Arc<UpdateService>,
    steam: tokio::sync::Mutex<Option<SteamSession>>,
    steam_wake: Notify,
    publish: Notify,
    key_watcher: Mutex<Option<KeyWatcher>>,
    retry_timer: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Recent provider results by (provider, SteamID), reused for players met again within the cache time.
    /// Memory only: nothing here is written to disk.
    cache: Mutex<HashMap<(String, String), crate::model::ProviderResult>>,
    /// Decoded Steam avatars by URL (None: it could not be fetched), for finding players on the scoreboard.
    avatars: Mutex<HashMap<String, Option<crate::scoreboard::Avatar>>>,
    /// Smoke test (CS2INTEL_SMOKE): pages and commands only; nothing touches Steam, CS2, hotkeys or the network.
    pub smoke: bool,
    /// Started with Windows (`--minimized`): stays in the tray and does not launch CS2.
    pub minimized: bool,
}

impl Engine {
    pub fn new(app: AppHandle, data_dir: PathBuf, settings: SettingsStore, state: MatchState, smoke: bool, minimized: bool) -> Arc<Self> {
        Arc::new_cyclic(|weak: &Weak<Engine>| {
            let (notify, log) = (weak.clone(), weak.clone());
            let updates = UpdateService::new(
                app.clone(),
                Arc::new(move || {
                    if let Some(engine) = notify.upgrade() {
                        engine.changed();
                    }
                }),
                Arc::new(move |message: &str| {
                    if let Some(engine) = log.upgrade() {
                        engine.diagnostics.error("update", message);
                    }
                }),
            );
            let mut settings = settings;
            let token = settings.gsi_token();
            let gsi = GsiState::new(token);
            Engine {
                providers: Providers::new(app.clone(), data_dir.clone()),
                diagnostics: Diagnostics::new(data_dir),
                app,
                settings: Mutex::new(settings),
                gsi: Mutex::new(gsi),
                state: Mutex::new(state),
                console: Mutex::new(ConsoleTailer::default()),
                overlay: Mutex::new(OverlayState::default()),
                misc: Mutex::new(Misc::default()),
                publisher: Mutex::new(Publisher::default()),
                updates,
                steam: tokio::sync::Mutex::new(None),
                steam_wake: Notify::new(),
                publish: Notify::new(),
                key_watcher: Mutex::new(None),
                retry_timer: Mutex::new(None),
                cache: Mutex::new(HashMap::new()),
                avatars: Mutex::new(HashMap::new()),
                smoke,
                minimized,
            }
        })
    }

    // ---- small helpers ----------------------------------------------------------------------------

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().get()
    }

    pub fn gsi_summary(&self) -> GsiSummary {
        self.gsi.lock().unwrap().summary(now_ms())
    }

    pub fn gsi_token(&self) -> String {
        self.gsi.lock().unwrap().token().to_string()
    }

    pub fn log(&self, operation: &str, message: &str) {
        self.diagnostics.error(operation, message);
    }

    /// Something a window shows may have changed.
    pub fn changed(&self) {
        self.publish.notify_one();
    }

    fn update_secrets(&self) {
        let s = self.settings();
        self.diagnostics.set_secrets(vec![s.csrep_api_key, s.steam_web_api_key, s.faceit_api_key, self.gsi_token()]);
    }

    pub fn update_source(&self) -> String {
        let source = self.settings().update_source;
        if source.is_empty() {
            DEFAULT_UPDATE_SOURCE.to_string()
        } else {
            source
        }
    }

    // ---- startup ----------------------------------------------------------------------------------

    pub fn start(self: &Arc<Self>) {
        self.update_secrets();
        let load_error = self.settings.lock().unwrap().load_error.clone();
        if !load_error.is_empty() {
            self.log("settings-load", &load_error);
        }
        self.refresh_displays();
        self.spawn_publisher();
        if self.smoke {
            // CS2INTEL_PROBE ("csrep:<steamid>,leetify:<steamid>"): real lookups, logged, for checking providers
            // without a match. Only run it while CS2 is closed.
            let probes: Vec<(String, String)> = std::env::var("CS2INTEL_PROBE")
                .unwrap_or_default()
                .split(',')
                .filter_map(|item| item.split_once(':').map(|(provider, id)| (provider.trim().to_string(), id.trim().to_string())))
                .collect();
            let engine = self.clone();
            tauri::async_runtime::spawn(async move {
                let context = LookupContext { csstats_enabled: true, csrep_pages_enabled: true, ..LookupContext::default() };
                for (provider, steam_id) in &probes {
                    let result = engine.providers.lookup(provider, steam_id, &context).await;
                    let json = serde_json::to_string(&result).unwrap_or_default();
                    engine.diagnostics.log(Level::Info, "probe", &format!("{provider} {steam_id}: {json}"));
                }
                tokio::time::sleep(Duration::from_secs(if probes.is_empty() { 10 } else { 1 })).await;
                engine.app.exit(0);
            });
            return;
        }
        self.refresh_gsi_config();
        self.refresh_gsi_installed();
        let retention = self.settings().retention_days;
        self.state.lock().unwrap().history.purge_older_than(retention);
        self.start_console_tailer();
        if self.settings().start_with_windows {
            // Keeps the registered path current after an update or a move.
            if let Err(error) = crate::autostart::set(true) {
                self.log("autostart", &error);
            }
        }
        let s = self.settings();
        for (kind, hotkey) in [(Hotkey::Pin, s.overlay_hotkey), (Hotkey::Interact, s.interact_hotkey)] {
            if !self.register_hotkey(kind, &hotkey) {
                self.log("shortcut", &format!("{hotkey} could not be registered (another app may use it). Choose another overlay shortcut in Settings."));
            }
        }
        self.register_settings_shortcut();
        self.spawn_gsi_server();
        self.spawn_loops();
        self.updates.configure(&self.update_source());
        self.schedule_retries();
        if self.settings().launch_cs2_on_start && !self.minimized {
            let engine = self.clone();
            tauri::async_runtime::spawn(async move {
                engine.start_game();
            });
        }
    }

    /// A config with another token or port is rewritten so CS2 keeps reporting.
    fn refresh_gsi_config(&self) {
        match gsi_config::refresh_installed(&self.settings().cs2_cfg_path, crate::gsi::PORT, &self.gsi_token()) {
            Ok(Some(_)) => {
                self.misc.lock().unwrap().gsi_config_notice = "GSI config updated with this install's private token. Restart CS2 if it is running.".into()
            }
            Ok(None) => {}
            Err(error) => self.log("gsi-refresh", &error),
        }
    }

    /// Whether CS2's cfg folder was found and holds this app's GSI config (for the setup guide).
    fn refresh_gsi_installed(&self) {
        let cfg = gsi_config::find_cs2_cfg_path(&self.settings().cs2_cfg_path);
        let installed = cfg.as_ref().is_some_and(|dir| dir.join(gsi_config::GSI_FILE_NAME).is_file());
        let mut misc = self.misc.lock().unwrap();
        if (misc.cfg_found, misc.gsi_installed) != (cfg.is_some(), installed) {
            (misc.cfg_found, misc.gsi_installed) = (cfg.is_some(), installed);
            drop(misc);
            self.changed();
        }
    }

    pub fn install_gsi_config(&self) -> Result<String, String> {
        let file = gsi_config::install(&self.settings().cs2_cfg_path, crate::gsi::PORT, &self.gsi_token())?;
        self.refresh_gsi_installed();
        self.misc.lock().unwrap().gsi_config_notice.clear();
        self.changed();
        Ok(file.to_string_lossy().into_owned())
    }

    fn spawn_gsi_server(self: &Arc<Self>) {
        use axum::body::Bytes;
        use axum::extract::{DefaultBodyLimit, State};
        use axum::http::{Method, StatusCode};
        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(("127.0.0.1", crate::gsi::PORT)).await {
                Ok(listener) => listener,
                Err(error) => {
                    let message = if error.kind() == std::io::ErrorKind::AddrInUse {
                        format!("Port {} is already in use. Close the other instance or application.", crate::gsi::PORT)
                    } else {
                        error.to_string()
                    };
                    engine.log("gsi-listen", &message);
                    engine.gsi.lock().unwrap().listen_error = message;
                    engine.changed();
                    return;
                }
            };
            let router = axum::Router::new()
                .fallback(|State(engine): State<Arc<Engine>>, method: Method, body: Bytes| async move {
                    if method != Method::POST {
                        return StatusCode::METHOD_NOT_ALLOWED;
                    }
                    engine.on_gsi_post(&body)
                })
                .layer(DefaultBodyLimit::max(2_000_000))
                .with_state(engine.clone());
            if let Err(error) = axum::serve(listener, router).await {
                engine.log("gsi-listen", &error.to_string());
            }
        });
    }

    /// State pushes: isolated changes go out at once, bursts are coalesced to one push per interval.
    fn spawn_publisher(self: &Arc<Self>) {
        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                engine.publish.notified().await;
                let last = engine.publisher.lock().unwrap().last_flush;
                if let Some(last) = last {
                    tokio::time::sleep_until(last + PUBLISH_INTERVAL).await;
                }
                engine.flush(&[]);
            }
        });
    }

    fn spawn_loops(self: &Arc<Self>) {
        // GSI silence means CS2 closed or left the server.
        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let connected = engine.gsi.lock().unwrap().connected(now_ms());
                let changed = std::mem::replace(&mut engine.misc.lock().unwrap().connected, connected) != connected;
                if changed {
                    if connected {
                        engine.changed()
                    } else {
                        engine.match_update()
                    }
                }
            }
        });
        // console.log sightings.
        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                engine.poll_console();
            }
        });
        // CS2 process watch: the Steam session and the key watcher only run while the game does.
        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(CS2_WATCH_INTERVAL);
            loop {
                tick.tick().await;
                engine.watch_cs2().await;
            }
        });
        // Notifications at quiet moments of a match.
        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(3));
            loop {
                tick.tick().await;
                engine.send_notifications();
            }
        });
        // Steam co-play roster while a match runs.
        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(STEAM_POLL_INTERVAL) => {}
                    _ = engine.steam_wake.notified() => {}
                }
                engine.poll_steam().await;
            }
        });
    }

    // ---- GSI and the match ------------------------------------------------------------------------

    fn on_gsi_post(self: &Arc<Self>, body: &[u8]) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        let receipt = self.gsi.lock().unwrap().receive(body, now_ms());
        match receipt {
            Receipt::BadJson => StatusCode::BAD_REQUEST,
            Receipt::Rejected { first } => {
                if first {
                    self.log("gsi-auth", crate::gsi::TOKEN_REJECTED);
                    self.changed();
                }
                StatusCode::UNAUTHORIZED
            }
            Receipt::Accepted => {
                // CS2 sends the current token, so a "restart CS2" notice is done.
                self.misc.lock().unwrap().gsi_config_notice.clear();
                self.match_update();
                // CS2 reports "menu" while its Esc menu is open; this corrects the Esc-key guess. Once CS2 has
                // shown that it reports the menu during a match, every report is authoritative (after a
                // short grace period for a key press it has not reported yet); before that, only changes count.
                let activity = self.gsi_summary().activity;
                let active = self.state.lock().unwrap().active();
                let reconcile = {
                    let mut misc = self.misc.lock().unwrap();
                    if activity == "menu" && active {
                        misc.gsi_reports_menu = true;
                    }
                    let settled = misc.menu_toggled_at.is_none_or(|at| at.elapsed() >= MENU_REPORT_GRACE);
                    let changed = std::mem::replace(&mut misc.last_activity, activity.clone()) != activity;
                    changed || (misc.gsi_reports_menu && settled)
                };
                if !activity.is_empty() && reconcile {
                    match activity.as_str() {
                        "menu" => self.set_menu_open(true),
                        "playing" => self.set_menu_open(false),
                        _ => {}
                    }
                }
                StatusCode::OK
            }
        }
    }

    /// Applies the latest GSI state to the match.
    pub fn match_update(self: &Arc<Self>) {
        let g = self.gsi_summary();
        let effects = self.state.lock().unwrap().update(&g);
        if effects.ended {
            self.diagnostics.log(Level::Info, "match", "ended; its players stay listed until the next match starts");
        }
        if effects.started {
            let detail = format!("started: {} {} ({})", g.map, g.mode, if g.phase == "warmup" { "from warmup" } else { "joined after warmup" });
            self.diagnostics.log(Level::Info, "match", &detail);
            self.misc.lock().unwrap().notified.clear();
        }
        if effects.ended {
            let days = self.settings().retention_days;
            self.state.lock().unwrap().history.purge_older_than(days);
        }
        if effects.reset {
            self.console.lock().unwrap().reset(true);
            self.providers.reset();
        }
        if effects.started {
            self.steam_wake.notify_one();
        }
        self.ensure_pending();
        self.changed();
    }

    /// Sends what is worth knowing now (players met before, strong opponents, profiles flagged for review,
    /// sources that are down), once per match and only at quiet moments.
    fn send_notifications(&self) {
        let s = self.settings();
        if !s.notifications || s.notify_types.is_empty() || self.smoke {
            return;
        }
        let g = self.gsi_summary();
        if !crate::notifications::quiet_moment(&g) {
            return;
        }
        let notices = {
            let mut state = self.state.lock().unwrap();
            if !state.active() {
                return;
            }
            let players = state.players.list();
            let self_id = g.self_steam_id.clone();
            let history = players.iter().map(|p| (p.steam_id.clone(), state.history.summary_for(&self_id, &p.steam_id))).collect();
            let analyses = if s.profile_indicators {
                crate::analysis::analyze_all(&players, crate::analysis::Options { min_matches: s.min_sample_matches, now_ms: now_ms() })
            } else {
                HashMap::new()
            };
            let sent = self.misc.lock().unwrap().notified.clone();
            crate::notifications::due(&players, &history, &analyses, &s.notify_types, &sent)
        };
        if notices.is_empty() {
            return;
        }
        use tauri_plugin_notification::NotificationExt;
        for notice in notices {
            let body = if s.privacy_mode { "Open the overlay for details.".to_string() } else { notice.body.clone() };
            if let Err(error) = self.app.notification().builder().title(&notice.title).body(body).show() {
                self.log("notification", &error.to_string());
            }
            self.misc.lock().unwrap().notified.extend(notice.keys);
        }
    }

    /// Everything recorded about a player, for the History and Notes tabs.
    pub fn player_history(&self, steam_id: &str) -> Value {
        let self_id = self.gsi_summary().self_steam_id;
        let mut state = self.state.lock().unwrap();
        let self_id =
            if self_id.is_empty() { state.players.list().iter().find(|p| p.is_self).map(|p| p.steam_id.clone()).unwrap_or_default() } else { self_id };
        state.history.player_history(&self_id, steam_id)
    }

    /// The FACEIT profile link FACEIT's API returned for a player in this match.
    pub fn faceit_url(&self, steam_id: &str) -> Option<String> {
        let state = self.state.lock().unwrap();
        let url = state.players.get(steam_id)?.provider("faceit")?.text("profileUrl")?.to_string();
        url.starts_with("https://www.faceit.com/").then_some(url)
    }

    /// Adds a note on a player: about this match, or with `match_id`, about a recorded one.
    pub fn add_note(&self, steam_id: &str, text: &str, match_id: Option<i64>) -> Result<Value, String> {
        let result = {
            let mut state = self.state.lock().unwrap();
            let context = match match_id {
                Some(id) => state.history.note_context_for_match(id, steam_id).ok_or("That match is no longer in the history.")?,
                None => state.note_context(steam_id),
            };
            state.history.add_note(steam_id, text, &context)
        };
        self.changed();
        result
    }

    pub fn delete_note(&self, id: i64) -> Result<(), String> {
        let result = self.state.lock().unwrap().history.delete_note(id);
        self.changed();
        result
    }

    pub fn history_stats(&self) -> Value {
        self.state.lock().unwrap().history.stats()
    }

    pub fn export_history(&self) -> Value {
        self.state.lock().unwrap().history.export()
    }

    pub fn import_history(&self, data: &Value) -> Result<Value, String> {
        let result = self.state.lock().unwrap().history.import(data);
        self.changed();
        result
    }

    pub fn clear_history(&self, notes: bool) -> Result<(), String> {
        let result = self.state.lock().unwrap().history.clear(notes);
        self.changed();
        result
    }

    /// Runs a state change that may add players, then looks them up.
    pub fn with_match<T>(self: &Arc<Self>, change: impl FnOnce(&mut MatchState, &GsiSummary) -> Result<T, String>) -> Result<T, String> {
        let g = self.gsi_summary();
        let result = change(&mut self.state.lock().unwrap(), &g);
        self.ensure_pending();
        self.changed();
        result
    }

    fn poll_console(self: &Arc<Self>) {
        let (poll, recent) = {
            let mut console = self.console.lock().unwrap();
            let poll = console.poll();
            (poll, console.recent())
        };
        match poll {
            Poll::Unchanged => {}
            Poll::Failed(error) => self.log("console-read", &error),
            Poll::Changed => {
                let g = self.gsi_summary();
                {
                    let mut state = self.state.lock().unwrap();
                    state.console_players = recent;
                    state.merge(&g);
                }
                self.ensure_pending();
                self.changed();
            }
        }
    }

    pub fn start_console_tailer(&self) {
        let s = self.settings();
        let cfg = gsi_config::find_cs2_cfg_path(&s.cs2_cfg_path).map(|p| p.to_string_lossy().into_owned()).unwrap_or(s.cs2_cfg_path.clone());
        let log = gsi_config::find_console_log_path(&s.cs2_console_log_path, &cfg);
        self.console.lock().unwrap().start(log);
    }

    // ---- Steam ------------------------------------------------------------------------------------

    async fn watch_cs2(self: &Arc<Self>) {
        self.install_update_if_idle();
        self.refresh_displays();
        self.refresh_gsi_installed();
        let running = crate::game::is_cs2_running();
        let previous = self.misc.lock().unwrap().cs2_running.replace(running);
        if previous == Some(running) {
            return;
        }
        self.changed();
        self.update_key_watcher();
        if running {
            self.init_roster().await;
        } else {
            self.stop_steam().await;
            self.set_roster_status(SteamStatus::Waiting);
        }
    }

    fn set_roster_status(&self, status: SteamStatus) {
        self.misc.lock().unwrap().steam = status;
        self.changed();
    }

    /// Starts the Steam helper for CS2's installed runtime (an SDK folder is only a fallback).
    pub async fn init_roster(self: &Arc<Self>) {
        let s = self.settings();
        let runtime = gsi_config::find_cs2_steam_runtime(&s.cs2_cfg_path).or_else(|| gsi_config::find_sdk_steam_runtime(&s.steamworks_sdk_path));
        let generation = {
            let mut misc = self.misc.lock().unwrap();
            if misc.steam_runtime == runtime && runtime.is_some() && self.steam.try_lock().map(|s| s.as_ref().is_some_and(|s| !s.is_closed())).unwrap_or(true) {
                return;
            }
            misc.steam_generation += 1;
            misc.steam_runtime = runtime.clone();
            misc.steam_generation
        };
        self.stop_steam_session().await;
        self.set_roster_status(SteamStatus::Starting);
        let Some(runtime) = runtime else {
            let message = "CS2's Steam runtime was not found. Set the CS2 cfg folder in Settings.";
            self.log("steam-init", message);
            self.set_roster_status(SteamStatus::Unavailable(message.into()));
            return;
        };
        let result = SteamSession::start(&runtime).await;
        if self.misc.lock().unwrap().steam_generation != generation {
            // Superseded (for example CS2 exited meanwhile): never leave an orphaned Steam session running.
            if let Ok(session) = result {
                session.shutdown().await;
            }
            return;
        }
        match result {
            Ok(session) => {
                *self.steam.lock().await = Some(session);
                self.set_roster_status(SteamStatus::Ready);
                self.steam_wake.notify_one();
            }
            Err(error) => {
                self.log("steam-init", &error);
                self.set_roster_status(SteamStatus::Unavailable(error));
            }
        }
    }

    async fn stop_steam_session(&self) {
        let session = self.steam.lock().await.take();
        if let Some(session) = session {
            session.shutdown().await;
        }
    }

    /// Steam treats the process holding the Steam session as CS2 itself, so it ends with the game.
    async fn stop_steam(&self) {
        {
            let mut misc = self.misc.lock().unwrap();
            misc.steam_generation += 1;
            misc.steam_runtime = None;
        }
        self.stop_steam_session().await;
    }

    async fn poll_steam(self: &Arc<Self>) {
        let since = {
            let state = self.state.lock().unwrap();
            if state.roster.match_started_at == 0 {
                return;
            }
            state.roster.window_start(now_secs())
        };
        let guard = self.steam.lock().await;
        let Some(session) = guard.as_ref() else { return };
        match session.snapshot(since).await {
            Ok(snapshot) => {
                drop(guard);
                let g = self.gsi_summary();
                let (errors, summary) = {
                    let mut state = self.state.lock().unwrap();
                    if state.roster.match_started_at == 0 {
                        return;
                    }
                    let before = state.roster.last_roster.len();
                    let errors = state.roster.apply_snapshot(&snapshot.entries, &snapshot.friends, now_secs());
                    state.merge(&g);
                    let roster = &state.roster;
                    // One line whenever the automatic list changes size, so missing players can be traced.
                    let summary = (roster.last_roster.len() != before).then(|| {
                        let now = now_secs();
                        let in_cs2 = snapshot.entries.iter().filter(|e| e.game_id == 730 && e.time >= roster.window_start(now)).count();
                        format!(
                            "{} players (including you) from {in_cs2} CS2 co-play entries in this match's window; {} mode, lobby of {}, {}. Co-play times vs first sighting: {}",
                            roster.last_roster.len(),
                            if roster.drop_in { "drop-in" } else { "fixed-lobby" },
                            roster.max_players,
                            if roster.joined_late { "joined after warmup" } else { "seen in warmup" },
                            roster.timing(&snapshot.entries, now)
                        )
                    });
                    (errors, summary)
                };
                for error in errors {
                    self.log("steam-entry", &error);
                }
                if let Some(summary) = summary {
                    self.diagnostics.log(Level::Info, "roster", &summary);
                }
                self.ensure_pending();
                self.changed();
            }
            Err(error) => {
                self.log("steam-poll", &error);
                // A crashed Steam session cannot recover; the next CS2 start initializes a new one.
                if session.is_closed() {
                    drop(guard);
                    self.stop_steam_session().await;
                    self.set_roster_status(SteamStatus::Unavailable(error));
                }
            }
        }
    }

    /// Ends the helper process at exit (it also exits by itself when the app's pipe closes).
    pub fn kill_steam(&self) {
        if let Ok(mut guard) = self.steam.try_lock() {
            if let Some(mut session) = guard.take() {
                session.kill();
            }
        }
    }

    // ---- lookups ----------------------------------------------------------------------------------

    fn lookup_context(&self) -> LookupContext {
        let s = self.settings();
        LookupContext {
            csrep_api_key: s.csrep_api_key,
            steam_web_api_key: s.steam_web_api_key,
            faceit_api_key: s.faceit_api_key,
            csstats_enabled: s.csstats_enabled,
            csrep_pages_enabled: s.csrep_pages_enabled,
            self_id: self.gsi_summary().self_steam_id,
        }
    }

    pub fn ensure_pending(self: &Arc<Self>) {
        let ids = self.state.lock().unwrap().players.ids();
        for id in ids {
            self.ensure(&id, Force::Pending);
        }
    }

    /// Starts the due lookups for one player, or joins the ones already running. The receiver turns true
    /// (or closes) when they are done.
    pub fn ensure(self: &Arc<Self>, steam_id: &str, force: Force) -> Option<watch::Receiver<bool>> {
        let self_id = self.gsi_summary().self_steam_id;
        let cache_ms = (self.settings().cache_minutes * 60_000.0) as i64;
        let mut state = self.state.lock().unwrap();
        if let Some(running) = state.players.in_flight.get(steam_id) {
            return Some(running.done.clone());
        }
        let mut due = state.players.due(steam_id, force, &self_id);
        // A player met again within the cache time: reuse what was fetched then (refreshes always fetch).
        if force == Force::Pending && cache_ms > 0 {
            let cache = self.cache.lock().unwrap();
            let now = now_ms();
            due.retain(|provider| {
                let hit = cache.get(&(provider.to_string(), steam_id.to_string())).filter(|r| r.fetched_at.is_some_and(|at| now - at < cache_ms)).cloned();
                match hit {
                    Some(result) => {
                        state.players.apply_cached(steam_id, provider, result);
                        false
                    }
                    None => true,
                }
            });
        }
        if due.is_empty() {
            return None;
        }
        let task = state.players.next_task();
        let generation = state.players.generation;
        let (done, receiver) = watch::channel(false);
        let engine = self.clone();
        let id = steam_id.to_string();
        let handle = tauri::async_runtime::spawn(async move {
            engine.run_lookups(&id, due, generation, task).await;
            let _ = done.send(true);
        });
        state
            .players
            .in_flight
            .insert(steam_id.to_string(), crate::player_data::InFlight { task, abort: handle.inner().abort_handle(), done: receiver.clone() });
        Some(receiver)
    }

    async fn run_lookups(self: &Arc<Self>, steam_id: &str, due: Vec<&'static str>, generation: u64, task: u64) {
        let context = Arc::new(self.lookup_context());
        // Providers run in parallel; dropping the set (when this task is cancelled) cancels them all.
        let mut lookups = tokio::task::JoinSet::new();
        for name in due.iter().copied() {
            let (engine, context, id) = (self.clone(), context.clone(), steam_id.to_string());
            lookups.spawn(async move { (name, engine.providers.lookup(name, &id, &context).await) });
        }
        while let Some(joined) = lookups.join_next().await {
            let Ok((name, result)) = joined else { continue };
            if name == "csstats" {
                self.note_csstats(&result, false);
            }
            if let Some(line) = noteworthy(name, &result) {
                self.log(&format!("{name}-lookup"), line.split_once(": ").map(|(_, rest)| rest).unwrap_or(&line));
            }
            {
                let mut state = self.state.lock().unwrap();
                if !state.players.is_current(steam_id, generation, task) {
                    return;
                }
                state.players.apply_result(steam_id, name, result);
                self.remember(steam_id, name, state.players.get(steam_id).and_then(|p| p.provider(name)));
            }
            self.changed();
        }
        let gaps = {
            let mut state = self.state.lock().unwrap();
            if !state.players.is_current(steam_id, generation, task) {
                return;
            }
            state.players.in_flight.remove(steam_id);
            state.players.gaps(steam_id, &due)
        };
        if let Some(gaps) = gaps {
            self.diagnostics.log(Level::Info, "lookup-summary", &gaps);
        }
    }

    /// Keeps a fresh, complete result for reuse ("history" depends on your account, so it is not kept).
    fn remember(&self, steam_id: &str, provider: &str, result: Option<&crate::model::ProviderResult>) {
        let Some(result) = result.filter(|r| r.is_ok() && !r.stale && provider != "history") else { return };
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= 5000 {
            // Drop the oldest entries rather than growing without bound.
            let mut by_age: Vec<_> = cache.iter().map(|(key, r)| (r.fetched_at.unwrap_or(0), key.clone())).collect();
            by_age.sort();
            for (_, key) in by_age.into_iter().take(1000) {
                cache.remove(&key);
            }
        }
        cache.insert((provider.to_string(), steam_id.to_string()), result.clone());
    }

    /// Stats on a CSStats page mean the sign-in works. A page without them only means "not signed in" for
    /// your own profile (`own`): another player may simply have no CSStats data. Other failures (a
    /// verification check, a timeout) say nothing about the sign-in.
    fn note_csstats(&self, result: &crate::model::ProviderResult, own: bool) {
        let signed_in = match result.status.as_str() {
            "ok" => true,
            "no-stats" if own => false,
            _ => return,
        };
        self.misc.lock().unwrap().csstats_signed_in = Some((signed_in, now_ms()));
    }

    /// Your recorded matches (this account's, or every account's before CS2 has reported one), newest first.
    pub fn history_matches(&self, limit: usize, offset: usize) -> Value {
        let self_id = self.own_steam_id();
        self.state.lock().unwrap().history.matches(&self_id, limit.clamp(1, 200), offset)
    }

    pub fn history_match(&self, id: i64) -> Option<Value> {
        self.state.lock().unwrap().history.match_details(id)
    }

    fn own_steam_id(&self) -> String {
        let id = self.gsi_summary().self_steam_id;
        if !id.is_empty() {
            return id;
        }
        self.state.lock().unwrap().history.last_self_id().unwrap_or_default()
    }

    /// Opens the CSStats sign-in window; when you close it, your own profile is looked up to check the sign-in.
    pub fn csstats_login(self: &Arc<Self>) -> Result<(), String> {
        self.providers.csstats.open_login()?;
        let Some(window) = self.app.get_webview_window("csstats-view") else { return Ok(()) };
        {
            let mut misc = self.misc.lock().unwrap();
            if misc.csstats_login_watched {
                return Ok(());
            }
            misc.csstats_login_watched = true;
        }
        let engine = self.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::Destroyed = event {
                engine.misc.lock().unwrap().csstats_login_watched = false;
                let engine = engine.clone();
                tauri::async_runtime::spawn(async move { engine.check_csstats().await });
            }
        });
        Ok(())
    }

    pub async fn check_csstats(self: &Arc<Self>) {
        let id = self.own_steam_id();
        if id.is_empty() || !self.settings().csstats_enabled {
            return;
        }
        let result = self.providers.csstats.player(&id).await;
        self.note_csstats(&result, true);
        self.changed();
    }

    async fn wait(receivers: Vec<watch::Receiver<bool>>) {
        for mut receiver in receivers {
            let _ = receiver.wait_for(|done| *done).await;
        }
    }

    pub async fn refresh_all(self: &Arc<Self>, force: Force) {
        if force == Force::All {
            self.providers.csrep.scraper.retry_manually();
            self.providers.csstats.scraper.retry_manually();
        }
        let ids = self.state.lock().unwrap().players.ids();
        let receivers = ids.iter().filter_map(|id| self.ensure(id, force)).collect();
        Self::wait(receivers).await;
    }

    /// Re-fetches every provider for one player, keeping current values if a lookup fails.
    pub async fn refresh_player(self: &Arc<Self>, steam_id: &str) -> Result<(), String> {
        if self.state.lock().unwrap().players.get(steam_id).is_none() {
            return Err("Unknown player.".into());
        }
        self.providers.csrep.scraper.retry_manually();
        self.providers.csstats.scraper.retry_manually();
        Self::wait(self.ensure(steam_id, Force::All).into_iter().collect()).await;
        Ok(())
    }

    pub fn schedule_retries(self: &Arc<Self>) {
        let minutes = self.settings().auto_retry_minutes;
        let mut timer = self.retry_timer.lock().unwrap();
        if let Some(previous) = timer.take() {
            previous.abort();
        }
        if minutes <= 0.0 {
            return;
        }
        let engine = self.clone();
        *timer = Some(tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs_f64(minutes * 60.0)).await;
                if engine.state.lock().unwrap().active() {
                    engine.refresh_all(Force::Failed).await;
                }
            }
        }));
    }

    // ---- overlay and keys -------------------------------------------------------------------------

    /// Positions the overlay; with `show`, also shows or hides it for its current view.
    pub fn apply_overlay(&self, show: bool) -> bool {
        let settings = self.settings();
        let visible = self.overlay.lock().unwrap().view() != "hidden";
        // Fresh state first, so the overlay never flashes data from the last time it was visible.
        if show && visible && !overlay::is_visible(&self.app) {
            self.flush(&[overlay::LABEL]);
        }
        // Window calls wait for the UI thread, so no lock is held during them.
        let state = self.overlay.lock().unwrap().clone();
        if let Err(error) = overlay::apply(&self.app, &state, &settings, show) {
            self.log("overlay", &format!("Could not apply overlay window settings: {error}"));
            return false;
        }
        true
    }

    pub fn toggle_pinned(&self) -> bool {
        let pinned = {
            let mut overlay = self.overlay.lock().unwrap();
            overlay.pinned = !overlay.pinned;
            overlay.pinned
        };
        self.apply_overlay(true);
        self.changed();
        pinned
    }

    fn set_key_view(&self, view: KeyView) {
        self.overlay.lock().unwrap().key_view = view;
        self.apply_overlay(true);
        self.changed();
    }

    /// CS2's Esc menu frees the cursor; the overlay then becomes clickable (only during a match).
    pub fn set_menu_open(&self, open: bool) {
        let allowed = self.settings().esc_interactive && self.state.lock().unwrap().active() && (!open || crate::key_watcher::is_cs2_foreground());
        {
            let mut overlay = self.overlay.lock().unwrap();
            if overlay.menu_open == (open && allowed) {
                return;
            }
            overlay.menu_open = open && allowed;
        }
        self.apply_overlay(true);
        self.changed();
    }

    pub fn fit_overlay(&self, height: f64, side_width: Option<f64>) -> bool {
        if !height.is_finite() || !(50.0..=2400.0).contains(&height) {
            return false;
        }
        {
            let mut overlay = self.overlay.lock().unwrap();
            overlay.content_height = height.ceil();
            if let Some(width) = side_width.filter(|w| w.is_finite() && (200.0..=2000.0).contains(w)) {
                overlay.side_width = width.ceil();
            }
        }
        self.apply_overlay(false);
        true
    }

    fn on_key(self: &Arc<Self>, event: KeyEvent) {
        match event {
            KeyEvent::View(view) => self.set_key_view(view),
            KeyEvent::Escape => {
                self.misc.lock().unwrap().menu_toggled_at = Some(std::time::Instant::now());
                let open = self.overlay.lock().unwrap().menu_open;
                self.set_menu_open(!open);
            }
            KeyEvent::Foreground(false) => self.set_menu_open(false),
            // Back in the game: profile windows opened from the overlay stop staying on top of it.
            KeyEvent::Foreground(true) => crate::providers::scraper::release_profile_windows(&self.app),
            KeyEvent::Scoreboard(open) => self.on_scoreboard(open),
        }
    }

    /// The scoreboard opened or closed. While it is open during a match, and colours are still missing, the
    /// scoreboard is read (at most every 20 seconds).
    fn on_scoreboard(self: &Arc<Self>, open: bool) {
        self.misc.lock().unwrap().scoreboard_open = open;
        if !open || !self.settings().scoreboard_colours {
            return;
        }
        let teams = self.state.lock().unwrap().mode(&self.gsi_summary()).teams;
        let wanted = {
            let state = self.state.lock().unwrap();
            state.active() && (state.players.wants_colours() || (teams && state.players.wants_sides()))
        };
        {
            let mut misc = self.misc.lock().unwrap();
            let recent = misc.scoreboard_scanned_at.is_some_and(|at| at.elapsed() < Duration::from_secs(20));
            if !wanted || misc.scoreboard_scanning || recent {
                return;
            }
            misc.scoreboard_scanning = true;
            misc.scoreboard_scanned_at = Some(std::time::Instant::now());
        }
        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = engine.read_scoreboard().await {
                engine.log("scoreboard-colours", &error);
            }
            engine.misc.lock().unwrap().scoreboard_scanning = false;
        });
    }

    /// A downloaded update installs by itself when it can't interrupt anything: CS2 isn't running and the
    /// dashboard is closed (the app is in the notification area). The app restarts there afterwards.
    /// Otherwise it waits for Restart to update, or installs when the app quits.
    fn install_update_if_idle(&self) {
        if self.smoke || !self.settings().auto_install_updates || !self.updates.is_ready() {
            return;
        }
        if self.misc.lock().unwrap().cs2_running != Some(false) {
            return;
        }
        let dashboard_open = self.app.get_webview_window("main").and_then(|w| w.is_visible().ok()).unwrap_or(false);
        if dashboard_open {
            return;
        }
        let data_dir = self.diagnostics.file.parent().map(std::path::Path::to_path_buf);
        if let Some(dir) = &data_dir {
            crate::updater::mark_quiet_restart(dir);
        }
        self.diagnostics.log(Level::Info, "update", "Installing the downloaded update; the app restarts in the notification area.");
        if let Err(error) = self.updates.install() {
            if let Some(dir) = &data_dir {
                crate::updater::take_quiet_restart(dir);
            }
            self.log("update", &error);
        }
    }

    async fn read_scoreboard(self: &Arc<Self>) -> Result<(), String> {
        // The scoreboard fades in; read it once it is fully drawn, and only if it is still open.
        tokio::time::sleep(Duration::from_millis(450)).await;
        if !self.misc.lock().unwrap().scoreboard_open {
            return Ok(());
        }
        #[cfg(not(windows))]
        return Err("Reading the scoreboard needs Windows.".into());
        #[cfg(windows)]
        {
            let hwnd = crate::capture::find_window("Counter-Strike 2").ok_or("The CS2 window was not found.")?;
            let frame = tauri::async_runtime::spawn_blocking(move || crate::capture::capture_window(hwnd, Duration::from_secs(2)))
                .await
                .map_err(|e| e.to_string())??;
            let players: Vec<(String, String)> = {
                let state = self.state.lock().unwrap();
                state
                    .players
                    .ids()
                    .iter()
                    .filter_map(|id| {
                        let player = state.players.get(id)?;
                        let url = player.provider("steam")?.text("avatar")?.to_string();
                        Some((id.clone(), url))
                    })
                    .collect()
            };
            let avatars = self.avatars_for(&players).await;
            let looked_for = avatars.len();
            let (found, frame) =
                tauri::async_runtime::spawn_blocking(move || (crate::scoreboard::read(&frame, &avatars), frame)).await.map_err(|e| e.to_string())?;
            let coloured: Vec<_> = found.iter().filter_map(|f| f.colour.map(|c| (f.steam_id.clone(), c.name()))).collect();
            let teams = self.state.lock().unwrap().mode(&self.gsi_summary()).teams;
            let sides = if teams { crate::scoreboard::sides(&found, &self.own_steam_id()) } else { Vec::new() };
            let changed = {
                let mut state = self.state.lock().unwrap();
                let coloured_changed = coloured.iter().fold(false, |changed, (id, colour)| state.players.set_colour(id, colour) || changed);
                sides.iter().fold(coloured_changed, |changed, (id, side)| state.players.set_scoreboard_side(id, side) || changed)
            };
            self.diagnostics.log(
                Level::Info,
                "scoreboard-colours",
                &format!(
                    "Scoreboard read: {} of {looked_for} avatars found, {} teammate colours, {} players placed in teams.",
                    found.len(),
                    coloured.len(),
                    sides.len()
                ),
            );
            if coloured.is_empty() && sides.is_empty() {
                // Kept for checking what the app saw; overwritten each time, never sent anywhere.
                let file = self.diagnostics.file.with_file_name("scoreboard-last.png");
                if frame.save_png(&file).is_ok() {
                    self.diagnostics.log(Level::Info, "scoreboard-colours", &format!("No colours found; the captured scoreboard is in {}", file.display()));
                }
            }
            if changed {
                self.changed();
            }
            Ok(())
        }
    }

    /// Decoded avatars for (SteamID, avatar URL) pairs, downloading the ones not seen yet. Only Steam's
    /// avatar hosts are contacted.
    async fn avatars_for(&self, players: &[(String, String)]) -> Vec<crate::scoreboard::Avatar> {
        let steam_host = |url: &str| {
            url::Url::parse(url).is_ok_and(|u| {
                u.scheme() == "https"
                    && u.host_str().is_some_and(|h| h.starts_with("avatars.") && h.ends_with(".steamstatic.com") || h == "avatars.steamstatic.com")
            })
        };
        let missing: Vec<String> = {
            let cache = self.avatars.lock().unwrap();
            players.iter().map(|(_, url)| url.clone()).filter(|url| steam_host(url) && !cache.contains_key(url)).collect()
        };
        if !missing.is_empty() {
            let http = crate::providers::http_client();
            for url in missing {
                let decoded = match http.get(&url).timeout(Duration::from_secs(8)).send().await {
                    Ok(response) if response.status().is_success() => response.bytes().await.ok().and_then(|bytes| decode_jpeg(&bytes)),
                    _ => None,
                };
                self.avatars.lock().unwrap().insert(url, decoded);
            }
        }
        let cache = self.avatars.lock().unwrap();
        players
            .iter()
            .filter_map(|(id, url)| cache.get(url).cloned().flatten().map(|avatar| crate::scoreboard::Avatar { steam_id: id.clone(), ..avatar }))
            .collect()
    }

    /// The key watcher runs while CS2 runs and either hold-Tab or the Esc-menu overlay is enabled.
    pub fn update_key_watcher(self: &Arc<Self>) {
        let s = self.settings();
        let hold_tab = s.overlay_trigger == "hold-tab";
        let running = self.misc.lock().unwrap().cs2_running == Some(true);
        let wanted = cfg!(windows) && running && (hold_tab || s.esc_interactive || s.scoreboard_colours);
        let mut watcher = self.key_watcher.lock().unwrap();
        if !wanted {
            let stopped = watcher.take();
            drop(watcher);
            drop(stopped);
            self.set_menu_open(false);
            return;
        }
        match watcher.as_ref() {
            Some(running) => running.set_hold_tab(hold_tab),
            None => {
                #[cfg(windows)]
                {
                    let weak = Arc::downgrade(self);
                    *watcher = Some(KeyWatcher::start(hold_tab, move |event| {
                        if let Some(engine) = weak.upgrade() {
                            engine.on_key(event);
                        }
                    }));
                }
            }
        }
    }

    /// The overlay shortcuts are yours to choose; a shortcut another app owns is refused.
    fn register_hotkey(self: &Arc<Self>, kind: Hotkey, accelerator: &str) -> bool {
        let shortcuts = self.app.global_shortcut();
        let previous = {
            let mut misc = self.misc.lock().unwrap();
            std::mem::take(if kind == Hotkey::Pin { &mut misc.registered_hotkey } else { &mut misc.registered_interact })
        };
        if !previous.is_empty() {
            let _ = shortcuts.unregister(to_hotkey(&previous).as_str());
        }
        let weak = Arc::downgrade(self);
        let ok = shortcuts
            .on_shortcut(to_hotkey(accelerator).as_str(), move |_, _, event| {
                // Shortcut handlers run on the UI thread; the work runs on the async runtime.
                if let (ShortcutState::Pressed, Some(engine)) = (event.state, weak.upgrade()) {
                    tauri::async_runtime::spawn(async move {
                        match kind {
                            Hotkey::Pin => {
                                engine.toggle_pinned();
                            }
                            Hotkey::Interact => {
                                if let Err(error) = engine.toggle_interactive() {
                                    engine.log("overlay-mouse", &error);
                                }
                            }
                        }
                    });
                }
            })
            .is_ok();
        let mut guard = self.misc.lock().unwrap();
        let misc = &mut *guard;
        let (ready, registered) = if kind == Hotkey::Pin {
            (&mut misc.shortcut_ready, &mut misc.registered_hotkey)
        } else {
            (&mut misc.interact_ready, &mut misc.registered_interact)
        };
        *ready = ok;
        if ok {
            *registered = accelerator.to_string();
        }
        ok
    }

    /// Only an explicit shortcut requests focus. GSI and foreground reports cannot cancel this mode;
    /// leaving the overlay, pressing Esc or pressing the shortcut again ends it.
    pub fn toggle_interactive(&self) -> Result<(), String> {
        if self.end_mouse_navigation() {
            return Ok(());
        }
        if !self.state.lock().unwrap().active() {
            return Err("Mouse navigation is available during a match. Check the GSI connection in Diagnostics.".into());
        }
        let window = self.app.get_webview_window(overlay::LABEL).ok_or("The overlay window is not available.")?;
        self.overlay.lock().unwrap().mouse_navigation = true;
        if !self.apply_overlay(true) {
            self.end_mouse_navigation();
            return Err("Could not prepare overlay mouse navigation. See Diagnostics for the window error.".into());
        }
        if let Err(error) = overlay::focus_mouse_navigation(&window) {
            self.end_mouse_navigation();
            return Err(format!("Could not enable overlay mouse navigation: {error}"));
        }
        self.diagnostics.log(Level::Info, "overlay-mouse", "Mouse navigation enabled; the native overlay owns foreground focus.");
        self.changed();
        Ok(())
    }

    pub fn end_mouse_navigation(&self) -> bool {
        let changed = self.overlay.lock().unwrap().end_mouse_navigation();
        if changed {
            self.apply_overlay(true);
            self.changed();
        }
        changed
    }

    /// Ctrl+Shift+O opens the settings in the dashboard.
    fn register_settings_shortcut(&self) {
        let app = self.app.clone();
        let result = self.app.global_shortcut().on_shortcut(SETTINGS_SHORTCUT, move |_, _, event| {
            if event.state == ShortcutState::Pressed {
                show_main(&app);
                let _ = app.emit_to("main", "ui:command", "open-setup");
            }
        });
        if let Err(error) = result {
            self.log("shortcut", &format!("{SETTINGS_SHORTCUT}: {error}"));
        }
    }

    // ---- settings and actions ---------------------------------------------------------------------

    pub async fn save_settings(self: &Arc<Self>, input: Map<String, Value>) -> Result<Value, String> {
        let before = self.settings();
        let view = self.settings.lock().unwrap().update(&input)?;
        let after = self.settings();
        for (kind, key, old, new) in [
            (Hotkey::Pin, "overlayHotkey", &before.overlay_hotkey, &after.overlay_hotkey),
            (Hotkey::Interact, "interactHotkey", &before.interact_hotkey, &after.interact_hotkey),
        ] {
            if old != new && !self.register_hotkey(kind, new) {
                let mut restore = Map::new();
                restore.insert(key.into(), old.clone().into());
                let _ = self.settings.lock().unwrap().update(&restore);
                self.register_hotkey(kind, old);
                return Err(format!("{new} is already used by another app. Choose a different shortcut."));
            }
        }
        if after.start_with_windows != before.start_with_windows && !self.smoke {
            if let Err(error) = crate::autostart::set(after.start_with_windows) {
                let mut restore = Map::new();
                restore.insert("startWithWindows".into(), before.start_with_windows.into());
                let _ = self.settings.lock().unwrap().update(&restore);
                return Err(format!("Start with Windows could not be changed: {error}"));
            }
        }
        self.update_secrets();
        self.apply_overlay(false);
        let running = self.misc.lock().unwrap().cs2_running == Some(true);
        let steam_ready = self.steam.lock().await.is_some();
        if running && (before.steamworks_sdk_path != after.steamworks_sdk_path || (!steam_ready && before.cs2_cfg_path != after.cs2_cfg_path)) {
            self.init_roster().await;
        }
        self.start_console_tailer();
        self.updates.configure(&self.update_source());
        self.update_key_watcher();
        if after.auto_retry_minutes != before.auto_retry_minutes {
            self.schedule_retries();
        }
        self.refresh_gsi_installed();
        self.refresh_displays();
        self.changed();
        Ok(view)
    }

    /// Launches CS2 through Steam unless it already runs.
    pub fn start_game(&self) -> String {
        let notice = if crate::game::is_cs2_running() {
            "CS2 is already running.".to_string()
        } else {
            match self.app.opener().open_url(crate::game::LAUNCH_URL, None::<&str>) {
                Ok(()) => "CS2 launch requested through Steam.".into(),
                Err(error) => {
                    self.log("game-launch", &error.to_string());
                    "CS2 could not be launched. Check that Steam is installed.".into()
                }
            }
        };
        self.misc.lock().unwrap().game_notice = notice.clone();
        self.changed();
        notice
    }

    pub fn set_player_side(&self, steam_id: &str, side: &str) -> Result<(), String> {
        let result = self.state.lock().unwrap().players.set_side(steam_id, side);
        self.changed();
        result
    }

    fn refresh_displays(&self) {
        let Some(window) = self.app.get_webview_window("main") else { return };
        let displays: Vec<Value> = window
            .available_monitors()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(index, monitor)| {
                let size = monitor.size();
                json!({ "id": overlay::monitor_id(monitor), "label": format!("Display {} ({}×{})", index + 1, size.width, size.height) })
            })
            .collect();
        let mut misc = self.misc.lock().unwrap();
        if misc.displays != displays {
            misc.displays = displays;
            drop(misc);
            self.apply_overlay(false);
            self.changed();
        }
    }

    // ---- state publishing -------------------------------------------------------------------------

    fn build_state(&self) -> Value {
        let (public_settings, s) = {
            let mut settings = self.settings.lock().unwrap();
            (settings.public_view(), settings.get())
        };
        let g = self.gsi_summary();
        let (mut view, coverage) = {
            let mut state = self.state.lock().unwrap();
            state.min_sample_matches = s.min_sample_matches;
            (state.view(&g), state.players.coverage())
        };
        let (overlay_view, pinned, menu_open, mouse_navigation) = {
            let overlay = self.overlay.lock().unwrap();
            (overlay.view(), overlay.pinned, overlay.menu_open, overlay.mouse_navigation)
        };
        let misc = self.misc.lock().unwrap();
        let mut settings_view = public_settings.clone();
        settings_view["defaultUpdateSource"] = DEFAULT_UPDATE_SOURCE.into();
        let hotkey = if misc.registered_hotkey.is_empty() { s.overlay_hotkey.clone() } else { misc.registered_hotkey.clone() };
        let extra = json!({
            "gsi": g,
            "coverage": coverage,
            "overlayView": overlay_view,
            "overlayMouseNavigation": mouse_navigation,
            "rosterStatus": misc.steam.view()["detail"],
            "steam": misc.steam.view(),
            "settings": settings_view,
            "overlayVisible": pinned,
            "displays": misc.displays,
            "version": self.app.package_info().version.to_string(),
            "updates": self.updates.view(),
            "setup": {
                "shortcutReady": misc.shortcut_ready,
                "interactReady": misc.interact_ready,
                "interactHotkey": if misc.registered_interact.is_empty() { s.interact_hotkey.clone() } else { misc.registered_interact.clone() },
                "hotkey": hotkey,
                "menuOpen": menu_open,
                "diagnosticsFile": self.diagnostics.file.to_string_lossy(),
                "cfgFound": misc.cfg_found,
                "gsiInstalled": misc.gsi_installed,
                "cs2Running": misc.cs2_running == Some(true),
                "csrep": if !s.csrep_api_key.is_empty() { "API key" } else if s.csrep_pages_enabled { "public profile pages" } else { "off" },
                "csstats": if s.csstats_enabled { "on (verify with a lookup)" } else { "off" },
                "csstatsSignIn": match misc.csstats_signed_in {
                    Some((signed_in, at)) => json!({ "signedIn": signed_in, "checkedAt": at }),
                    None => Value::Null,
                },
                "gsiConfig": misc.gsi_config_notice,
                "game": misc.game_notice,
                "warnings": public_settings["warnings"],
            },
        });
        if let (Some(target), Value::Object(fields)) = (view.as_object_mut(), extra) {
            target.extend(fields);
        }
        view
    }

    pub fn state_snapshot(&self) -> Value {
        self.build_state()
    }

    fn window_visible(&self, label: &str) -> bool {
        self.app.get_webview_window(label).is_some_and(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
    }

    /// Pushes state to visible windows whose last copy differs; `force` windows always get it.
    pub fn flush(&self, force: &[&str]) {
        // Visibility checks wait for the UI thread, so they run before any lock is taken.
        let targets: Vec<&'static str> = WINDOWS.into_iter().filter(|label| force.contains(label) || self.window_visible(label)).collect();
        let mut publisher = self.publisher.lock().unwrap();
        publisher.last_flush = Some(tokio::time::Instant::now());
        if targets.is_empty() {
            return;
        }
        let state = self.build_state();
        let Ok(json) = serde_json::to_string(&state) else { return };
        for label in targets {
            if !force.contains(&label) && publisher.last.get(label) == Some(&json) {
                continue;
            }
            publisher.last.insert(label, json.clone());
            if let Ok(raw) = serde_json::value::RawValue::from_string(json.clone()) {
                let _ = self.app.emit_to(label, "state:update", raw);
            }
        }
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// A Steam avatar JPEG as RGB.
fn decode_jpeg(bytes: &[u8]) -> Option<crate::scoreboard::Avatar> {
    let mut decoder = zune_jpeg::JpegDecoder::new(bytes);
    let rgb = decoder.decode().ok()?;
    let (width, height) = decoder.dimensions()?;
    (width > 0 && height > 0 && rgb.len() == width * height * 3).then(|| crate::scoreboard::Avatar { steam_id: String::new(), width, height, rgb })
}
