//! Shared data types. Field names serialize in camelCase because the UI reads them directly.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// A 17-digit SteamID64 in the individual-account range.
pub fn is_steam_id(value: &str) -> bool {
    value.len() == 17 && value.starts_with("7656119") && value.bytes().all(|b| b.is_ascii_digit())
}

/// One provider's answer for one player: a status plus whatever fields the provider published.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderResult {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    #[serde(flatten)]
    pub data: Map<String, Value>,
}

impl ProviderResult {
    pub fn status(status: &str) -> Self {
        Self { status: status.into(), ..Self::default() }
    }

    pub fn failed(status: &str, error: impl Into<String>) -> Self {
        Self { status: status.into(), error: Some(error.into()), ..Self::default() }
    }

    pub fn ok(data: Map<String, Value>) -> Self {
        Self { status: "ok".into(), data, ..Self::default() }
    }

    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }

    pub fn number(&self, key: &str) -> Option<f64> {
        self.data.get(key).and_then(Value::as_f64).filter(|value| value.is_finite())
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(Value::as_str)
    }
}

/// A Steam friend's party (lobby) as reported by the Steam helper.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Party {
    pub friend: bool,
    pub lobby: String,
    pub server: String,
}

/// Where a player's side came from. `Own`, `Manual`, `Spectated` and `Scoreboard` are certain; `Likely` is inferred.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SideSource {
    #[default]
    #[serde(rename = "")]
    None,
    #[serde(rename = "self")]
    Own,
    Manual,
    Spectated,
    /// Your team or the other on CS2's scoreboard.
    Scoreboard,
    Likely,
}

impl SideSource {
    pub fn is_certain(self) -> bool {
        matches!(self, Self::Own | Self::Manual | Self::Spectated | Self::Scoreboard)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    pub steam_id: String,
    pub name: String,
    pub is_self: bool,
    pub team: String,
    /// "team", "enemy" or "".
    pub side: String,
    pub side_source: SideSource,
    pub side_reason: String,
    pub source: String,
    pub confidence: String,
    pub seen_at: i64,
    pub party: Option<Party>,
    /// CS2 teammate colour ("yellow", "purple", "green", "blue", "orange"), read from the scoreboard.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    /// Provider results by provider name (leetify, csrep, csstats, steam, history).
    #[serde(flatten)]
    pub providers: BTreeMap<String, ProviderResult>,
}

impl Player {
    pub fn provider(&self, name: &str) -> Option<&ProviderResult> {
        self.providers.get(name)
    }

    pub fn premier(&self) -> Option<f64> {
        self.provider("leetify").and_then(|result| result.number("premier")).filter(|value| *value > 0.0)
    }
}

/// A roster entry from discovery (Steam co-play, console, your selection, spectating).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Candidate {
    pub steam_id: String,
    pub name: String,
    pub is_self: bool,
    pub team: String,
    pub source: String,
    pub confidence: String,
    pub seen_at: Option<i64>,
    pub party: Option<Party>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_ids_are_validated() {
        assert!(is_steam_id("76561198012345678"));
        assert!(!is_steam_id("7656119801234567"));
        assert!(!is_steam_id("86561198012345678"));
        assert!(!is_steam_id("7656119801234567x"));
    }

    #[test]
    fn players_serialize_like_the_ui_expects() {
        let mut player = Player { steam_id: "76561198012345678".into(), side_source: SideSource::Own, ..Player::default() };
        let mut data = Map::new();
        data.insert("premier".into(), 15000.into());
        player.providers.insert("leetify".into(), ProviderResult::ok(data));
        let json = serde_json::to_value(&player).unwrap();
        assert_eq!(json["steamId"], "76561198012345678");
        assert_eq!(json["sideSource"], "self");
        assert_eq!(json["leetify"]["status"], "ok");
        assert_eq!(json["leetify"]["premier"], 15000);
        assert_eq!(player.premier(), Some(15000.0));
        assert_eq!(serde_json::to_value(SideSource::None).unwrap(), "");
    }
}
