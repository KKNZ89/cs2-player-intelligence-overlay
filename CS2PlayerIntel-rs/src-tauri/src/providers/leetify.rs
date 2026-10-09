//! Leetify Public CS API (https://api-public-docs.cs-prod.leetify.com). Keyless requests are allowed with
//! stricter rate limits. Leetify's developer guidelines apply: show "Data Provided by Leetify", do not
//! store their data (results live in memory for the current match only) and do not rename, rescale or
//! recalculate their metrics.

use super::{number, request_failure, Failure};
use crate::model::{is_steam_id, ProviderResult};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const API: &str = "https://api-public.cs-prod.leetify.com";
/// Leetify sometimes sends headers at once and then stalls the body; a stalled request is retried.
const TIMEOUT: Duration = Duration::from_secs(20);
/// Keyless use hit a rate limit after about ten quick requests in testing, so requests are spaced.
const GAP: Duration = Duration::from_millis(1500);
const PAUSE: Duration = Duration::from_secs(30);
const ATTEMPTS: u32 = 3;

pub fn profile_url(steam_id: &str) -> String {
    format!("https://leetify.com/app/profile/{steam_id}")
}

type Fetched = Result<Value, Failure>;

/// Requests are serialized, so a rate-limit wait holds back every later lookup too.
pub struct LeetifyClient {
    http: reqwest::Client,
    last_request: tokio::sync::Mutex<Option<Instant>>,
}

impl LeetifyClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http, last_request: tokio::sync::Mutex::new(None) }
    }

    pub async fn get(&self, path: &str) -> Fetched {
        let mut last = self.last_request.lock().await;
        for attempt in 1.. {
            if let Some(previous) = *last {
                tokio::time::sleep(GAP.saturating_sub(previous.elapsed())).await;
            }
            *last = Some(Instant::now());
            let response = match self.http.get(format!("{API}{path}")).header("Accept", "application/json").timeout(TIMEOUT).send().await {
                Ok(response) => response,
                Err(_) if attempt < ATTEMPTS => continue,
                Err(error) => return Err(request_failure(&error)),
            };
            let status = response.status().as_u16();
            if status == 429 {
                if attempt >= ATTEMPTS {
                    return Err(Failure::with("rate-limited", "Leetify kept rate limiting; refresh later."));
                }
                // A 429 makes the whole queue wait (Retry-After, else 30 seconds) before retrying.
                let retry_after = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).filter(|s| *s > 0);
                tokio::time::sleep(retry_after.map(|s| Duration::from_secs(s.min(120))).unwrap_or(PAUSE)).await;
                continue;
            }
            if status == 404 {
                return Err(Failure::new("not-found"));
            }
            if !response.status().is_success() {
                return Err(Failure::new(&format!("http-{status}")));
            }
            match response.bytes().await {
                Ok(body) => return serde_json::from_slice::<Value>(&body).map_err(|e| Failure::with("error", format!("Leetify sent unreadable data: {e}"))),
                Err(_) if attempt < ATTEMPTS => continue,
                Err(error) => return Err(request_failure(&error)),
            }
        }
        unreachable!("the retry loop always returns")
    }
}

