//! settings.json in the app data folder. API keys are encrypted with Windows DPAPI (current user), so
//! the file alone does not reveal them. Every change is validated before anything is written.

use regex::Regex;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const LAYOUTS: [&str; 3] = ["top", "left", "right"];
pub const TRIGGERS: [&str; 2] = ["hold-tab", "f8"];
pub const GROUPS: [&str; 6] = ["performance", "reputation", "faceit", "map", "leetify", "steam"];
const DEFAULT_GROUPS: [&str; 4] = ["performance", "reputation", "faceit", "map"];
const SECRET_KEYS: [&str; 3] = ["csrepApiKey", "steamWebApiKey", "faceitApiKey"];
pub const THEMES: [&str; 2] = ["dark", "light"];
pub const OVERLAY_STYLES: [&str; 2] = ["standard", "minimal"];
/// Columns the compact overlay can show, in the order chosen.
pub const OVERLAY_COLUMNS: [&str; 10] = ["hours", "premier", "faceit", "aim", "ttd", "hs", "kd", "win", "csrep", "profile"];
const DEFAULT_OVERLAY_COLUMNS: [&str; 7] = ["hours", "premier", "faceit", "aim", "ttd", "hs", "profile"];
/// Sources that publish the same kind of value; the first with a value wins. API sources come first by
/// default, then values read from page text.
pub const SOURCES: [&str; 4] = ["faceit", "leetify", "csstats", "csrep"];
pub const NOTIFY_TYPES: [&str; 4] = ["encounter", "strong", "review", "provider"];
const DEFAULT_NOTIFY_TYPES: [&str; 3] = ["encounter", "review", "provider"];
const PREFIX: &str = "dpapi:";

