//! Public Steam profile data. Keyless: the community XML profile (privacy, VAC flag, account age, avatar).
//! CS2 hours and game bans need a Steam Web API key, because Steam only shows game lists to signed-in
//! visitors. Missing values stay null with a reason; they are never reported as zero.

use super::{request_failure, Failure};
use crate::model::ProviderResult;
use chrono::{Datelike, NaiveDate};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
const GAP: Duration = Duration::from_millis(1000);
/// Steam's community pages rate-limit bursts; one wait and retry usually gets through.
const RATE_LIMIT_WAIT: Duration = Duration::from_secs(15);
/// When Steam still refuses after waiting, lookups stop for a while instead of keeping the limit active.
/// The pause doubles each time Steam is still limiting afterwards, up to an hour.
const RATE_LIMIT_PAUSE_MS: i64 = 10 * 60_000;
const RATE_LIMIT_PAUSE_MAX_MS: i64 = 60 * 60_000;
/// Steam's API answers profile and ban questions for up to 100 accounts per request.
const BATCH_MAX: usize = 100;
/// Lobby answers not yet used by a lookup are dropped after this long.
const BATCH_KEEP_MS: i64 = 10 * 60_000;

fn xml_value(xml: &str, tag: &str) -> Option<String> {
    let pattern = Regex::new(&format!(r"<{tag}>(?:<!\[CDATA\[)?([\s\S]*?)(?:\]\]>)?</{tag}>")).ok()?;
    pattern.captures(xml).map(|c| c[1].trim().to_string())
}

/// "March 3, 2012", or "March 3" for accounts created this year.
fn member_since_ms(text: &str) -> Option<i64> {
    let date = NaiveDate::parse_from_str(text, "%B %d, %Y")
        .or_else(|_| NaiveDate::parse_from_str(&format!("{text}, {}", chrono::Utc::now().year()), "%B %d, %Y"))
        .ok()?;
    Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis())
}

/// How long Steam lookups pause after the `previous`-th pause in a row: 10, 20, 40, then 60 minutes.
fn pause_after(previous: u32) -> i64 {
    (RATE_LIMIT_PAUSE_MS << previous.min(3)).min(RATE_LIMIT_PAUSE_MAX_MS)
}

/// A GetPlayerSummaries answer per SteamID, with the same fields as the profile XML (bans come from
/// GetPlayerBans). Accounts that don't exist are missing.
pub fn parse_player_summaries(json: &Value) -> HashMap<String, Map<String, Value>> {
    let players = json["response"]["players"].as_array().map(Vec::as_slice).unwrap_or_default();
    players.iter().filter_map(|player| Some((player["steamid"].as_str()?.to_string(), parse_player_summary(player)?))).collect()
}

/// A GetPlayerBans answer per SteamID.
pub fn parse_player_bans(json: &Value) -> HashMap<String, Value> {
    let players = json["players"].as_array().map(Vec::as_slice).unwrap_or_default();
    players.iter().filter_map(|entry| Some((entry["SteamId"].as_str()?.to_string(), entry.clone()))).collect()
}

fn parse_player_summary(player: &Value) -> Option<Map<String, Value>> {
    let public = player["communityvisibilitystate"].as_i64() == Some(3);
    let fields = json!({
        "name": player["personaname"].as_str().unwrap_or_default(),
        "avatar": player["avatarmedium"].as_str().unwrap_or_default(),
        "privacy": if public { "public" } else { "private" },
        "vacBanned": Value::Null,
        "tradeBanState": Value::Null,
        "limitedAccount": false,
        "createdAt": player["timecreated"].as_i64().map(|seconds| seconds * 1000),
        "memberSince": Value::Null,
    });
    fields.as_object().cloned()
}

pub fn parse_profile_xml(xml: &str) -> Option<Map<String, Value>> {
    if !xml.contains("<profile>") {
        return None;
    }
    let head = xml.split("<mostPlayedGames>").next().unwrap_or_default().split("<groups>").next().unwrap_or_default();
    let member_since = xml_value(head, "memberSince");
    let fields = json!({
        "name": xml_value(head, "steamID").unwrap_or_default(),
        "avatar": xml_value(head, "avatarMedium").unwrap_or_default(),
        "privacy": xml_value(head, "privacyState").unwrap_or_default(),
        "vacBanned": xml_value(head, "vacBanned").map(|v| v == "1"),
        "tradeBanState": xml_value(head, "tradeBanState"),
        "limitedAccount": xml_value(head, "isLimitedAccount").as_deref() == Some("1"),
        "createdAt": member_since.as_deref().and_then(member_since_ms),
        "memberSince": member_since,
    });
    fields.as_object().cloned()
}

