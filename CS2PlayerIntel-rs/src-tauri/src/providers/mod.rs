//! Player data sources. Each lookup answers with a [`ProviderResult`]: "ok" with fields, or a status
//! explaining why there is no data (shown as N/A, never as zero).

pub mod csrep;
pub mod csstats;
pub mod faceit;
pub mod leetify;
pub mod scraper;
pub mod steam_profile;

use crate::model::ProviderResult;
use serde_json::Value;
use std::time::Duration;

/// Lookup order and the field name of each provider on a player.
pub const PROVIDERS: [&str; 6] = ["csrep", "csstats", "leetify", "history", "steam", "faceit"];

/// Why a request produced no data.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub status: String,
    pub error: Option<String>,
}

impl Failure {
    pub fn new(status: &str) -> Self {
        Self { status: status.into(), error: None }
    }

    pub fn with(status: &str, error: impl Into<String>) -> Self {
        Self { status: status.into(), error: Some(error.into()) }
    }

    pub fn into_result(self, profile_url: Option<String>) -> ProviderResult {
        let mut result = ProviderResult { status: self.status, error: self.error, ..ProviderResult::default() };
        if let Some(url) = profile_url {
            result.data.insert("profileUrl".into(), url.into());
        }
        result
    }
}

/// Status for a failed HTTP request.
pub fn request_failure(error: &reqwest::Error) -> Failure {
    Failure::with(if error.is_timeout() { "timeout" } else { "error" }, error.to_string())
}

/// What lookups need from the settings and the current match, read when each lookup starts.
#[derive(Clone, Debug, Default)]
pub struct LookupContext {
    pub csrep_api_key: String,
    pub steam_web_api_key: String,
    pub faceit_api_key: String,
    pub csstats_enabled: bool,
    pub csrep_pages_enabled: bool,
    pub self_id: String,
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("CS2PlayerIntel/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(8))
        .build()
        .expect("the HTTP client configuration is valid")
}

/// A JSON number, or null for anything else (missing, text, NaN).
pub fn number(value: &Value) -> Value {
    value.as_f64().filter(|n| n.is_finite()).map(Value::from).unwrap_or(Value::Null)
}

pub struct Providers {
    pub leetify: leetify::LeetifyProvider,
    pub csrep: csrep::CsRepProvider,
    pub csstats: csstats::CsStatsProvider,
    pub steam: steam_profile::SteamProfileProvider,
    pub faceit: faceit::FaceitProvider,
    /// Browser profile for Leetify, Steam and FACEIT pages opened from the overlay.
    pub profile_dir: std::path::PathBuf,
}

impl Providers {
    pub fn new(app: tauri::AppHandle, data_dir: std::path::PathBuf) -> Self {
        let http = http_client();
        Self {
            leetify: leetify::LeetifyProvider::new(http.clone()),
            csrep: csrep::CsRepProvider::new(
                http.clone(),
                scraper::PageScraper::new(app.clone(), "csrep", data_dir.join("webview-csrep"), "CSRep - CS2 Player Intel"),
            ),
            csstats: csstats::CsStatsProvider::new(scraper::PageScraper::new(app, "csstats", data_dir.join("webview-csstats"), "CSStats - CS2 Player Intel")),
            faceit: faceit::FaceitProvider::new(http.clone()),
            steam: steam_profile::SteamProfileProvider::new(http),
            profile_dir: data_dir.join("webview-profiles"),
        }
    }

    pub async fn lookup(&self, provider: &str, steam_id: &str, context: &LookupContext) -> ProviderResult {
        match provider {
            "leetify" => self.leetify.profile(steam_id).await,
            "history" => self.leetify.shared_history(steam_id, &context.self_id).await,
            // Without an API key, CSRep data comes from its public pages, which the user has to allow.
            "csrep" if context.csrep_api_key.is_empty() && !context.csrep_pages_enabled => {
                ProviderResult::failed("disabled", "CSRep is off. Add a CSRep API key or allow reading its public pages in Settings → Data sources.")
            }
            "csrep" => self.csrep.player(steam_id, &context.csrep_api_key).await,
            "csstats" if !context.csstats_enabled => ProviderResult::failed("disabled", "CSStats is off. Turn it on in Settings → Data sources."),
            "csstats" => self.csstats.player(steam_id).await,
            "steam" => self.steam.player(steam_id, &context.steam_web_api_key).await,
            "faceit" => self.faceit.player(steam_id, &context.faceit_api_key).await,
            other => ProviderResult::failed("error", format!("Unknown provider {other}")),
        }
    }

    /// Forgets per-match caches.
    pub fn reset(&self) {
        self.leetify.reset();
    }
}
