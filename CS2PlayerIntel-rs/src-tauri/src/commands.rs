//! Commands the app's own pages call through `window.__TAURI__.core.invoke` (see ui/lib/bridge.js).
//! Each one refuses calls from any webview other than the dashboard and the overlay; provider pages
//! have no capabilities either.

use crate::engine::Engine;
use crate::player_data::Force;
use crate::providers::leetify;
use crate::roster_input::numeric_steam_id;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use tauri::{Manager, State, Webview};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

type Result<T> = std::result::Result<T, String>;

fn trusted(webview: &Webview) -> Result<()> {
    match webview.label() {
        "main" | crate::overlay::LABEL => Ok(()),
        other => Err(format!("Rejected a command from an untrusted page ({other}).")),
    }
}

#[tauri::command(async)]
pub fn state_get(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<Value> {
    trusted(&webview)?;
    if engine.smoke {
        engine.diagnostics.log(crate::diagnostics::Level::Info, "smoke", &format!("state requested by {}", webview.label()));
    }
    Ok(engine.state_snapshot())
}

#[tauri::command(async)]
pub async fn settings_save(webview: Webview, engine: State<'_, Arc<Engine>>, input: Map<String, Value>) -> Result<Value> {
    trusted(&webview)?;
    engine.inner().clone().save_settings(input).await
}

#[tauri::command(async)]
pub async fn path_pick(webview: Webview, kind: String) -> Result<String> {
    trusted(&webview)?;
    if !["sdk", "cfg", "console", "updates"].contains(&kind.as_str()) {
        return Err("Unknown path type".into());
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut dialog = webview.app_handle().dialog().file();
    if let Some(main) = webview.app_handle().get_webview_window("main") {
        dialog = dialog.set_parent(&main);
    }
    let done = move |path: Option<tauri_plugin_dialog::FilePath>| {
        let _ = sender.send(path.map(|p| p.to_string()).unwrap_or_default());
    };
    if kind == "console" {
        dialog.pick_file(done)
    } else {
        dialog.pick_folder(done)
    }
    receiver.await.map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn gsi_install(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<Value> {
    trusted(&webview)?;
    Ok(json!({ "ok": true, "file": engine.install_gsi_config()? }))
}

#[tauri::command(async)]
pub fn csstats_login(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<bool> {
    trusted(&webview)?;
    engine.csstats_login()?;
    Ok(true)
}

#[tauri::command(async)]
pub fn csrep_verify(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<bool> {
    trusted(&webview)?;
    engine.csrep_verify()?;
    Ok(true)
}

#[tauri::command(async)]
pub fn csrep_login(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<bool> {
    trusted(&webview)?;
    engine.csrep_login()?;
    Ok(true)
}

#[tauri::command(async)]
pub fn profile_open(webview: Webview, engine: State<'_, Arc<Engine>>, provider: String, steam_id: String) -> Result<bool> {
    trusted(&webview)?;
    let id = numeric_steam_id(&steam_id);
    if id.is_empty() {
        return Err("Invalid SteamID64.".into());
    }
    // From the overlay (CS2's Esc menu), pages open in an app window over the game; a browser window would
    // open unseen behind CS2.
    let over_game = webview.label() == crate::overlay::LABEL;
    let open_page = |url: String| -> Result<()> {
        if over_game {
            crate::providers::scraper::open_profile_window(&engine.app, engine.providers.profile_dir.clone(), &url)
        } else {
            engine.app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
        }
    };
    match provider.as_str() {
        "csstats" => engine.providers.csstats.open_profile(&id, over_game)?,
        // In-app window: shares the profile used for lookups, so a verification there unblocks them.
        "csrep" => engine.providers.csrep.open_profile(&id, over_game)?,
        "leetify" => open_page(leetify::profile_url(&id))?,
        // FACEIT profiles are addressed by nickname, which only FACEIT's API (your key) provides.
        "faceit" => open_page(
            engine.faceit_url(&id).ok_or("A FACEIT profile link needs the FACEIT API key (Settings → Data sources), or the player has no FACEIT account.")?,
        )?,
        "steam" => open_page(format!("https://steamcommunity.com/profiles/{id}"))?,
        _ => return Err(format!("Unknown provider {provider}.")),
    }
    Ok(true)
}

#[tauri::command(async)]
pub fn link_open(webview: Webview, engine: State<'_, Arc<Engine>>, target: String) -> Result<bool> {
    trusted(&webview)?;
    // A fixed list, so a page can never ask for an arbitrary address to be opened.
    let url = match target.as_str() {
        "leetify" => "https://leetify.com/",
        "author" => "https://github.com/KKNZ89",
        "repo" => "https://github.com/KKNZ89/cs2-player-intelligence-overlay",
        "issues" => "https://github.com/KKNZ89/cs2-player-intelligence-overlay/issues",
        "releases" => "https://github.com/KKNZ89/cs2-player-intelligence-overlay/releases",
        _ => return Err(format!("Unknown link {target}.")),
    };
    engine.app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command(async)]
pub fn player_add(webview: Webview, engine: State<'_, Arc<Engine>>, steam_id: String) -> Result<bool> {
    trusted(&webview)?;
    engine.inner().with_match(|state, g| state.add_player(&steam_id, g))?;
    Ok(true)
}

#[tauri::command(async)]
pub fn players_select(webview: Webview, engine: State<'_, Arc<Engine>>, input: String) -> Result<Value> {
    trusted(&webview)?;
    let count = engine.inner().with_match(|state, g| state.select_players(&input, g))?;
    Ok(json!({ "count": count }))
}

#[tauri::command(async)]
pub fn players_automatic(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<bool> {
    trusted(&webview)?;
    engine.inner().with_match(|state, g| {
        state.use_automatic(g);
        Ok(())
    })?;
    Ok(true)
}

#[tauri::command(async)]
pub fn player_remove(webview: Webview, engine: State<'_, Arc<Engine>>, steam_id: String) -> Result<bool> {
    trusted(&webview)?;
    engine.inner().with_match(|state, g| state.remove_player(&steam_id, g))?;
    Ok(true)
}

#[tauri::command(async)]
pub fn player_side(webview: Webview, engine: State<'_, Arc<Engine>>, steam_id: String, side: String) -> Result<bool> {
    trusted(&webview)?;
    engine.set_player_side(&steam_id, &side)?;
    Ok(true)
}

#[tauri::command(async)]
pub async fn players_refresh(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<bool> {
    trusted(&webview)?;
    engine.inner().refresh_all(Force::All).await;
    Ok(true)
}

#[tauri::command(async)]
pub async fn player_refresh(webview: Webview, engine: State<'_, Arc<Engine>>, steam_id: String) -> Result<bool> {
    trusted(&webview)?;
    engine.inner().refresh_player(&steam_id).await?;
    Ok(true)
}

#[tauri::command(async)]
pub fn overlay_toggle(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<bool> {
    trusted(&webview)?;
    Ok(engine.toggle_pinned())
}

#[tauri::command(async)]
pub fn overlay_interaction_end(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<bool> {
    if webview.label() != crate::overlay::LABEL {
        return Err("Only the overlay can end mouse navigation.".into());
    }
    Ok(engine.end_mouse_navigation())
}

#[tauri::command(async)]
pub fn overlay_fit(webview: Webview, engine: State<'_, Arc<Engine>>, height: f64, side_width: Option<f64>) -> Result<bool> {
    // Only the overlay sizes itself.
    if webview.label() != crate::overlay::LABEL {
        return Ok(false);
    }
    Ok(engine.fit_overlay(height, side_width))
}

#[tauri::command(async)]
pub async fn update_check(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<Value> {
    trusted(&webview)?;
    Ok(engine.updates.check().await)
}

#[tauri::command(async)]
pub fn update_install(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<bool> {
    trusted(&webview)?;
    engine.kill_steam();
    engine.updates.install()?;
    Ok(true)
}

#[tauri::command(async)]
pub fn game_launch(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<String> {
    trusted(&webview)?;
    Ok(engine.start_game())
}

#[tauri::command(async)]
pub fn diagnostics_recent(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<Vec<String>> {
    trusted(&webview)?;
    Ok(engine.diagnostics.recent(80))
}

#[tauri::command(async)]
pub fn diagnostics_report(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<String> {
    trusted(&webview)?;
    Ok(engine.diagnostics.last_match_report())
}

#[tauri::command(async)]
pub fn diagnostics_open(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<bool> {
    trusted(&webview)?;
    let file = &engine.diagnostics.file;
    if !file.exists() {
        engine.diagnostics.log(crate::diagnostics::Level::Info, "diagnostics", "log opened");
    }
    engine.app.opener().reveal_item_in_dir(file).map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command(async)]
pub fn player_history(webview: Webview, engine: State<'_, Arc<Engine>>, steam_id: String) -> Result<Value> {
    trusted(&webview)?;
    Ok(engine.player_history(&steam_id))
}

#[tauri::command(async)]
pub fn note_add(webview: Webview, engine: State<'_, Arc<Engine>>, steam_id: String, text: String, match_id: Option<i64>) -> Result<Value> {
    trusted(&webview)?;
    let steam_id = numeric_steam_id(&steam_id);
    if steam_id.is_empty() {
        return Err("Invalid SteamID64.".into());
    }
    engine.add_note(&steam_id, &text, match_id)
}

#[tauri::command(async)]
pub fn note_delete(webview: Webview, engine: State<'_, Arc<Engine>>, id: i64) -> Result<bool> {
    trusted(&webview)?;
    engine.delete_note(id)?;
    Ok(true)
}

#[tauri::command(async)]
pub fn history_matches(webview: Webview, engine: State<'_, Arc<Engine>>, limit: usize, offset: usize) -> Result<Value> {
    trusted(&webview)?;
    Ok(engine.history_matches(limit, offset))
}

#[tauri::command(async)]
pub fn history_performance(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<Value> {
    trusted(&webview)?;
    Ok(engine.history_performance())
}

#[tauri::command(async)]
pub fn history_match(webview: Webview, engine: State<'_, Arc<Engine>>, id: i64) -> Result<Value> {
    trusted(&webview)?;
    engine.history_match(id).ok_or_else(|| "That match is no longer in the history.".to_string())
}

#[tauri::command(async)]
pub fn history_stats(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<Value> {
    trusted(&webview)?;
    Ok(engine.history_stats())
}

/// Saves the history database as JSON where you choose; returns the file, or "" when cancelled.
#[tauri::command(async)]
pub async fn history_export(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<String> {
    trusted(&webview)?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut dialog = webview.app_handle().dialog().file().add_filter("JSON", &["json"]).set_file_name("cs2-player-intel-history.json");
    if let Some(main) = webview.app_handle().get_webview_window("main") {
        dialog = dialog.set_parent(&main);
    }
    dialog.save_file(move |path| {
        let _ = sender.send(path);
    });
    let Some(path) = receiver.await.map_err(|e| e.to_string())? else { return Ok(String::new()) };
    let path = path.into_path().map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(&engine.export_history()).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command(async)]
pub fn history_clear(webview: Webview, engine: State<'_, Arc<Engine>>, notes: bool) -> Result<bool> {
    trusted(&webview)?;
    engine.clear_history(notes)?;
    Ok(true)
}

/// Merges a history export chosen by you into this app's history; returns what was added.
#[tauri::command(async)]
pub async fn history_import(webview: Webview, engine: State<'_, Arc<Engine>>) -> Result<Value> {
    trusted(&webview)?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut dialog = webview.app_handle().dialog().file().add_filter("JSON", &["json"]);
    if let Some(main) = webview.app_handle().get_webview_window("main") {
        dialog = dialog.set_parent(&main);
    }
    dialog.pick_file(move |path| {
        let _ = sender.send(path);
    });
    let Some(path) = receiver.await.map_err(|e| e.to_string())? else { return Ok(Value::Null) };
    let path = path.into_path().map_err(|e| e.to_string())?;
    // Exports are a few MB at most; a much larger file is not one.
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 100 * 1024 * 1024 {
        return Err("That file is too large to be a history export.".into());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let data: Value = serde_json::from_str(&text).map_err(|e| format!("Not a valid export: {e}"))?;
    engine.import_history(&data)
}