pub fn map_profile(data: &Value, steam_id: &str) -> Map<String, Value> {
    // Recent matches as Leetify reports them (newest first); only counted, never re-scored.
    let recent: Vec<Value> = data["recent_matches"]
        .as_array()
        .map(|list| {
            list.iter()
                .take(100)
                .map(|m| {
                    let outcome = m["outcome"].as_str().filter(|o| matches!(*o, "win" | "loss" | "tie")).unwrap_or("unknown");
                    json!({
                        "finishedAt": m["finished_at"].as_str(),
                        "map": m["map_name"].as_str().unwrap_or_default(),
                        "outcome": outcome,
                        "score": m["score"].as_array().map(|s| s.iter().take(2).cloned().collect::<Vec<_>>()),
                        "leetifyRating": number(&m["leetify_rating"]),
                        "dataSource": m["data_source"].as_str().unwrap_or_default(),
                        // Rank at the time: Premier rating (type 11), Competitive skill group (12), or the
                        // FACEIT level for FACEIT matches.
                        "rank": number(&m["rank"]),
                        "rankType": number(&m["rank_type"]),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // Who they queue with most; used to group party members onto one team.
    let teammates: Vec<Value> = data["recent_teammates"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|mate| {
                    let id = match &mate["steam64_id"] {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        _ => return None,
                    };
                    is_steam_id(&id).then(|| json!({ "steamId": id, "count": mate["recent_matches_count"].as_f64().unwrap_or(0.0) }))
                })
                .take(20)
                .collect()
        })
        .unwrap_or_default();
    let fields = json!({
        "name": data["name"].as_str().unwrap_or_default(),
        "privacy": data["privacy_mode"].as_str().unwrap_or_default(),
        "premier": number(&data["ranks"]["premier"]),
        "leetifyRating": number(&data["ranks"]["leetify"]),
        "faceitLevel": number(&data["ranks"]["faceit"]),
        "faceitElo": number(&data["ranks"]["faceit_elo"]),
        "aim": number(&data["rating"]["aim"]),
        "positioning": number(&data["rating"]["positioning"]),
        "utility": number(&data["rating"]["utility"]),
        "timeToDamageMs": number(&data["stats"]["reaction_time_ms"]),
        "headAccuracy": number(&data["stats"]["accuracy_head"]),
        "preaim": number(&data["stats"]["preaim"]),
        "sprayAccuracy": number(&data["stats"]["spray_accuracy"]),
        "clutch": number(&data["rating"]["clutch"]),
        "opening": number(&data["rating"]["opening"]),
        "ctLeetify": number(&data["rating"]["ct_leetify"]),
        "tLeetify": number(&data["rating"]["t_leetify"]),
        "ctOpeningDuelSuccess": number(&data["stats"]["ct_opening_duel_success_percentage"]),
        "tOpeningDuelSuccess": number(&data["stats"]["t_opening_duel_success_percentage"]),
        "ctOpeningAggressionSuccess": number(&data["stats"]["ct_opening_aggression_success_rate"]),
        "tOpeningAggressionSuccess": number(&data["stats"]["t_opening_aggression_success_rate"]),
        "tradeKillsSuccess": number(&data["stats"]["trade_kills_success_percentage"]),
        "tradedDeathsSuccess": number(&data["stats"]["traded_deaths_success_percentage"]),
        "flashbangLeadingToKill": number(&data["stats"]["flashbang_leading_to_kill"]),
        "utilityOnDeath": number(&data["stats"]["utility_on_death_avg"]),
        "accuracyEnemySpotted": number(&data["stats"]["accuracy_enemy_spotted"]),
        "counterStrafing": number(&data["stats"]["counter_strafing_good_shots_ratio"]),
        "wingman": number(&data["ranks"]["wingman"]),
        // Competitive skill group per map (0 = unranked on that map).
        "mapRanks": data["ranks"]["competitive"].as_array().map(|list| {
            list.iter().filter_map(|r| Some(json!({ "map": r["map_name"].as_str()?, "rank": number(&r["rank"]) }))).collect::<Vec<_>>()
        }).unwrap_or_default(),
        "firstMatchAt": data["first_match_date"].as_str(),
        "winrate": number(&data["winrate"]),
        "totalMatches": number(&data["total_matches"]),
        "bans": data["bans"].as_array().map(|b| Value::from(b.len())).unwrap_or(Value::Null),
        "recent": recent,
        "recentTeammates": teammates,
        "profileUrl": profile_url(steam_id),
    });
    match fields {
        Value::Object(map) => map,
        _ => unreachable!("json! object literal"),
    }
}

/// Shared matches in both players' recent Leetify histories (up to 100 each), split into games on the
/// same team and against each other, with your result in each.
pub fn summarize_shared(own: &Value, theirs: &Value, self_id: &str, steam_id: &str) -> Map<String, Value> {
    let empty = || json!({ "played": 0, "won": 0, "lost": 0, "tied": 0 });
    let (mut together, mut against) = (empty(), empty());
    let mut last_played: Option<String> = None;
    let their_matches: HashMap<String, &Value> =
        theirs.as_array().map(|list| list.iter().filter_map(|m| Some((m["id"].as_str()?.to_string(), m))).collect()).unwrap_or_default();
    let find = |entry: &Value, id: &str| -> Option<Value> {
        let stats = entry["stats"].as_array()?;
        stats
            .iter()
            .find(|s| match &s["steam64_id"] {
                Value::String(value) => value == id,
                Value::Number(value) => value.to_string() == id,
                _ => false,
            })
            .cloned()
    };
    for entry in own.as_array().into_iter().flatten() {
        let Some(other) = entry["id"].as_str().and_then(|id| their_matches.get(id)) else { continue };
        let (Some(me), Some(them)) = (find(entry, self_id), find(other, steam_id)) else { continue };
        if me["initial_team_number"].is_null() || them["initial_team_number"].is_null() {
            continue;
        }
        let bucket = if me["initial_team_number"] == them["initial_team_number"] { &mut together } else { &mut against };
        let (won, lost) = (me["rounds_won"].as_f64().unwrap_or(0.0), me["rounds_lost"].as_f64().unwrap_or(0.0));
        let key = if won > lost {
            "won"
        } else if won < lost {
            "lost"
        } else {
            "tied"
        };
        for field in ["played", key] {
            bucket[field] = Value::from(bucket[field].as_u64().unwrap_or(0) + 1);
        }
        if let Some(finished) = entry["finished_at"].as_str() {
            if last_played.as_deref().is_none_or(|last| finished > last) {
                last_played = Some(finished.to_string());
            }
        }
    }
    let mut map = Map::new();
    map.insert("together".into(), together);
    map.insert("against".into(), against);
    map.insert("lastPlayedAt".into(), last_played.map(Value::from).unwrap_or(Value::Null));
    map
}

type OwnHistory = Arc<tokio::sync::OnceCell<Fetched>>;

pub struct LeetifyProvider {
    client: LeetifyClient,
    /// Your own match history, fetched once per match and shared by every lookup.
    own: Mutex<HashMap<String, OwnHistory>>,
}

impl LeetifyProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self { client: LeetifyClient::new(http), own: Mutex::new(HashMap::new()) }
    }

    pub fn reset(&self) {
        self.own.lock().unwrap().clear();
    }

    pub async fn profile(&self, steam_id: &str) -> ProviderResult {
        match self.client.get(&format!("/v3/profile?steam64_id={steam_id}")).await {
            Ok(data) => ProviderResult::ok(map_profile(&data, steam_id)),
            Err(failure) => failure.into_result(Some(profile_url(steam_id))),
        }
    }

    pub async fn shared_history(&self, steam_id: &str, self_id: &str) -> ProviderResult {
        if self_id.is_empty() {
            return ProviderResult::status("waiting-for-self");
        }
        if steam_id == self_id {
            return ProviderResult::status("self");
        }
        let cell = self.own.lock().unwrap().entry(self_id.to_string()).or_default().clone();
        let path = format!("/v3/profile/matches?steam64_id={self_id}");
        let own = cell.get_or_init(|| self.client.get(&path)).await.clone();
        let own = match own {
            Ok(own) => own,
            Err(failure) if failure.status == "not-found" => return ProviderResult::failed("no-own-history", "Your account has no Leetify match history."),
            Err(failure) => {
                // Transient failures are retried by the next lookup.
                self.own.lock().unwrap().remove(self_id);
                return failure.into_result(None);
            }
        };
        match self.client.get(&format!("/v3/profile/matches?steam64_id={steam_id}")).await {
            Ok(theirs) => ProviderResult::ok(summarize_shared(&own, &theirs, self_id, steam_id)),
            Err(failure) => failure.into_result(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_profiles_without_inventing_values() {
        let data = json!({
            "name": "Player", "privacy_mode": "public", "winrate": 0.52, "total_matches": 812,
            "ranks": { "premier": 15432, "leetify": 1.2, "faceit": null, "competitive": [{ "map_name": "de_ancient", "rank": 14 }] },
            "rating": { "aim": 71.5, "opening": 0.013 }, "stats": { "reaction_time_ms": 512.3, "accuracy_head": 21.1, "ct_opening_duel_success_percentage": 57.9 },
            "bans": [], "recent_matches": [{ "finished_at": "2026-10-01T10:00:00Z", "map_name": "de_ancient", "outcome": "win", "score": [13, 9, 1], "leetify_rating": 0.05, "rank": 15432, "rank_type": 11 }],
            "recent_teammates": [{ "steam64_id": "76561198000000001", "recent_matches_count": 7 }, { "steam64_id": "bogus", "recent_matches_count": 9 }]
        });
        let profile = map_profile(&data, "76561198000000000");
        assert_eq!(profile["premier"], 15432.0);
        assert_eq!(profile["faceitLevel"], Value::Null, "missing values stay null");
        assert_eq!(profile["timeToDamageMs"], 512.3);
        assert_eq!(profile["bans"], 0);
        assert_eq!(profile["recent"][0]["score"], json!([13, 9]));
        assert_eq!(profile["recentTeammates"], json!([{ "steamId": "76561198000000001", "count": 7.0 }]));
        assert_eq!(
            (profile["recent"][0]["rank"].as_f64(), profile["recent"][0]["rankType"].as_f64()),
            (Some(15432.0), Some(11.0)),
            "rank at the time of each match"
        );
        assert_eq!(profile["mapRanks"], json!([{ "map": "de_ancient", "rank": 14.0 }]));
        assert_eq!((profile["opening"].as_f64(), profile["ctOpeningDuelSuccess"].as_f64()), (Some(0.013), Some(57.9)));
        assert_eq!(profile["clutch"], Value::Null);
    }

    #[test]
    fn summarizes_shared_matches() {
        let me = "76561198000000000";
        let them = "76561198000000001";
        let stats = |id: &str, team: u8, won: u8, lost: u8| json!({ "steam64_id": id, "initial_team_number": team, "rounds_won": won, "rounds_lost": lost });
        let own = json!([
            { "id": "a", "finished_at": "2026-10-01", "stats": [stats(me, 2, 13, 5)] },
            { "id": "b", "finished_at": "2026-10-03", "stats": [stats(me, 3, 5, 13)] },
            { "id": "c", "finished_at": "2026-10-04", "stats": [stats(me, 3, 12, 12)] },
            { "id": "solo", "finished_at": "2026-10-05", "stats": [stats(me, 3, 13, 0)] }
        ]);
        let theirs = json!([
            { "id": "a", "stats": [stats(them, 2, 13, 5)] },
            { "id": "b", "stats": [stats(them, 2, 13, 5)] },
            { "id": "c", "stats": [stats(them, 3, 12, 12)] }
        ]);
        let summary = summarize_shared(&own, &theirs, me, them);
        assert_eq!(summary["together"], json!({ "played": 2, "won": 1, "lost": 0, "tied": 1 }));
        assert_eq!(summary["against"], json!({ "played": 1, "won": 0, "lost": 1, "tied": 0 }));
        assert_eq!(summary["lastPlayedAt"], "2026-10-04");
    }

    #[test]
    fn shared_history_requires_both_player_identities() {
        let own = json!([{ "id": "a", "stats": [{ "steam64_id": "76561198000000000", "initial_team_number": 2, "rounds_won": 13, "rounds_lost": 5 }] }]);
        let wrong = json!([{ "id": "a", "stats": [{ "steam64_id": "76561198000000002", "initial_team_number": 2 }] }]);
        let result = summarize_shared(&own, &wrong, "76561198000000000", "76561198000000001");
        assert_eq!(result["together"]["played"], 0);
        assert_eq!(result["against"]["played"], 0);
        let numeric = json!([{ "id": "a", "stats": [{ "steam64_id": 76561198000000001u64, "initial_team_number": 3 }] }]);
        assert_eq!(summarize_shared(&own, &numeric, "76561198000000000", "76561198000000001")["against"]["played"], 1);
    }
}
