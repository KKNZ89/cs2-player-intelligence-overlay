//! Updates from the GitHub releases feed, another https:// feed, or a local release folder. Every update is signed; the updater refuses
//! anything not signed with this app's key. Downloads run in the background and install when you choose
//! Restart to update, or when you quit. The app never restarts by itself.
//!
//! The updater only downloads over HTTP, so a local folder is served on loopback. Only latest.json and
//! the installer files next to it are exposed.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_updater::{Update, UpdaterExt};

const CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    pub status: String,
    pub version: String,
    pub percent: Option<u32>,
    pub message: String,
}

impl UpdateState {
    fn new(status: &str) -> Self {
        Self { status: status.into(), ..Self::default() }
    }
}

/// latest.json from a release folder with its download links pointing at the loopback server.
pub fn rewrite_manifest(manifest: &str, base: &str) -> Result<Value, String> {
    let mut json: Value = serde_json::from_str(manifest).map_err(|e| format!("latest.json is invalid: {e}"))?;
    let platforms = json.get_mut("platforms").and_then(Value::as_object_mut).ok_or("latest.json has no platforms.")?;
    for platform in platforms.values_mut() {
        let url = platform["url"].as_str().unwrap_or_default();
        let name = url.rsplit(['/', '\\']).next().unwrap_or_default();
        let decoded = percent_encoding::percent_decode_str(name).decode_utf8_lossy().into_owned();
        let encoded = percent_encoding::utf8_percent_encode(&decoded, percent_encoding::NON_ALPHANUMERIC);
        platform["url"] = Value::from(format!("{base}/files/{encoded}"));
    }
    Ok(json)
}

/// Only installer files directly inside the release folder are served.
pub fn served_file(folder: &Path, name: &str) -> Option<PathBuf> {
    let allowed = !name.is_empty()
        && !name.contains(['/', '\\', ':'])
        && name != ".."
        && [".exe", ".msi", ".zip", ".sig"].iter().any(|ext| name.to_ascii_lowercase().ends_with(ext));
    let file = folder.join(name);
    (allowed && file.is_file()).then_some(file)
}

struct LocalServer {
    url: String,
    shutdown: tokio::sync::oneshot::Sender<()>,
}

async fn serve_folder(folder: PathBuf) -> Result<LocalServer, String> {
    use axum::extract::{Path as UrlPath, State};
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let url = format!("http://127.0.0.1:{}", listener.local_addr().map_err(|e| e.to_string())?.port());
    let state = (Arc::new(folder), Arc::new(url.clone()));
    let router = axum::Router::new()
        .route(
            "/latest.json",
            get(|State((folder, base)): State<(Arc<PathBuf>, Arc<String>)>| async move {
                let text = tokio::fs::read_to_string(folder.join("latest.json")).await.map_err(|_| StatusCode::NOT_FOUND)?;
                let json = rewrite_manifest(&text, &base).map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
                Ok::<_, StatusCode>(([(header::CACHE_CONTROL, "no-store")], axum::Json(json)))
            }),
        )
        .route(
            "/files/{name}",
            get(|State((folder, _)): State<(Arc<PathBuf>, Arc<String>)>, UrlPath(name): UrlPath<String>| async move {
                let file = served_file(&folder, &name).ok_or(StatusCode::NOT_FOUND)?;
                let bytes = tokio::fs::read(file).await.map_err(|_| StatusCode::NOT_FOUND)?;
                Ok::<_, StatusCode>(([(header::CACHE_CONTROL, "no-store")], bytes).into_response())
            }),
        )
        .with_state(state);
    let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await;
    });
    Ok(LocalServer { url, shutdown })
}

struct Inner {
    state: UpdateState,
    source: String,
    server: Option<LocalServer>,
    timer: Option<tokio::task::AbortHandle>,
    ready: Option<(Update, Vec<u8>)>,
    last_error: String,
}

type Notify = Arc<dyn Fn() + Send + Sync>;
type Log = Arc<dyn Fn(&str) + Send + Sync>;

pub struct UpdateService {
    app: AppHandle,
    enabled: bool,
    inner: Mutex<Inner>,
    checking: tokio::sync::Mutex<()>,
    on_change: Notify,
    on_error: Log,
}