/// Optional modifiers plus one key, as the dashboard records them (for example "Ctrl+Shift+F8").
static ACCELERATOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^((CommandOrControl|Control|Ctrl|Alt|Shift|Super)\+)*(F([1-9]|1[0-9]|2[0-4])|[A-Z0-9]|Space|Tab|Insert|Delete|Home|End|PageUp|PageDown|Up|Down|Left|Right|num[0-9]|numadd|numsub|nummult|numdiv|numdec|Plus|[`\-=\[\];',./\\])$").unwrap()
});

pub fn valid_accelerator(value: &str) -> bool {
    ACCELERATOR.is_match(value)
}

/// The same shortcut in the global-hotkey syntax Tauri registers.
pub fn to_hotkey(accelerator: &str) -> String {
    accelerator
        .split('+')
        .map(|part| match part {
            "Plus" => "Equal",
            "numsub" => "NumSubtract",
            "nummult" => "NumMultiply",
            "numdiv" => "NumDivide",
            "numdec" => "NumDecimal",
            other => other,
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Encrypts secrets for the current Windows user. Tests use a stand-in.
pub trait Protector: Send + Sync {
    fn protect(&self, data: &[u8]) -> Result<Vec<u8>, String>;
    fn unprotect(&self, data: &[u8]) -> Result<Vec<u8>, String>;
}

#[cfg(windows)]
pub struct Dpapi;

#[cfg(windows)]
impl Dpapi {
    fn run(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY: input points to live memory for the call; DPAPI allocates output with LocalAlloc, which
        // is copied and then released exactly once.
        unsafe {
            let result = if protect {
                CryptProtectData(&input, windows::core::w!("CS2 Player Intel"), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
            } else {
                CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
            };
            result.map_err(|error| error.message())?;
            let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            LocalFree(Some(HLOCAL(output.pbData.cast())));
            Ok(bytes)
        }
    }
}

#[cfg(windows)]
impl Protector for Dpapi {
    fn protect(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        Self::run(data, true)
    }
    fn unprotect(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        Self::run(data, false)
    }
}

/// Resolved settings with defaults applied and secrets decrypted.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub csrep_api_key: String,
    pub steam_web_api_key: String,
    pub faceit_api_key: String,
    pub theme: String,
    pub overlay_style: String,
    pub overlay_columns: Vec<String>,
    /// Hides names, SteamIDs and avatars (streaming).
    pub privacy_mode: bool,
    /// This app's unvalidated win estimate, off by default.
    pub show_win_estimate: bool,
    pub profile_indicators: bool,
    /// Leetify matches a player needs before tendencies and indicators are shown.
    pub min_sample_matches: f64,
    pub notifications: bool,
    pub notify_types: Vec<String>,
    /// Days of match history kept (0 = all).
    pub retention_days: f64,
    pub source_priority: Vec<String>,
    /// Minutes a provider result is reused for a player met again (0 = always fetch).
    pub cache_minutes: f64,
    pub overlay_opacity: f64,
    pub overlay_trigger: String,
    pub overlay_hotkey: String,
    /// Toggles the clickable overlay directly, for when the Esc-menu detection is wrong.
    pub interact_hotkey: String,
    pub esc_interactive: bool,
    pub expanded_groups: Vec<String>,
    pub auto_retry_minutes: f64,
    pub steamworks_sdk_path: String,
    pub cs2_cfg_path: String,
    pub cs2_console_log_path: String,
    pub update_source: String,
    /// Reading CSStats and CSRep profile pages in hidden windows is opt-in: those sites publish no API
    /// for it, so the user decides after checking their terms.
    pub csstats_enabled: bool,
    pub csrep_pages_enabled: bool,
    /// Read teammate colours from CS2's scoreboard (window capture while Tab is held). Opt-in.
    pub scoreboard_colours: bool,
    /// Install a downloaded update by itself when nothing is in use (CS2 closed, window closed).
    pub auto_install_updates: bool,
    pub launch_cs2_on_start: bool,
    /// Closing the dashboard keeps the app running in the notification area.
    pub close_to_tray: bool,
    pub start_with_windows: bool,
    pub overlay_display: String,
    pub overlay_layout: String,
    pub overlay_x: f64,
    pub overlay_y: f64,
    pub overlay_scale: f64,
}

pub struct SettingsStore {
    file: PathBuf,
    data: Map<String, Value>,
    protector: Box<dyn Protector>,
    pub load_error: String,
    key_error: String,
    cache: Option<Settings>,
}

fn text(data: &Map<String, Value>, key: &str) -> String {
    data.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn number(data: &Map<String, Value>, key: &str, default: f64) -> f64 {
    data.get(key).and_then(Value::as_f64).unwrap_or(default)
}

fn flag(data: &Map<String, Value>, key: &str) -> bool {
    data.get(key) != Some(&Value::Bool(false))
}

impl SettingsStore {
    pub fn open(directory: &Path, protector: Box<dyn Protector>) -> Self {
        let file = directory.join("settings.json");
        let mut store = Self { file, data: Map::new(), protector, load_error: String::new(), key_error: String::new(), cache: None };
        store.data = store.read();
        store
    }

    fn read(&mut self) -> Map<String, Value> {
        let Ok(text) = std::fs::read_to_string(&self.file) else { return Map::new() };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(data)) => data,
            other => {
                // Keep the unreadable file for inspection instead of silently overwriting it with defaults.
                let reason = other.err().map(|e| e.to_string()).unwrap_or_else(|| "it does not contain an object".into());
                let backup = self.file.with_extension(format!("json.corrupt-{}", crate::model::now_ms()));
                let _ = std::fs::rename(&self.file, &backup);
                self.load_error = format!("Settings could not be read ({reason}); defaults restored. Previous file: {}", backup.display());
                Map::new()
            }
        }
    }

    fn write(&mut self) -> std::io::Result<()> {
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = self.file.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(&self.data)?)?;
        std::fs::rename(&temporary, &self.file)?;
        self.cache = None;
        Ok(())
    }

    fn encrypt(&self, value: &str) -> Result<String, String> {
        if value.is_empty() {
            return Ok(String::new());
        }
        use base64::Engine;
        let sealed = self.protector.protect(value.as_bytes()).map_err(|e| format!("Secure key storage is unavailable ({e}). The key was not saved."))?;
        Ok(format!("{PREFIX}{}", base64::engine::general_purpose::STANDARD.encode(sealed)))
    }

    fn decrypt(&mut self, key: &str) -> String {
        let value = text(&self.data, key);
        let Some(encoded) = value.strip_prefix(PREFIX) else { return String::new() };
        use base64::Engine;
        let opened = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| e.to_string())
            .and_then(|sealed| self.protector.unprotect(&sealed))
            .and_then(|plain| String::from_utf8(plain).map_err(|e| e.to_string()));
        opened.unwrap_or_else(|error| {
            self.key_error = format!("A saved API key could not be decrypted ({error}). Enter it again.");
            String::new()
        })
    }

    /// Decryption uses the OS key store, so the result is cached until the next write.
    pub fn get(&mut self) -> Settings {
        if let Some(cached) = &self.cache {
            return cached.clone();
        }
        self.key_error.clear();
        let d = &self.data;
        let trigger = text(d, "overlayTrigger");
        let hotkey = text(d, "overlayHotkey");
        let layout = text(d, "overlayLayout");
        let groups = match d.get("expandedGroups").and_then(Value::as_array) {
            Some(list) => list.iter().filter_map(Value::as_str).filter(|g| GROUPS.contains(g)).map(String::from).collect(),
            None => DEFAULT_GROUPS.iter().map(|g| g.to_string()).collect(),
        };
        let list = |key: &str, allowed: &[&str], default: &[&str]| -> Vec<String> {
            match d.get(key).and_then(Value::as_array) {
                Some(items) => items.iter().filter_map(Value::as_str).filter(|v| allowed.contains(v)).map(String::from).collect(),
                None => default.iter().map(|v| v.to_string()).collect(),
            }
        };
        let choice = |key: &str, allowed: &[&str]| -> String {
            let value = text(d, key);
            if allowed.contains(&value.as_str()) {
                value
            } else {
                allowed[0].to_string()
            }
        };
        let mut settings = Settings {
            csrep_api_key: String::new(),
            steam_web_api_key: String::new(),
            faceit_api_key: String::new(),
            theme: choice("theme", &THEMES),
            overlay_style: choice("overlayStyle", &OVERLAY_STYLES),
            overlay_columns: list("overlayColumns", &OVERLAY_COLUMNS, &DEFAULT_OVERLAY_COLUMNS),
            privacy_mode: d.get("privacyMode") == Some(&Value::Bool(true)),
            show_win_estimate: d.get("showWinEstimate") == Some(&Value::Bool(true)),
            profile_indicators: flag(d, "profileIndicators"),
            min_sample_matches: number(d, "minSampleMatches", 30.0),
            notifications: flag(d, "notifications"),
            notify_types: list("notifyTypes", &NOTIFY_TYPES, &DEFAULT_NOTIFY_TYPES),
            retention_days: number(d, "retentionDays", 0.0),
            source_priority: {
                // Unknown names are dropped and missing sources keep their default place at the end.
                let mut order = list("sourcePriority", &SOURCES, &SOURCES);
                for source in SOURCES {
                    if !order.iter().any(|s| s == source) {
                        order.push(source.to_string());
                    }
                }
                order
            },
            cache_minutes: number(d, "cacheMinutes", 30.0),
            overlay_opacity: number(d, "overlayOpacity", 0.92),
            overlay_trigger: if TRIGGERS.contains(&trigger.as_str()) { trigger } else { "hold-tab".into() },
            overlay_hotkey: if valid_accelerator(&hotkey) { hotkey } else { "F8".into() },
            interact_hotkey: {
                let interact = text(d, "interactHotkey");
                if valid_accelerator(&interact) {
                    interact
                } else {
                    "Shift+F8".into()
                }
            },
            esc_interactive: flag(d, "escInteractive"),
            expanded_groups: groups,
            auto_retry_minutes: number(d, "autoRetryMinutes", 2.0),
            steamworks_sdk_path: text(d, "steamworksSdkPath"),
            cs2_cfg_path: text(d, "cs2CfgPath"),
            cs2_console_log_path: text(d, "cs2ConsoleLogPath"),
            update_source: text(d, "updateSource"),
            csstats_enabled: d.get("csstatsEnabled") == Some(&Value::Bool(true)),
            csrep_pages_enabled: d.get("csrepPagesEnabled") == Some(&Value::Bool(true)),
            scoreboard_colours: d.get("scoreboardColours") == Some(&Value::Bool(true)),
            auto_install_updates: flag(d, "autoInstallUpdates"),
            launch_cs2_on_start: flag(d, "launchCs2OnStart"),
            close_to_tray: flag(d, "closeToTray"),
            start_with_windows: d.get("startWithWindows") == Some(&Value::Bool(true)),
            overlay_display: text(d, "overlayDisplay"),
            overlay_layout: if LAYOUTS.contains(&layout.as_str()) { layout } else { "right".into() },
            overlay_x: number(d, "overlayX", 0.0),
            overlay_y: number(d, "overlayY", 36.0),
            overlay_scale: number(d, "overlayScale", 1.0),
        };
        settings.csrep_api_key = self.decrypt("csrepApiKey");
        settings.steam_web_api_key = self.decrypt("steamWebApiKey");
        settings.faceit_api_key = self.decrypt("faceitApiKey");
        self.cache = Some(settings.clone());
        settings
    }

    pub fn public_view(&mut self) -> Value {
        let s = self.get();
        let warnings: Vec<&String> = [&self.load_error, &self.key_error].into_iter().filter(|w| !w.is_empty()).collect();
        json!({
            "hasCsrepApiKey": !s.csrep_api_key.is_empty(),
            "hasSteamWebApiKey": !s.steam_web_api_key.is_empty(),
            "hasFaceitApiKey": !s.faceit_api_key.is_empty(),
            // Lets you tell which key is saved without showing it.
            "keyFingerprints": {
                "csrepApiKey": fingerprint(&s.csrep_api_key),
                "steamWebApiKey": fingerprint(&s.steam_web_api_key),
                "faceitApiKey": fingerprint(&s.faceit_api_key),
            },
            "theme": s.theme,
            "overlayStyle": s.overlay_style,
            "overlayColumns": s.overlay_columns,
            "privacyMode": s.privacy_mode,
            "showWinEstimate": s.show_win_estimate,
            "profileIndicators": s.profile_indicators,
            "minSampleMatches": s.min_sample_matches,
            "notifications": s.notifications,
            "notifyTypes": s.notify_types,
            "retentionDays": s.retention_days,
            "sourcePriority": s.source_priority,
            "cacheMinutes": s.cache_minutes,
            "overlayOpacity": s.overlay_opacity,
            "overlayTrigger": s.overlay_trigger,
            "overlayHotkey": s.overlay_hotkey,
            "interactHotkey": s.interact_hotkey,
            "escInteractive": s.esc_interactive,
            "expandedGroups": s.expanded_groups,
            "autoRetryMinutes": s.auto_retry_minutes,
            "steamworksSdkPath": s.steamworks_sdk_path,
            "cs2CfgPath": s.cs2_cfg_path,
            "cs2ConsoleLogPath": s.cs2_console_log_path,
            "updateSource": s.update_source,
            "csstatsEnabled": s.csstats_enabled,
            "csrepPagesEnabled": s.csrep_pages_enabled,
            "scoreboardColours": s.scoreboard_colours,
            "autoInstallUpdates": s.auto_install_updates,
            "launchCs2OnStart": s.launch_cs2_on_start,
            "closeToTray": s.close_to_tray,
            "startWithWindows": s.start_with_windows,
            "overlayDisplay": s.overlay_display,
            "overlayLayout": s.overlay_layout,
            "overlayX": s.overlay_x,
            "overlayY": s.overlay_y,
            "overlayScale": s.overlay_scale,
            "warnings": warnings,
        })
    }

    /// Shared with CS2's GSI config so other local software cannot post game state.
    pub fn gsi_token(&mut self) -> String {
        let current = text(&self.data, "gsiToken");
        if current.len() == 32 && current.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return current;
        }
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).expect("the OS random number generator is available");
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        self.data.insert("gsiToken".into(), token.clone().into());
        let _ = self.write();
        token
    }

    /// Applies a partial update from the dashboard; nothing changes when any field is invalid.
    pub fn update(&mut self, input: &Map<String, Value>) -> Result<Value, String> {
        let previous = self.data.clone();
        let result = self.apply(input);
        if result.is_err() {
            self.data = previous;
            self.cache = None;
        }
        result
    }

    fn apply(&mut self, input: &Map<String, Value>) -> Result<Value, String> {
        let as_text = |value: &Value| match value {
            Value::String(s) => s.trim().to_string(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        for key in SECRET_KEYS {
            if let Some(value) = input.get(key) {
                let sealed = self.encrypt(&as_text(value))?;
                self.data.insert(key.into(), sealed.into());
            }
        }
        if let Some(value) = input.get("overlayTrigger") {
            let trigger = as_text(value);
            if !TRIGGERS.contains(&trigger.as_str()) {
                return Err("Overlay trigger must be hold-tab or f8.".into());
            }
            self.data.insert("overlayTrigger".into(), trigger.into());
        }
        if let Some(value) = input.get("overlayHotkey") {
            let hotkey = as_text(value);
            if !valid_accelerator(&hotkey) {
                return Err("Overlay shortcut must be a key such as F8, optionally with Ctrl, Alt or Shift.".into());
            }
            self.data.insert("overlayHotkey".into(), hotkey.into());
        }
        if let Some(value) = input.get("interactHotkey") {
            let hotkey = as_text(value);
            if !valid_accelerator(&hotkey) {
                return Err("The clickable-overlay shortcut must be a key such as Shift+F8, optionally with Ctrl, Alt or Shift.".into());
            }
            self.data.insert("interactHotkey".into(), hotkey.into());
        }
        if input.contains_key("overlayHotkey") || input.contains_key("interactHotkey") {
            self.cache = None;
            let s = self.get();
            if s.overlay_hotkey.eq_ignore_ascii_case(&s.interact_hotkey) {
                return Err("The pin and clickable-overlay shortcuts must be different keys.".into());
            }
        }
        for key in [
            "escInteractive",
            "csstatsEnabled",
            "csrepPagesEnabled",
            "scoreboardColours",
            "autoInstallUpdates",
            "launchCs2OnStart",
            "closeToTray",
            "startWithWindows",
            "privacyMode",
            "showWinEstimate",
            "profileIndicators",
            "notifications",
        ] {
            if let Some(value) = input.get(key) {
                let truthy = !matches!(value, Value::Bool(false) | Value::Null) && value != &json!(0) && value != &json!("");
                self.data.insert(key.into(), truthy.into());
            }
        }
        for (key, allowed) in [("theme", &THEMES[..]), ("overlayStyle", &OVERLAY_STYLES[..])] {
            if let Some(value) = input.get(key) {
                let choice = as_text(value);
                if !allowed.contains(&choice.as_str()) {
                    return Err(format!("{key} must be one of: {}.", allowed.join(", ")));
                }
                self.data.insert(key.into(), choice.into());
            }
        }
        for (key, allowed) in [("overlayColumns", &OVERLAY_COLUMNS[..]), ("notifyTypes", &NOTIFY_TYPES[..]), ("sourcePriority", &SOURCES[..])] {
            if let Some(value) = input.get(key) {
                let items = value.as_array().ok_or_else(|| format!("{key} must be a list."))?;
                let mut unique: Vec<String> = Vec::new();
                for item in items {
                    let name = item.as_str().filter(|v| allowed.contains(v)).ok_or_else(|| format!("{key} must be among: {}.", allowed.join(", ")))?;
                    if !unique.iter().any(|v| v == name) {
                        unique.push(name.into());
                    }
                }
                if key == "overlayColumns" && unique.is_empty() {
                    return Err("Choose at least one overlay column.".into());
                }
                self.data.insert(key.into(), unique.into());
            }
        }
        if let Some(value) = input.get("expandedGroups") {
            let groups = value.as_array().ok_or("Column groups must be a list.")?;
            let mut unique: Vec<String> = Vec::new();
            for group in groups {
                let name = group.as_str().filter(|g| GROUPS.contains(g)).ok_or_else(|| format!("Column groups must be among: {}.", GROUPS.join(", ")))?;
                if !unique.iter().any(|g| g == name) {
                    unique.push(name.into());
                }
            }
            self.data.insert("expandedGroups".into(), unique.into());
        }
        for key in ["steamworksSdkPath", "cs2CfgPath", "cs2ConsoleLogPath", "overlayDisplay"] {
            if let Some(value) = input.get(key) {
                self.data.insert(key.into(), as_text(value).into());
            }
        }
        if let Some(value) = input.get("updateSource") {
            let source = as_text(value);
            if !source.is_empty() && !source.to_ascii_lowercase().starts_with("https://") && !Path::new(&source).is_absolute() {
                return Err("Update source must be a local folder or an https:// URL.".into());
            }
            self.data.insert("updateSource".into(), source.into());
        }
        if let Some(value) = input.get("overlayLayout") {
            let layout = as_text(value);
            if !LAYOUTS.contains(&layout.as_str()) {
                return Err("Overlay layout must be top, left or right.".into());
            }
            self.data.insert("overlayLayout".into(), layout.into());
        }
        for (key, min, max) in [
            ("overlayX", -10000.0, 10000.0),
            ("overlayY", -10000.0, 10000.0),
            ("overlayScale", 0.5, 2.0),
            ("overlayOpacity", 0.3, 1.0),
            ("autoRetryMinutes", 0.0, 60.0),
            ("minSampleMatches", 0.0, 1000.0),
            ("retentionDays", 0.0, 3650.0),
            ("cacheMinutes", 0.0, 1440.0),
        ] {
            let Some(value) = input.get(key) else { continue };
            let parsed = match value {
                Value::Number(n) => n.as_f64(),
                Value::String(s) if !s.trim().is_empty() => s.trim().parse::<f64>().ok(),
                _ => None,
            };
            match parsed.filter(|n| n.is_finite() && *n >= min && *n <= max) {
                Some(n) => self.data.insert(key.into(), json!(n)),
                None => return Err(format!("Invalid {key} ({min} to {max}).")),
            };
        }
        self.write().map_err(|e| format!("Settings could not be saved: {e}"))?;
        Ok(self.public_view())
    }
}

/// The first 12 hex digits of the key's SHA-256 hash, or null when no key is saved. The key itself is
/// stored encrypted (it has to be sent to the service, so it cannot be stored as a hash).
pub fn fingerprint(key: &str) -> Value {
    use sha2::{Digest, Sha256};
    if key.is_empty() {
        return Value::Null;
    }
    let hash = Sha256::digest(key.as_bytes());
    let hex: String = hash.iter().take(6).map(|b| format!("{b:02x}")).collect();
    Value::from(format!("{}-{}-{}", &hex[..4], &hex[4..8], &hex[8..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Reverse;
    impl Protector for Reverse {
        fn protect(&self, data: &[u8]) -> Result<Vec<u8>, String> {
            Ok(data.iter().rev().copied().collect())
        }
        fn unprotect(&self, data: &[u8]) -> Result<Vec<u8>, String> {
            Ok(data.iter().rev().copied().collect())
        }
    }

    fn input(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn defaults_and_validated_updates() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::open(dir.path(), Box::new(Reverse));
        let s = store.get();
        assert_eq!((s.overlay_hotkey.as_str(), s.overlay_layout.as_str(), s.overlay_trigger.as_str()), ("F8", "right", "hold-tab"));
        assert!(s.esc_interactive && s.launch_cs2_on_start && s.close_to_tray);
        assert!(!s.csstats_enabled && !s.csrep_pages_enabled, "reading provider pages is opt-in");
        assert!(!s.scoreboard_colours, "screen capture is opt-in");
        assert!(s.auto_install_updates, "updates install by themselves by default");
        assert!(!s.start_with_windows, "starting with Windows is opt-in");
        assert_eq!(s.interact_hotkey, "Shift+F8");
        assert_eq!((s.theme.as_str(), s.overlay_style.as_str(), s.privacy_mode, s.show_win_estimate), ("dark", "standard", false, false));
        assert_eq!(s.overlay_columns, DEFAULT_OVERLAY_COLUMNS);
        assert!(store.update(&input(json!({ "overlayColumns": [] }))).is_err());
        assert!(store.update(&input(json!({ "overlayColumns": ["premier", "bogus"] }))).is_err());
        assert!(store.update(&input(json!({ "theme": "neon" }))).is_err());
        store
            .update(&input(json!({ "overlayColumns": ["premier", "hours", "premier"], "theme": "light", "faceitApiKey": "fk", "retentionDays": 90 })))
            .unwrap();
        assert_eq!(store.get().overlay_columns, ["premier", "hours"]);
        assert_eq!((store.get().theme.as_str(), store.get().faceit_api_key.as_str(), store.get().retention_days), ("light", "fk", 90.0));
        assert_eq!(store.get().source_priority, SOURCES, "API sources first by default");
        store.update(&input(json!({ "sourcePriority": ["csrep", "faceit"], "cacheMinutes": 0 }))).unwrap();
        assert_eq!(store.get().source_priority, ["csrep", "faceit", "leetify", "csstats"], "unlisted sources keep their place after the chosen ones");
        assert!(store.update(&input(json!({ "sourcePriority": ["steam"] }))).is_err());
        assert!(store.update(&input(json!({ "cacheMinutes": 5000 }))).is_err());
        assert!(store.update(&input(json!({ "interactHotkey": "F8" }))).unwrap_err().contains("different"), "the two shortcuts cannot clash");
        assert_eq!(s.expanded_groups, DEFAULT_GROUPS);

        let view =
            store.update(&input(json!({ "overlayHotkey": "Ctrl+Shift+I", "overlayScale": "1.5", "escInteractive": false, "csrepApiKey": " key " }))).unwrap();
        assert_eq!(view["overlayHotkey"], "Ctrl+Shift+I");
        assert_eq!(view["hasCsrepApiKey"], true);
        assert_eq!(view["keyFingerprints"]["csrepApiKey"], fingerprint("key"));
        assert!(view["keyFingerprints"]["steamWebApiKey"].is_null());
        // SHA-256("key") starts 2c70e12b7a06.
        assert_eq!(fingerprint("key"), "2c70-e12b-7a06");
        assert_eq!(store.get().csrep_api_key, "key");
        assert!(!store.get().esc_interactive);

        assert!(store.update(&input(json!({ "overlayX": 5, "overlayHotkey": "Ctrl+Shift" }))).is_err());
        assert_eq!(store.get().overlay_x, 0.0, "a rejected update changes nothing");
        assert!(store.update(&input(json!({ "overlayScale": 3 }))).is_err());
        assert!(store.update(&input(json!({ "updateSource": "relative/folder" }))).is_err());
        assert!(store.update(&input(json!({ "expandedGroups": ["map", "bogus"] }))).is_err());
        store.update(&input(json!({ "expandedGroups": ["map", "map", "steam"] }))).unwrap();

        let saved = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert!(saved.contains("dpapi:") && !saved.contains("\"key\""), "keys are stored encrypted");
        let mut reopened = SettingsStore::open(dir.path(), Box::new(Reverse));
        assert_eq!(reopened.get().expanded_groups, ["map", "steam"]);
        assert_eq!(reopened.get().overlay_scale, 1.5);
    }

    #[test]
    fn tokens_and_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("settings.json"), "{not json").unwrap();
        let mut store = SettingsStore::open(dir.path(), Box::new(Reverse));
        assert!(store.load_error.contains("defaults restored"));
        let token = store.gsi_token();
        assert_eq!(token.len(), 32);
        assert_eq!(store.gsi_token(), token, "the token is stable");
        assert_eq!(SettingsStore::open(dir.path(), Box::new(Reverse)).gsi_token(), token, "and survives a restart");
    }

    #[test]
    fn accelerators_map_to_hotkeys() {
        assert!(valid_accelerator("F8") && valid_accelerator("Ctrl+Alt+K") && valid_accelerator("Shift+numadd"));
        assert!(!valid_accelerator("Ctrl+") && !valid_accelerator("Hyper+K"));
        assert_eq!(to_hotkey("Ctrl+Plus"), "Ctrl+Equal");
        assert_eq!(to_hotkey("Alt+numsub"), "Alt+NumSubtract");
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_round_trips() {
        let sealed = Dpapi.protect(b"secret").unwrap();
        assert_ne!(sealed, b"secret");
        assert_eq!(Dpapi.unprotect(&sealed).unwrap(), b"secret");
    }
}
