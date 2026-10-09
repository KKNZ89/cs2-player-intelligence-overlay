//! FACEIT Data API (https://docs.faceit.com/docs/data-api/data), used only with your own API key (free from
//! the FACEIT developer portal). Without a key the app shows the FACEIT level and Elo that Leetify reports.
//! FACEIT's API does not publish peak Elo, so none is shown.

use super::{request_failure, Failure};
use crate::model::ProviderResult;
use serde_json::{json, Map, Value};
use std::time::Duration;

const API: &str = "https://open.faceit.com/data/v4";
const TIMEOUT: Duration = Duration::from_secs(10);
const GAP: Duration = Duration::from_millis(300);

/// FACEIT lifetime stats are strings ("1.12", "48"); missing or unreadable ones stay null.
fn stat(value: &Value) -> Value {
    match value {
        Value::Number(n) => n.as_f64().map(Value::from).unwrap_or(Value::Null),
        Value::String(s) => s.trim().trim_end_matches('%').parse::<f64>().ok().filter(|n| n.is_finite()).map(Value::from).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

pub fn map_player(player: &Value, stats: &Value) -> Map<String, Value> {
    let cs2 = &player["games"]["cs2"];
    let lifetime = &stats["lifetime"];
    let recent: Vec<Value> = lifetime["Recent Results"]
        .as_array()
        .map(|list| list.iter().filter_map(|r| r.as_str()).map(|r| Value::from(if r == "1" { "win" } else { "loss" })).collect())
        .unwrap_or_default();
    let fields = json!({
        "nickname": player["nickname"].as_str().unwrap_or_default(),
        "elo": stat(&cs2["faceit_elo"]),
        "level": stat(&cs2["skill_level"]),
        "region": cs2["region"].as_str().unwrap_or_default(),
        "matches": stat(&lifetime["Matches"]),
        "winRate": stat(&lifetime["Win Rate %"]),
        "kd": stat(&lifetime["Average K/D Ratio"]),
        "hs": stat(&lifetime["Average Headshots %"]),
        "longestWinStreak": stat(&lifetime["Longest Win Streak"]),
        "recentResults": recent,
        "profileUrl": player["faceit_url"].as_str().map(|url| url.replace("{lang}", "en")),
    });
    fields.as_object().cloned().unwrap_or_default()
}

pub struct FaceitProvider {
    http: reqwest::Client,
    queue: tokio::sync::Mutex<()>,
}

impl FaceitProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http, queue: tokio::sync::Mutex::new(()) }
    }

    async fn get(&self, path: &str, key: &str) -> Result<Value, Failure> {
        let _turn = self.queue.lock().await;
        let result = async {
            let response = self.http.get(format!("{API}{path}")).bearer_auth(key).timeout(TIMEOUT).send().await.map_err(|e| request_failure(&e))?;
            match response.status().as_u16() {
                401 | 403 => Err(Failure::with("auth-failed", "FACEIT rejected the API key; check it in Settings.")),
                404 => Err(Failure::new("not-found")),
                429 => Err(Failure::new("rate-limited")),
                status if !response.status().is_success() => Err(Failure::new(&format!("http-{status}"))),
                _ => response.json::<Value>().await.map_err(|e| Failure::with("error", e.to_string())),
            }
        }
        .await;
        tokio::time::sleep(GAP).await;
        result
    }

    pub async fn player(&self, steam_id: &str, api_key: &str) -> ProviderResult {
        if api_key.is_empty() {
            return ProviderResult::status("missing-api-key");
        }
        let player = match self.get(&format!("/players?game=cs2&game_player_id={steam_id}"), api_key).await {
            Ok(player) => player,
            Err(failure) => return failure.into_result(None),
        };
        let Some(player_id) = player["player_id"].as_str() else { return Failure::new("not-found").into_result(None) };
        // Lifetime stats are optional: a player without CS2 matches on FACEIT still has a level.
        let stats = self.get(&format!("/players/{player_id}/stats/cs2"), api_key).await.unwrap_or(Value::Null);
        ProviderResult::ok(map_player(&player, &stats))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_player_and_lifetime_stats() {
        let player = json!({ "player_id": "abc", "nickname": "Kestrel", "faceit_url": "https://www.faceit.com/{lang}/players/Kestrel", "games": { "cs2": { "faceit_elo": 2010, "skill_level": 8, "region": "EU" } } });
        let stats = json!({ "lifetime": { "Matches": "812", "Win Rate %": "53", "Average K/D Ratio": "1.21", "Average Headshots %": "48", "Recent Results": ["1", "0", "1"] } });
        let mapped = map_player(&player, &stats);
        assert_eq!((mapped["elo"].as_f64(), mapped["level"].as_f64(), mapped["matches"].as_f64()), (Some(2010.0), Some(8.0), Some(812.0)));
        assert_eq!((mapped["kd"].as_f64(), mapped["hs"].as_f64()), (Some(1.21), Some(48.0)));
        assert_eq!(mapped["recentResults"], json!(["win", "loss", "win"]));
        assert_eq!(mapped["profileUrl"], "https://www.faceit.com/en/players/Kestrel");
        let bare = map_player(&player, &Value::Null);
        assert_eq!(bare["matches"], Value::Null, "missing stats stay null, never zero");
    }
}
