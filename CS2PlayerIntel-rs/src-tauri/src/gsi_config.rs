//! Finds the CS2 install (Steam registry entry plus every Steam library) and writes the GSI config.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const GSI_FILE_NAME: &str = "gamestate_integration_cs2playerintel.cfg";
const CFG_SUFFIX: [&str; 6] = ["steamapps", "common", "Counter-Strike Global Offensive", "game", "csgo", "cfg"];

fn existing_dir(path: PathBuf) -> Option<PathBuf> {
    path.is_dir().then_some(path)
}

fn steam_root() -> Option<PathBuf> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        #[cfg(windows)]
        {
            let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam").ok()?;
            let path: String = key.get_value("SteamPath").ok()?;
            Some(PathBuf::from(path.replace('/', "\\")))
        }
        #[cfg(not(windows))]
        None
    })
    .clone()
}

/// Library folders listed in Steam's libraryfolders.vdf, plus Steam's own folder.
pub fn library_roots(steam_root: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let Some(root) = steam_root else { return roots };
    roots.push(root.to_path_buf());
    if let Ok(text) = std::fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf")) {
        roots.extend(library_paths(&text).into_iter().map(PathBuf::from));
    }
    roots.dedup();
    roots
}

pub fn library_paths(vdf: &str) -> Vec<String> {
    let pattern = regex::Regex::new(r#""path"\s+"([^"]+)""#).unwrap();
    pattern.captures_iter(vdf).map(|c| c[1].replace("\\\\", "\\")).collect()
}

pub fn find_cs2_cfg_path(explicit: &str) -> Option<PathBuf> {
    if !explicit.is_empty() {
        return existing_dir(PathBuf::from(explicit));
    }
    let mut candidates: Vec<PathBuf> =
        library_roots(steam_root().as_deref()).into_iter().map(|root| CFG_SUFFIX.iter().fold(root, |p, part| p.join(part))).collect();
    for base in [r"C:\Program Files (x86)\Steam", r"C:\Program Files\Steam"] {
        candidates.push(CFG_SUFFIX.iter().fold(PathBuf::from(base), |p, part| p.join(part)));
    }
    candidates.into_iter().find_map(existing_dir)
}

/// console.log next to the cfg folder (written only when CS2 runs with -condebug).
pub fn find_console_log_path(explicit: &str, cfg_path: &str) -> Option<PathBuf> {
    if !explicit.is_empty() {
        return Some(PathBuf::from(explicit));
    }
    find_cs2_cfg_path(cfg_path).and_then(|cfg| cfg.parent().map(|game| game.join("console.log")))
}

/// CS2's own steam_api64.dll (game\bin\win64).
pub fn find_cs2_steam_runtime(cfg_path: &str) -> Option<PathBuf> {
    let cfg = find_cs2_cfg_path(cfg_path)?;
    let file = cfg.parent()?.parent()?.join("bin").join("win64").join("steam_api64.dll");
    file.is_file().then_some(file)
}

/// Fallback when CS2's own runtime is not found: steam_api64.dll from a Steamworks SDK folder.
pub fn find_sdk_steam_runtime(sdk_path: &str) -> Option<PathBuf> {
    if sdk_path.is_empty() {
        return None;
    }
    let base = PathBuf::from(sdk_path);
    [base.clone(), base.join("redistributable_bin").join("win64"), base.join("sdk").join("redistributable_bin").join("win64")]
        .into_iter()
        .map(|dir| dir.join("steam_api64.dll"))
        .find(|file| file.is_file())
}

pub fn config_content(port: u16, token: &str) -> String {
    format!(
        r#""CS2 Player Intel"
{{
  "uri" "http://127.0.0.1:{port}/"
  "timeout" "5.0"
  "buffer" "0.1"
  "throttle" "0.2"
  "heartbeat" "5.0"
  "auth"
  {{
    "token" "{token}"
  }}
  "data"
  {{
    "provider" "1"
    "map" "1"
    "round" "1"
    "player_id" "1"
    "player_match_stats" "1"
  }}
}}
"#
    )
}

pub fn install(cfg_path: &str, port: u16, token: &str) -> Result<PathBuf, String> {
    let dir = find_cs2_cfg_path(cfg_path).ok_or("CS2 cfg directory was not found. Set it manually in Settings.")?;
    let file = dir.join(GSI_FILE_NAME);
    std::fs::write(&file, config_content(port, token)).map_err(|e| format!("Could not write {}: {e}", file.display()))?;
    Ok(file)
}

/// Rewrites a previously installed config whose port or token is outdated. Never creates one.
pub fn refresh_installed(cfg_path: &str, port: u16, token: &str) -> Result<Option<PathBuf>, String> {
    let Some(dir) = find_cs2_cfg_path(cfg_path) else { return Ok(None) };
    let Ok(current) = std::fs::read_to_string(dir.join(GSI_FILE_NAME)) else { return Ok(None) };
    if current == config_content(port, token) {
        return Ok(None);
    }
    install(&dir.to_string_lossy(), port, token).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_library_folders() {
        let vdf = r#""libraryfolders" { "0" { "path" "C:\\Program Files (x86)\\Steam" } "1" { "path" "D:\\SteamLibrary" } }"#;
        assert_eq!(library_paths(vdf), [r"C:\Program Files (x86)\Steam", r"D:\SteamLibrary"]);
    }

    #[test]
    fn installs_and_refreshes_only_existing_configs() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().to_string_lossy().to_string();
        assert_eq!(refresh_installed(&cfg, 31982, "a").unwrap(), None, "never creates a config");
        let file = install(&cfg, 31982, "a").unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().contains(r#""token" "a""#));
        assert_eq!(refresh_installed(&cfg, 31982, "a").unwrap(), None, "an up-to-date config is left alone");
        assert!(refresh_installed(&cfg, 31982, "b").unwrap().is_some());
        assert!(std::fs::read_to_string(&file).unwrap().contains(r#""token" "b""#));
        assert!(install(&dir.path().join("missing").to_string_lossy(), 31982, "a").is_err());
    }
}
