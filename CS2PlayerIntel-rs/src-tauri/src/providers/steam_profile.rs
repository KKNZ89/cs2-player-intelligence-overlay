//! Public Steam profile data. Keyless: the community XML profile (privacy, VAC flag, account age, avatar).
//! CS2 hours and game bans need a Steam Web API key, because Steam only shows game lists to signed-in
//! visitors. Missing values stay null with a reason; they are never reported as zero.

use super::{request_failure, Failure};
use crate::model::ProviderResult;
use chrono::{Datelike, NaiveDate};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
const GAP: Duration = Duration::from_millis(1000);
/// Steam's community pages rate-limit bursts; one wait and retry usually gets through.
const RATE_LIMIT_WAIT: Duration = Duration::from_secs(15);
/// When Steam still refuses after waiting, lookups stop for a while instead of keeping the limit active.
const RATE_LIMIT_PAUSE_MS: i64 = 10 * 60_000;

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

pub struct SteamProfileProvider {
    http: reqwest::Client,
    queue: tokio::sync::Mutex<()>,
    paused_until: std::sync::atomic::AtomicI64,
}

impl SteamProfileProvider {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http, queue: tokio::sync::Mutex::new(()), paused_until: std::sync::atomic::AtomicI64::new(0) }
    }

    /// Requests are serialized and spaced so a full lobby does not trip Steam's rate limit.
    async fn get(&self, url: &str) -> Result<String, Failure> {
        let _turn = self.queue.lock().await;
        use std::sync::atomic::Ordering;
        if crate::model::now_ms() < self.paused_until.load(Ordering::Relaxed) {
            return Err(Failure::with("rate-limited", "Steam is limiting profile requests; they resume within 10 minutes."));
        }
        let mut result = self.fetch(url).await;
        if result.as_ref().is_err_and(|failure| failure.status == "rate-limited") {
            tokio::time::sleep(RATE_LIMIT_WAIT).await;
            result = self.fetch(url).await;
            if result.as_ref().is_err_and(|failure| failure.status == "rate-limited") {
                self.paused_until.store(crate::model::now_ms() + RATE_LIMIT_PAUSE_MS, Ordering::Relaxed);
            }
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

    pub async fn player(&self, steam_id: &str, api_key: &str) -> ProviderResult {
        let profile_url = format!("https://steamcommunity.com/profiles/{steam_id}");
        let xml = match self.get(&format!("{profile_url}/?xml=1")).await {
            Ok(xml) => xml,
            Err(failure) => return failure.into_result(Some(profile_url)),
        };
        let Some(mut data) = parse_profile_xml(&xml) else { return Failure::new("not-found").into_result(Some(profile_url)) };
        for key in ["hoursCs2", "gameBans", "vacBans", "daysSinceLastBan"] {
            data.insert(key.into(), Value::Null);
        }
        data.insert("hoursReason".into(), "Needs a Steam Web API key".into());
        data.insert("profileUrl".into(), profile_url.into());
        if !api_key.is_empty() {
            self.add_api_data(&mut data, steam_id, api_key).await;
        }
        ProviderResult::ok(data)
    }

    async fn add_api_data(&self, data: &mut Map<String, Value>, steam_id: &str, key: &str) {
        let api = "https://api.steampowered.com";
        let key = percent_encoding::utf8_percent_encode(key, percent_encoding::NON_ALPHANUMERIC).to_string();
        let games = self
            .get(&format!("{api}/IPlayerService/GetOwnedGames/v1/?key={key}&steamid={steam_id}&include_played_free_games=1&appids_filter%5B0%5D=730"))
            .await;
        let bans = self.get(&format!("{api}/ISteamUser/GetPlayerBans/v1/?key={key}&steamids={steam_id}")).await;
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
        if let Ok(Ok(json)) = bans.as_ref().map(|text| serde_json::from_str::<Value>(text)) {
            if let Some(entry) = json["players"].get(0) {
                data.insert("vacBans".into(), entry["NumberOfVACBans"].clone());
                data.insert("gameBans".into(), entry["NumberOfGameBans"].clone());
                data.insert("daysSinceLastBan".into(), entry["DaysSinceLastBan"].clone());
                data.insert("vacBanned".into(), entry["VACBanned"].clone());
            }
        }
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
}
