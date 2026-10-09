//! CSStats.gg profile pages. They need a signed-in browser profile; the user signs in once in the visible
//! window, which shares its profile with the hidden lookups.

use super::scraper::{PageScraper, ReadOptions};
use crate::model::ProviderResult;
use regex::Regex;
use serde_json::{Map, Value};

const CLICK_LOAD_STATS: &str = r#"(() => { const button = [...document.querySelectorAll("button")].find(x => (x.innerText || "").trim().toLowerCase() === "load stats"); if (!button) return false; button.click(); return true; })()"#;

pub fn profile_url(steam_id: &str) -> String {
    format!("https://csstats.gg/player/{steam_id}")
}

/// A number next to one of the labels ("K/D 1.2" or "1.2 K/D"), optionally followed by a percent sign.
pub fn parse_number(text: &str, labels: &[&str], percent: bool) -> Option<f64> {
    let suffix = if percent { r"\s*%" } else { "" };
    for label in labels {
        let escaped = regex::escape(label);
        let patterns = [
            format!(r"(?i)(?:^|\s){escaped}\s*[:\-]?\s*([0-9]+(?:\.[0-9]+)?){suffix}(?:\s|$)"),
            format!(r"(?i)(?:^|\s)([0-9]+(?:\.[0-9]+)?){suffix}\s*{escaped}(?:\s|$)"),
        ];
        for pattern in patterns {
            let found = Regex::new(&pattern).ok().and_then(|re| re.captures(text)).and_then(|c| c[1].parse::<f64>().ok());
            if found.is_some() {
                return found;
            }
        }
    }
    None
}

pub fn parse_cs_stats_text(text: &str) -> Map<String, Value> {
    let fields: [(&str, &[&str], bool); 7] = [
        ("kd", &["K/D", "K/D Ratio", "KD"], false),
        ("adr", &["ADR", "Average Damage per Round"], false),
        ("hltv", &["HLTV Rating", "HLTV"], false),
        ("kast", &["KAST"], true),
        ("hs", &["HS", "HS%", "Headshot", "Headshots"], true),
        ("winRate", &["Win Rate", "Winrate"], true),
        ("matches", &["Matches", "Games"], false),
    ];
    fields.iter().map(|(key, labels, percent)| (key.to_string(), parse_number(text, labels, *percent).map(Value::from).unwrap_or(Value::Null))).collect()
}

fn ready(text: &str) -> bool {
    let stats = parse_cs_stats_text(text);
    ["kd", "adr", "hltv", "kast"].iter().any(|key| !stats[*key].is_null())
}

pub struct CsStatsProvider {
    pub scraper: PageScraper,
}

impl CsStatsProvider {
    pub fn new(scraper: PageScraper) -> Self {
        Self { scraper }
    }

    pub fn open_login(&self) -> Result<(), String> {
        self.scraper.open("https://csstats.gg/", false)
    }

    pub fn open_profile(&self, steam_id: &str, over_game: bool) -> Result<(), String> {
        self.scraper.open(&profile_url(steam_id), over_game)
    }

    pub async fn player(&self, steam_id: &str) -> ProviderResult {
        let page = match self
            .scraper
            .read(&profile_url(steam_id), ReadOptions { is_ready: &ready, click_script: Some(CLICK_LOAD_STATS), consent_script: None, is_complete: None })
            .await
        {
            Ok(page) => page,
            Err(failure) => return failure.into_result(Some(profile_url(steam_id))),
        };
        let stats = parse_cs_stats_text(&page.text);
        let has_any = stats.values().any(|v| !v.is_null());
        let mut data = Map::new();
        data.insert("parsing".into(), "unverified-text-labels".into());
        data.insert("ratingDefinition".into(), "Page label: HLTV (version unverified)".into());
        data.insert("name".into(), page.heading.trim().into());
        data.extend(stats);
        data.insert("profileUrl".into(), profile_url(steam_id).into());
        ProviderResult { status: if has_any { "ok" } else { "no-stats" }.into(), data, ..ProviderResult::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_labels_either_side_of_values() {
        let text = "Overview K/D 1.21 ADR 84.2 HLTV Rating 1.10 KAST 71% 48% HS Win Rate: 55 % Matches 312";
        let stats = parse_cs_stats_text(text);
        assert_eq!(stats["kd"], 1.21);
        assert_eq!(stats["adr"], 84.2);
        assert_eq!(stats["hltv"], 1.1);
        assert_eq!(stats["kast"], 71.0);
        assert_eq!(stats["hs"], 48.0);
        assert_eq!(stats["winRate"], 55.0);
        assert_eq!(stats["matches"], 312.0);
        assert!(ready(text));
        assert!(parse_cs_stats_text("nothing here").values().all(Value::is_null));
    }
}