/// What one lobby-wide request pair said about an account: its profile (None: no such account) and bans
/// (None: the ban request failed).
#[derive(Clone)]
struct LobbyAnswer {
    at: i64,
    profile: Option<Map<String, Value>>,
    bans: Option<Value>,
}

/// The SteamIDs for one lobby request: this player first, then lobby members without an answer, at most 100.
fn batch_ids(steam_id: &str, lobby: &[String], answered: &HashMap<String, LobbyAnswer>) -> Vec<String> {
    let mut ids = vec![steam_id.to_string()];
    for id in lobby {
        if ids.len() >= BATCH_MAX {
            break;
        }
        if !id.is_empty() && !ids.contains(id) && !answered.contains_key(id) {
            ids.push(id.clone());
        }
    }
    ids
}

/// A rate-limit pause for one of Steam's services.
#[derive(Default)]
struct Pause {
    until: std::sync::atomic::AtomicI64,
    /// Pauses in a row without a successful request in between.
    count: std::sync::atomic::AtomicU32,
}

pub struct SteamProfileProvider {
    http: reqwest::Client,
    queue: tokio::sync::Mutex<()>,
    /// The community pages (profile XML) and the Web API are limited separately, so a limit on the pages
    /// never holds up lookups made with an API key.
    community: Pause,
    api: Pause,
    /// Answers from lobby-wide API requests, each used by that player's lookup. Locked across the request,
    /// so lookups starting together wait for one request instead of each making their own.
    lobby: tokio::sync::Mutex<HashMap<String, LobbyAnswer>>,
}

/// Whether a request goes to the Web API rather than the community pages.
fn is_api(url: &str) -> bool {
    url.starts_with("https://api.steampowered.com/")
}