impl UpdateService {
    /// Updates run only in installed release builds.
    pub fn new(app: AppHandle, on_change: Notify, on_error: Log) -> Arc<Self> {
        let enabled = !cfg!(debug_assertions);
        let state = UpdateState::new(if enabled { "not-configured" } else { "disabled" });
        Arc::new(Self {
            app,
            enabled,
            inner: Mutex::new(Inner { state, source: String::new(), server: None, timer: None, ready: None, last_error: String::new() }),
            checking: tokio::sync::Mutex::new(()),
            on_change,
            on_error,
        })
    }

    pub fn view(&self) -> Value {
        let inner = self.inner.lock().unwrap();
        let mut view = serde_json::to_value(&inner.state).unwrap_or(Value::Null);
        view["source"] = inner.source.clone().into();
        view["currentVersion"] = self.app.package_info().version.to_string().into();
        view
    }

    fn set(&self, state: UpdateState) {
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.state.status == "ready" && state.status != "ready" {
                return;
            }
            if state.status != "error" {
                inner.last_error.clear();
            }
            inner.state = state;
        }
        (self.on_change)();
    }

    fn fail(&self, message: String) {
        let message: String = message.lines().next().unwrap_or("Unknown update error").chars().take(240).collect();
        // Each distinct failure is logged once, however often the periodic check repeats it.
        let repeated = self.inner.lock().unwrap().last_error == message;
        self.set(UpdateState { message: message.clone(), ..UpdateState::new("error") });
        self.inner.lock().unwrap().last_error = message.clone();
        if !repeated {
            (self.on_error)(&message);
        }
    }

    /// Switches the update source; checks now and every 30 minutes until an update is ready.
    pub fn configure(self: &Arc<Self>, source: &str) {
        if !self.enabled {
            return;
        }
        let source = source.trim().to_string();
        {
            let mut inner = self.inner.lock().unwrap();
            if source == inner.source && (inner.timer.is_some() || source.is_empty()) {
                return;
            }
            inner.source = source.clone();
            if let Some(timer) = inner.timer.take() {
                timer.abort();
            }
            if let Some(server) = inner.server.take() {
                let _ = server.shutdown.send(());
            }
            if inner.state.status == "ready" {
                return;
            }
        }
        if source.is_empty() {
            self.set(UpdateState::new("not-configured"));
            return;
        }
        let service = self.clone();
        let timer = tauri::async_runtime::spawn(async move {
            loop {
                service.check().await;
                tokio::time::sleep(CHECK_INTERVAL).await;
            }
        });
        self.inner.lock().unwrap().timer = Some(timer.inner().abort_handle());
    }

    async fn endpoint(&self, source: &str) -> Result<url::Url, String> {
        if source.to_ascii_lowercase().starts_with("https://") {
            let url = if source.to_ascii_lowercase().ends_with(".json") { source.to_string() } else { format!("{}/latest.json", source.trim_end_matches('/')) };
            return url.parse().map_err(|e: url::ParseError| e.to_string());
        }
        let folder = PathBuf::from(source);
        if !folder.is_dir() {
            return Err(format!("Update folder not found: {source}"));
        }
        let existing = self.inner.lock().unwrap().server.as_ref().map(|s| s.url.clone());
        let base = match existing {
            Some(url) => url,
            None => {
                let server = serve_folder(folder).await?;
                let url = server.url.clone();
                self.inner.lock().unwrap().server = Some(server);
                url
            }
        };
        format!("{base}/latest.json").parse().map_err(|e: url::ParseError| e.to_string())
    }

    pub async fn check(&self) -> Value {
        if !self.enabled {
            return self.view();
        }
        let Ok(_turn) = self.checking.try_lock() else {
            // A check is already running; report its outcome.
            let _wait = self.checking.lock().await;
            return self.view();
        };
        let (status, source) = {
            let inner = self.inner.lock().unwrap();
            (inner.state.status.clone(), inner.source.clone())
        };
        if matches!(status.as_str(), "downloading" | "ready") {
            return self.view();
        }
        if source.is_empty() {
            self.set(UpdateState::new("not-configured"));
            return self.view();
        }
        self.set(UpdateState::new("checking"));
        if let Err(error) = self.check_and_download(&source).await {
            self.fail(error);
        }
        self.view()
    }

    async fn check_and_download(&self, source: &str) -> Result<(), String> {
        let endpoint = self.endpoint(source).await?;
        let updater = self.app.updater_builder().endpoints(vec![endpoint]).map_err(|e| e.to_string())?.build().map_err(|e| e.to_string())?;
        let found = match updater.check().await {
            Ok(found) => found,
            // No latest.json at the source: nothing has been published there yet.
            Err(tauri_plugin_updater::Error::ReleaseNotFound) => {
                self.set(UpdateState { message: "No release is published yet.".into(), ..UpdateState::new("up-to-date") });
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        };
        let Some(update) = found else {
            self.set(UpdateState::new("up-to-date"));
            return Ok(());
        };
        let version = update.version.clone();
        self.set(UpdateState { version: version.clone(), percent: Some(0), ..UpdateState::new("downloading") });
        let mut received = 0usize;
        let mut last_percent = 0u32;
        let bytes = update
            .download(
                |chunk, total| {
                    received += chunk;
                    if let Some(total) = total.filter(|t| *t > 0) {
                        let percent = ((received as f64 / total as f64) * 100.0).min(100.0) as u32;
                        if percent != last_percent {
                            last_percent = percent;
                            self.set(UpdateState { version: version.clone(), percent: Some(percent), ..UpdateState::new("downloading") });
                        }
                    }
                },
                || {},
            )
            .await
            .map_err(|e| e.to_string())?;
        {
            let mut inner = self.inner.lock().unwrap();
            inner.ready = Some((update, bytes));
            if let Some(timer) = inner.timer.take() {
                timer.abort();
            }
        }
        self.set(UpdateState { version, ..UpdateState::new("ready") });
        Ok(())
    }

    /// Runs the downloaded installer (passive) and exits; the installer restarts the app.
    pub fn install(&self) -> Result<(), String> {
        let ready = self.inner.lock().unwrap().ready.take();
        let (update, bytes) = ready.ok_or("No downloaded update is ready to install.")?;
        update.install(bytes).map_err(|e| e.to_string())
    }

    pub fn is_ready(&self) -> bool {
        self.inner.lock().unwrap().ready.is_some()
    }

    /// Installs a downloaded update while the app quits, without restarting it.
    pub fn install_on_quit(&self) {
        let ready = self.inner.lock().unwrap().ready.take();
        if let Some((update, bytes)) = ready {
            let _ = update.restart_after_install(false).install(bytes);
        }
    }
}

