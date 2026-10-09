//! CS2 Game State Integration: CS2 posts JSON to a loopback HTTP server. Posts must carry this install's
//! private token, compared in constant time.

use serde::Serialize;
use serde_json::Value;
use subtle::ConstantTimeEq;

pub const PORT: u16 = 31982;
pub const TOKEN_REJECTED: &str = "CS2 sent an unknown GSI token. Click Install GSI config, then restart CS2.";
const CONNECTED_FOR_MS: i64 = 15_000;

fn same_token(received: Option<&str>, expected: &str) -> bool {
    received.is_some_and(|value| value.len() == expected.len() && bool::from(value.as_bytes().ct_eq(expected.as_bytes())))
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Observed {
    pub steam_id: String,
    pub name: String,
    pub team: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GsiSummary {
    pub connected: bool,
    pub error: String,
    pub notice: String,
    pub map: String,
    pub mode: String,
    pub phase: String,
    pub round: Option<i64>,
    pub round_phase: String,
    /// CS2 reports "menu" while its pause menu (Esc) or main menu is open, "playing" otherwise.
    pub activity: String,
    #[serde(rename = "scoreCT")]
    pub score_ct: Option<i64>,
    #[serde(rename = "scoreT")]
    pub score_t: Option<i64>,
    pub self_steam_id: String,
    pub self_name: String,
    pub self_team: String,
    /// The player you are spectating (only while you are not observing yourself).
    pub observed: Option<Observed>,
    pub match_stats_status: String,
    pub kills: Option<i64>,
    pub assists: Option<i64>,
    pub deaths: Option<i64>,
    pub kd: Option<f64>,
    pub mvps: Option<i64>,
    pub score: Option<i64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Receipt {
    Accepted,
    /// Wrong token; `first` is true for the first rejection in a row.
    Rejected {
        first: bool,
    },
    BadJson,
}

struct OwnStats {
    map: String,
    mode: String,
    stats: Value,
}

pub struct GsiState {
    token: String,
    latest: Value,
    last_seen: i64,
    rejected_at: i64,
    last_own: Option<OwnStats>,
    pub listen_error: String,
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

impl GsiState {
    pub fn new(token: String) -> Self {
        Self { token, latest: Value::Null, last_seen: 0, rejected_at: 0, last_own: None, listen_error: String::new() }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn receive(&mut self, body: &[u8], now: i64) -> Receipt {
        let Ok(payload) = serde_json::from_slice::<Value>(if body.is_empty() { b"{}" } else { body }) else { return Receipt::BadJson };
        let received = payload.pointer("/auth/token").and_then(Value::as_str);
        if !same_token(received, &self.token) {
            let first = self.rejected_at == 0;
            self.rejected_at = now;
            return Receipt::Rejected { first };
        }
        self.rejected_at = 0;
        self.remember_own_stats(&payload);
        self.latest = payload;
        self.last_seen = now;
        Receipt::Accepted
    }

    /// While you are dead GSI reports the player you spectate, so your last own stats on this map are kept.
    fn remember_own_stats(&mut self, payload: &Value) {
        let self_id = text(&payload["provider"]["steamid"]);
        let player = &payload["player"];
        let map = text(&payload["map"]["name"]);
        if self_id.is_empty() || text(&player["steamid"]) != self_id || map.is_empty() || !player["match_stats"].is_object() {
            return;
        }
        self.last_own = Some(OwnStats { map, mode: text(&payload["map"]["mode"]), stats: player["match_stats"].clone() });
    }

    pub fn connected(&self, now: i64) -> bool {
        self.last_seen > 0 && now - self.last_seen < CONNECTED_FOR_MS
    }

    pub fn summary(&self, now: i64) -> GsiSummary {
        let p = &self.latest;
        let map = &p["map"];
        let player = &p["player"];
        let player_id = text(&player["steamid"]);
        let provider_id = text(&p["provider"]["steamid"]);
        let self_steam_id = if provider_id.is_empty() { player_id.clone() } else { provider_id };
        let observing_self = player_id.is_empty() || player_id == self_steam_id;
        let connected = self.connected(now);
        let map_name = text(&map["name"]);
        let mode = text(&map["mode"]);
        let phase = text(&map["phase"]);
        let in_match = !map_name.is_empty() && matches!(phase.as_str(), "warmup" | "live" | "intermission" | "gameover");
        let remembered = self.last_own.as_ref().filter(|own| own.map == map_name && own.mode == mode).map(|own| &own.stats);
        let empty = Value::Null;
        let own_stats = if !connected || !in_match {
            &empty
        } else if observing_self {
            &player["match_stats"]
        } else {
            remembered.unwrap_or(&empty)
        };
        let number = |key: &str| own_stats[key].as_f64().filter(|n| n.is_finite() && *n >= 0.0).map(|n| n as i64);
        let (kills, deaths) = (number("kills"), number("deaths"));
        let status = if !connected {
            "disconnected"
        } else if !in_match {
            "not-in-match"
        } else if !observing_self {
            if remembered.is_some() && (kills.is_some() || deaths.is_some()) {
                "last-known"
            } else {
                "observing-other-player"
            }
        } else if kills.is_none() && deaths.is_none() {
            "waiting"
        } else {
            "available"
        };
        let rejected = !connected && self.rejected_at > 0 && now - self.rejected_at < CONNECTED_FOR_MS;
        let error = if !self.listen_error.is_empty() {
            self.listen_error.clone()
        } else if rejected {
            TOKEN_REJECTED.into()
        } else {
            String::new()
        };
        let observed = (connected && !observing_self && !player_id.is_empty()).then(|| Observed {
            steam_id: player_id.clone(),
            name: text(&player["name"]),
            team: text(&player["team"]),
        });
        GsiSummary {
            connected,
            error,
            notice: String::new(),
            map: map_name,
            mode,
            phase,
            round: map["round"].as_i64().map(|r| r + 1),
            round_phase: text(&p["round"]["phase"]),
            activity: text(&player["activity"]),
            score_ct: map["team_ct"]["score"].as_i64(),
            score_t: map["team_t"]["score"].as_i64(),
            self_steam_id,
            self_name: if observing_self { text(&player["name"]) } else { String::new() },
            self_team: if observing_self { text(&player["team"]) } else { String::new() },
            observed,
            match_stats_status: status.into(),
            kills,
            assists: number("assists"),
            deaths,
            kd: match (kills, deaths) {
                (Some(k), Some(d)) if d > 0 => Some(k as f64 / d as f64),
                _ => None,
            },
            mvps: number("mvps"),
            score: number("score"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const ME: &str = "76561198000000000";

    fn post(state: &mut GsiState, value: Value, now: i64) -> Receipt {
        state.receive(value.to_string().as_bytes(), now)
    }

    fn payload(token: &str, player_id: &str, phase: &str) -> Value {
        json!({
            "auth": { "token": token },
            "provider": { "steamid": ME },
            "map": { "name": "de_mirage", "mode": "competitive", "phase": phase, "round": 4, "team_ct": { "score": 3 }, "team_t": { "score": 1 } },
            "round": { "phase": "live" },
            "player": { "steamid": player_id, "name": "Someone", "team": "CT", "activity": "playing", "match_stats": { "kills": 6, "assists": 1, "deaths": 2, "mvps": 1, "score": 14 } }
        })
    }

    #[test]
    fn tokens_are_checked() {
        let mut state = GsiState::new(TOKEN.into());
        assert_eq!(post(&mut state, payload("wrong", ME, "live"), 1), Receipt::Rejected { first: true });
        assert_eq!(post(&mut state, payload("wrong", ME, "live"), 2), Receipt::Rejected { first: false });
        assert_eq!(state.summary(3).error, TOKEN_REJECTED);
        assert_eq!(post(&mut state, payload(TOKEN, ME, "live"), 6), Receipt::Accepted);
        assert!(state.summary(7).error.is_empty());
        assert_eq!(post(&mut state, payload("wrong", ME, "live"), 8), Receipt::Rejected { first: true });
        assert_eq!(state.receive(b"{oops", 8), Receipt::BadJson);
    }

    #[test]
    fn summary_reports_own_and_last_known_stats() {
        let mut state = GsiState::new(TOKEN.into());
        post(&mut state, payload(TOKEN, ME, "live"), 1000);
        let own = state.summary(1000);
        assert!(own.connected);
        assert_eq!((own.round, own.score_ct, own.score_t), (Some(5), Some(3), Some(1)));
        assert_eq!((own.kills, own.deaths, own.kd), (Some(6), Some(2), Some(3.0)));
        assert_eq!((own.match_stats_status.as_str(), own.self_team.as_str()), ("available", "CT"));
        let json = serde_json::to_value(&own).unwrap();
        assert_eq!(json["scoreCT"], 3);
        assert_eq!(json["selfSteamId"], ME);

        let spectating = payload(TOKEN, "76561198000000001", "live");
        post(&mut state, spectating, 2000);
        let other = state.summary(2000);
        assert_eq!(other.match_stats_status, "last-known");
        assert_eq!(other.kills, Some(6));
        assert_eq!(other.observed.as_ref().unwrap().steam_id, "76561198000000001");
        assert_eq!(other.self_team, "");
        assert!(!state.summary(2000 + CONNECTED_FOR_MS).connected, "silence disconnects");
        assert_eq!(state.summary(2000 + CONNECTED_FOR_MS).match_stats_status, "disconnected");
    }
}