impl SteamProfileProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http, queue: tokio::sync::Mutex::new(()), community: Pause::default(), api: Pause::default(), lobby: Default::default() }
    }

    /// Requests are serialized and spaced so a full lobby does not trip Steam's rate limit.
    async fn get(&self, url: &str) -> Result<String, Failure> {
        let _turn = self.queue.lock().await;
        use std::sync::atomic::Ordering;
        let pause = if is_api(url) { &self.api } else { &self.community };
        let paused_until = pause.until.load(Ordering::Relaxed);
        if crate::model::now_ms() < paused_until {
            // Answered without a request, and not logged again: the pause was reported when it began.
            let minutes = ((paused_until - crate::model::now_ms()) / 60_000).max(1);
            return Err(Failure::with("paused", format!("Steam is limiting profile requests; trying again in about {minutes} min.")));
        }
        let mut result = self.fetch(url).await;
        if result.as_ref().is_err_and(|failure| failure.status == "rate-limited") {
            tokio::time::sleep(RATE_LIMIT_WAIT).await;
            result = self.fetch(url).await;
            if result.as_ref().is_err_and(|failure| failure.status == "rate-limited") {
                let wait = pause_after(pause.count.fetch_add(1, Ordering::Relaxed));
                pause.until.store(crate::model::now_ms() + wait, Ordering::Relaxed);
                result = Err(Failure::with("rate-limited", format!("Steam is limiting profile requests; Steam lookups pause for {} min.", wait / 60_000)));
            }
        }
        if result.is_ok() {
            pause.count.store(0, Ordering::Relaxed);
        }
        tokio::time::sleep(GAP).await;
        result
    }

    async fn fetch(&self, url: &str) -> Result<String, Failure> {
        let response = self.http.get(url).timeout(TIMEOUT).send().await.map_err(|e| request_failure(&e))?;
        match response.status().as_u16() {
            429 => Err(Failure::new("rate-limited")),
            status if !response.status().is_success() => Err(Failure::new(&format!("http-{status}"))),
            _ => response.text().await.map_err(|e| request_failure(&e)),
        }
    }

    pub async fn player(&self, steam_id: &str, api_key: &str, lobby: &[String]) -> ProviderResult {
        let profile_url = format!("https://steamcommunity.com/profiles/{steam_id}");
        // With a Web API key the profile and bans come from Steam's API, asked about the whole lobby at once;
        // without one, from the public profile XML, one player at a time.
        let (profile, bans) = if api_key.is_empty() {
            (self.profile_xml(&profile_url).await, None)
        } else {
            match self.lobby_answer(steam_id, api_key, lobby).await {
                Ok(answer) => (answer.profile.ok_or_else(|| Failure::new("not-found")), answer.bans),
                Err(failure) => (Err(failure), None),
            }
        };
        let mut data = match profile {
            Ok(data) => data,
            Err(failure) => return failure.into_result(Some(profile_url)),
        };
        for key in ["hoursCs2", "gameBans", "vacBans", "daysSinceLastBan"] {
            data.insert(key.into(), Value::Null);
        }
        data.insert("hoursReason".into(), "Needs a Steam Web API key".into());
        data.insert("profileUrl".into(), profile_url.into());
        if let Some(entry) = bans {
            for (key, field) in
                [("vacBans", "NumberOfVACBans"), ("gameBans", "NumberOfGameBans"), ("daysSinceLastBan", "DaysSinceLastBan"), ("vacBanned", "VACBanned")]
            {
                data.insert(key.into(), entry[field].clone());
            }
        }
        if !api_key.is_empty() {
            self.add_hours(&mut data, steam_id, api_key).await;
        }
        ProviderResult::ok(data)
    }

    /// This player's profile and bans from Steam's API. The first lookup in a lobby asks about everyone in
    /// it (one request for profiles, one for bans); the others then use those answers.
    async fn lobby_answer(&self, steam_id: &str, key: &str, lobby: &[String]) -> Result<LobbyAnswer, Failure> {
        let mut answers = self.lobby.lock().await;
        let now = crate::model::now_ms();
        answers.retain(|_, answer| now - answer.at < BATCH_KEEP_MS);
        if let Some(answer) = answers.remove(steam_id) {
            return Ok(answer);
        }
        let ids = batch_ids(steam_id, lobby, &answers);
        let key = percent_encoding::utf8_percent_encode(key, percent_encoding::NON_ALPHANUMERIC).to_string();
        let api = "https://api.steampowered.com/ISteamUser";
        let text = self.get(&format!("{api}/GetPlayerSummaries/v2/?key={key}&steamids={}", ids.join(","))).await?;
        let json: Value = serde_json::from_str(&text).map_err(|e| Failure::with("error", format!("Steam API: {e}")))?;
        let mut profiles = parse_player_summaries(&json);
        let bans = self.get(&format!("{api}/GetPlayerBans/v1/?key={key}&steamids={}", ids.join(","))).await;
        let mut bans = bans.ok().and_then(|text| serde_json::from_str::<Value>(&text).ok()).map(|json| parse_player_bans(&json));
        let at = crate::model::now_ms();
        for id in &ids {
            let answer = LobbyAnswer { at, profile: profiles.remove(id), bans: bans.as_mut().and_then(|b| b.remove(id)) };
            answers.insert(id.clone(), answer);
        }
        answers.remove(steam_id).ok_or_else(|| Failure::new("not-found"))
    }

    async fn profile_xml(&self, profile_url: &str) -> Result<Map<String, Value>, Failure> {
        let xml = self.get(&format!("{profile_url}/?xml=1")).await?;
        parse_profile_xml(&xml).ok_or_else(|| Failure::new("not-found"))
    }

    /// CS2 hours, which Steam's API gives one account at a time.
    async fn add_hours(&self, data: &mut Map<String, Value>, steam_id: &str, key: &str) {
        let key = percent_encoding::utf8_percent_encode(key, percent_encoding::NON_ALPHANUMERIC).to_string();
        let games = self
            .get(&format!(
                "https://api.steampowered.com/IPlayerService/GetOwnedGames/v1/?key={key}&steamid={steam_id}&include_played_free_games=1&appids_filter%5B0%5D=730"
            ))
            .await;
        let reason = match games.as_ref().map(|text| serde_json::from_str::<Value>(text)) {
            Ok(Ok(json)) => match json["response"]["games"].as_array().and_then(|g| g.iter().find(|game| game["appid"] == 730)) {
                Some(game) => {
                    data.insert("hoursCs2".into(), json!((game["playtime_forever"].as_f64().unwrap_or(0.0) / 60.0).round()));
                    String::new()
                }
                None => "Game details are private".into(),
            },
            Ok(Err(error)) => format!("Steam API error: {error}"),
            Err(failure) => format!("Steam API {}", failure.status),
        };
        data.insert("hoursReason".into(), reason.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_community_xml() {
        let xml = "<?xml version=\"1.0\"?><profile><steamID><![CDATA[Some Name]]></steamID><privacyState>public</privacyState><vacBanned>0</vacBanned><tradeBanState>None</tradeBanState><isLimitedAccount>0</isLimitedAccount><avatarMedium><![CDATA[https://avatars.steamstatic.com/a_medium.jpg]]></avatarMedium><memberSince>March 3, 2012</memberSince><groups><group><steamID>Not me</steamID></group></groups></profile>";
        let profile = parse_profile_xml(xml).unwrap();
        assert_eq!(profile["name"], "Some Name");
        assert_eq!(profile["vacBanned"], false);
        assert_eq!(profile["createdAt"], 1_330_732_800_000i64);
        assert_eq!(profile["avatar"], "https://avatars.steamstatic.com/a_medium.jpg");
        assert!(parse_profile_xml("<response><error>nope</error></response>").is_none());
        let private = parse_profile_xml("<profile><steamID>x</steamID></profile>").unwrap();
        assert_eq!(private["vacBanned"], Value::Null, "unknown is not 'not banned'");
    }

    #[test]
    fn pauses_grow_and_api_profiles_match_the_xml_fields() {
        assert_eq!([0, 1, 2, 3, 9].map(pause_after), [10, 20, 40, 60, 60].map(|m: i64| m * 60_000));
        assert!(is_api("https://api.steampowered.com/ISteamUser/GetPlayerSummaries/v2/?key=k&steamids=1"));
        assert!(!is_api("https://steamcommunity.com/profiles/1/?xml=1"), "the community pages are paused separately");
        let json = json!({ "response": { "players": [
            { "steamid": "1", "personaname": "Pechkin", "avatarmedium": "https://avatars.steamstatic.com/a_medium.jpg", "communityvisibilitystate": 3, "timecreated": 1_500_000_000 },
            { "steamid": "2", "personaname": "Hidden", "communityvisibilitystate": 1 }
        ] } });
        let profiles = parse_player_summaries(&json);
        let profile = &profiles["1"];
        assert_eq!((profile["name"].as_str(), profile["privacy"].as_str()), (Some("Pechkin"), Some("public")));
        assert_eq!(profile["avatar"], "https://avatars.steamstatic.com/a_medium.jpg");
        assert_eq!(profile["createdAt"], 1_500_000_000_000i64);
        assert_eq!(profiles["2"]["privacy"], "private");
        assert!(!profiles.contains_key("3"), "unknown accounts are missing");
        let bans = parse_player_bans(
            &json!({ "players": [{ "SteamId": "2", "NumberOfVACBans": 1, "NumberOfGameBans": 0, "VACBanned": true, "DaysSinceLastBan": 300 }] }),
        );
        assert_eq!(bans["2"]["NumberOfVACBans"], 1);
    }

    #[test]
    fn one_request_covers_the_lobby() {
        let lobby: Vec<String> = ["a", "b", "c", "", "b"].map(String::from).to_vec();
        let mut answered = HashMap::new();
        assert_eq!(batch_ids("b", &lobby, &answered), ["b", "a", "c"], "this player first, no blanks or repeats");
        answered.insert("a".to_string(), LobbyAnswer { at: 0, profile: None, bans: None });
        assert_eq!(batch_ids("x", &lobby, &answered), ["x", "b", "c"], "lobby members already answered are left out");
        let big: Vec<String> = (0..250).map(|n| n.to_string()).collect();
        assert_eq!(batch_ids("me", &big, &HashMap::new()).len(), BATCH_MAX);
    }
}