const QUIET_RESTART: &str = "restart-in-tray";

/// Before an automatic update: the restarted app should come back in the notification area.
pub fn mark_quiet_restart(data_dir: &Path) {
    let _ = std::fs::write(data_dir.join(QUIET_RESTART), b"");
}

/// At startup: whether this start follows an automatic update (the marker is removed).
pub fn take_quiet_restart(data_dir: &Path) -> bool {
    std::fs::remove_file(data_dir.join(QUIET_RESTART)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_point_at_the_loopback_server() {
        let manifest = r#"{"version":"0.7.1","platforms":{"windows-x86_64":{"signature":"sig","url":"https://example.invalid/CS2%20Player%20Intel_0.7.1_x64-setup.exe"}}}"#;
        let json = rewrite_manifest(manifest, "http://127.0.0.1:5000").unwrap();
        assert_eq!(json["platforms"]["windows-x86_64"]["url"], "http://127.0.0.1:5000/files/CS2%20Player%20Intel%5F0%2E7%2E1%5Fx64%2Dsetup%2Eexe");
        assert_eq!(json["platforms"]["windows-x86_64"]["signature"], "sig");
        assert!(rewrite_manifest("{}", "x").is_err());
    }

    #[test]
    fn serves_only_installers_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("setup.exe"), b"x").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        assert!(served_file(dir.path(), "setup.exe").is_some());
        assert!(served_file(dir.path(), "notes.txt").is_none());
        assert!(served_file(dir.path(), "../setup.exe").is_none());
        assert!(served_file(dir.path(), "missing.exe").is_none());
    }

    #[test]
    fn quiet_restart_marker_is_used_once() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!take_quiet_restart(dir.path()));
        mark_quiet_restart(dir.path());
        assert!(take_quiet_restart(dir.path()));
        assert!(!take_quiet_restart(dir.path()), "the next normal start opens the window again");
    }
}
