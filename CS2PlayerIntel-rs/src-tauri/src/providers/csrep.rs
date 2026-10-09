//! CSRep: the API when a key is saved; otherwise the public profile page anyone can view. CSRep's own
//! assessment is shown with its exact labels and verdicts; nothing is reclassified, and a section that
//! cannot be read is reported as unavailable, never as clean.

use super::scraper::{PageScraper, ReadOptions};
use super::{request_failure, Failure};
use crate::model::ProviderResult;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;
use std::time::Duration;

/// Labels as rendered on a public csrep.gg profile (checked October 2026). Each value is on the line after
/// its label; layout changes make values unavailable rather than wrong.
const PAGE_LABELS: [(&str, &[&str]); 10] = [
    ("trust", &["TRUST SCORE"]),
    ("timeToDamageMs", &["TIME TO DAMAGE"]),
    ("reactionTimeMs", &["REACTION TIME"]),
    ("crosshairPlacement", &["CROSSHAIR PLACEMENT"]),
    ("kd", &["K/D RATIO", "K/D", "KD RATIO", "KD"]),
    ("adr", &["ADR", "AVG. DAMAGE / ROUND", "AVERAGE DAMAGE PER ROUND"]),
    ("aimAccuracy", &["AIM ACCURACY"]),
    ("headAccuracy", &["HEAD ACCURACY"]),
    ("hltv", &["HLTV RATING 2.0", "HLTV RATING", "HLTV 2.0"]),
    ("kast", &["KAST"]),
];
/// Set once the page headings have been reported for a profile without stats.
static REPORTED_HEADINGS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-?\d+(?:\.\d+)?$").unwrap());
static VERDICT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z][A-Z ]+$").unwrap());
static SAMPLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)LAST \d+ MATCHES").unwrap());
static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(.*?) - CS2 Stats & Reputation").unwrap());

pub fn profile_url(steam_id: &str) -> String {
    format!("https://csrep.gg/player/{steam_id}")
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).collect()
}

pub fn parse_csrep_text(text: &str) -> Map<String, Value> {
    let lines = lines(text);
    PAGE_LABELS
        .iter()
        .map(|(key, labels)| {
            let value = labels
                .iter()
                .find_map(|label| lines.iter().position(|l| l.to_uppercase() == *label))
                .and_then(|i| lines.get(i + 1))
                .map(|v| v.replace(',', "").trim_end_matches('%').trim().to_string())
                .unwrap_or_default();
            let parsed = if NUMBER.is_match(&value) { value.parse::<f64>().map(Value::from).unwrap_or(Value::Null) } else { Value::Null };
            (key.to_string(), parsed)
        })
        .collect()
}

/// Section headings are upper case on the page; the same words in mixed case are labels inside sections.
fn section<'a>(lines: &[&'a str], start: &str, ends: &[&str]) -> Vec<&'a str> {
    let Some(from) = lines.iter().position(|l| *l == start) else { return Vec::new() };
    let rest = &lines[from + 1..];
    match rest.iter().position(|l| ends.contains(l)) {
        Some(to) => rest[..to].to_vec(),
        None => rest.iter().take(40).copied().collect(),
    }
}

pub fn parse_csrep_assessment(text: &str) -> Map<String, Value> {
    let lines = lines(text);
    // "TRUST SCORE", value, "%", verdict (for example "EXCELLENT").
    let verdict = lines
        .iter()
        .position(|l| l.to_uppercase() == "TRUST SCORE")
        .filter(|&i| lines.get(i + 2) == Some(&"%"))
        .and_then(|i| lines.get(i + 3))
        .filter(|v| VERDICT.is_match(v))
        .map(|v| Value::from(*v))
        .unwrap_or(Value::Null);
    let pairs = |items: Vec<&str>, second: &str| -> Vec<Value> { items.chunks_exact(2).map(|pair| json!({ "label": pair[0], second: pair[1] })).collect() };
    let breakdown = pairs(section(&lines, "BREAKDOWN", &["ACCOUNT FLAGS", "STATS OVERVIEW"]), "value");
    let anomalies = pairs(section(&lines, "ANOMALIES", &["Advertisement", "REMOVE ADS", "RANKS"]), "verdict");
    let sample = SAMPLE.find(text).map(|m| {
        let s = m.as_str();
        let mut chars = s.chars();
        let first = chars.next().map(|c| c.to_string()).unwrap_or_default();
        Value::from(first + &chars.as_str().to_lowercase())
    });
    let mut map = Map::new();
    map.insert("trustVerdict".into(), verdict);
    map.insert("breakdown".into(), breakdown.into());
    map.insert("anomalies".into(), anomalies.into());
    map.insert("sample".into(), sample.unwrap_or(Value::Null));
    map
}

pub fn page_name(title: &str) -> String {
    TITLE.captures(title).map(|c| c[1].trim().to_string()).unwrap_or_default()
}

/// The first numeric value among several possible JSON paths (API field names vary).
pub fn first_number(value: &Value, paths: &[&str]) -> Value {
    for path in paths {
        let current = path.split('.').try_fold(value, |cur, part| cur.get(part));
        let parsed = match current {
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::String(s)) => s.trim().trim_end_matches('%').trim().parse::<f64>().ok(),
            _ => None,
        };
        if let Some(n) = parsed.filter(|n| n.is_finite()) {
            return n.into();
        }
    }
    Value::Null
}

/// Declines optional cookies when CSRep offers its existing "Deny all" choice.
const DENY_COOKIES: &str = r#"(() => { const button = [...document.querySelectorAll("button, a")].find(x => (x.innerText || "").trim().toLowerCase() === "deny all"); if (!button) return false; button.click(); return true; })()"#;

/// The performance stats (K/D, ADR, rating) have rendered.
/// Whether a looked-up CSRep page showed its stats overview, which CSRep only shows to signed-in visitors.
/// None for answers that say nothing about the sign-in (the API, failures).
pub fn shows_signed_in_stats(result: &crate::model::ProviderResult) -> Option<bool> {
    if !result.is_ok() || result.text("origin") != Some("public-page") {
        return None;
    }
    Some(["kd", "adr", "hltv"].iter().any(|key| result.number(key).is_some()))
}

fn has_stats(text: &str) -> bool {
    let stats = parse_csrep_text(text);
    ["kd", "adr", "hltv"].iter().any(|key| !stats[*key].is_null())
}

/// Upper-case lines of a page, which are its headings and labels, for diagnosing layout changes.
pub fn headings(text: &str) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for line in lines(text) {
        if line.len() <= 40 && line.chars().any(|c| c.is_ascii_alphabetic()) && line == line.to_uppercase() && !seen.contains(&line) {
            seen.push(line);
        }
    }
    seen.into_iter().take(40).collect::<Vec<_>>().join(" | ")
}

fn ready(text: &str) -> bool {
    // Ready once the profile has rendered; a profile without a Trust Score still has its other sections.
    !parse_csrep_text(text)["trust"].is_null() || (text.contains("\nSTATS OVERVIEW\n") && text.contains("\nANOMALIES\n"))
}

pub struct CsRepProvider {
    http: reqwest::Client,
    pub scraper: PageScraper,
}

impl CsRepProvider {
    pub fn new(http: reqwest::Client, scraper: PageScraper) -> Self {
        Self { http, scraper }
    }

    /// CSRep's home page in the visible window, to sign in through Steam on their site.
    pub fn open_login(&self) -> Result<(), String> {
        self.scraper.open("https://csrep.gg/", false)
    }

    pub fn open_profile(&self, steam_id: &str, over_game: bool) -> Result<(), String> {
        self.scraper.open(&profile_url(steam_id), over_game)
    }

    pub async fn player(&self, steam_id: &str, api_key: &str) -> ProviderResult {
        let result = if api_key.is_empty() { self.page_lookup(steam_id).await } else { self.api_lookup(steam_id, api_key).await };
        result.unwrap_or_else(|failure| failure.into_result(Some(profile_url(steam_id))))
    }

    async fn page_lookup(&self, steam_id: &str) -> Result<ProviderResult, Failure> {
        let page = self
            .scraper
            .read(
                &profile_url(steam_id),
                ReadOptions { is_ready: &ready, click_script: None, consent_script: Some(DENY_COOKIES), is_complete: Some(&has_stats) },
            )
            .await?;
        let mut data = Map::new();
        data.insert("origin".into(), "public-page".into());
        data.insert("parsing".into(), "unverified-page-text".into());
        data.insert("name".into(), page_name(&page.title).into());
        data.extend(parse_csrep_text(&page.text));
        data.extend(parse_csrep_assessment(&page.text));
        data.insert("profileUrl".into(), profile_url(steam_id).into());
        // Probe runs (CS2INTEL_PROBE_TEXT) keep the page text for checking the parser against layout changes.
        if std::env::var_os("CS2INTEL_PROBE_TEXT").is_some() {
            data.insert("pageText".into(), page.text.clone().into());
        }
        let mut result = ProviderResult::ok(data);
        // A Trust Score without stats may mean the page layout changed; its headings are logged once.
        if !has_stats(&page.text) && !REPORTED_HEADINGS.swap(true, std::sync::atomic::Ordering::Relaxed) {
            result.error = Some(format!("no K/D, ADR or rating found on the page; headings seen: {}", headings(&page.text)));
        }
        Ok(result)
    }

    async fn api_lookup(&self, steam_id: &str, key: &str) -> Result<ProviderResult, Failure> {
        let response = self
            .http
            .get(format!("https://csrep.gg/api/players/{steam_id}"))
            .header("X-API-Key", key)
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .map_err(|e| request_failure(&e))?;
        match response.status().as_u16() {
            401 | 403 => return Err(Failure::new("auth-failed")),
            404 => return Err(Failure::new("not-found")),
            429 => return Err(Failure::new("rate-limited")),
            status if !response.status().is_success() => return Err(Failure::new(&format!("http-{status}"))),
            _ => {}
        }
        let json: Value = response.json().await.map_err(|e| Failure::with("error", e.to_string()))?;
        let r = json.get("result").or_else(|| json.get("data")).unwrap_or(&json);
        let fields = json!({
            "origin": "api",
            "name": r["name"].as_str().unwrap_or_default(),
            "trust": first_number(r, &["trust_rating", "trust.score", "trustRating"]),
            "hltv": first_number(r, &["hltv_rating", "stats.hltv_rating", "stats.hltv", "performance.hltv_rating"]),
            "adr": first_number(r, &["adr", "stats.adr", "performance.adr"]),
            "kast": first_number(r, &["kast", "stats.kast", "performance.kast"]),
            "kd": first_number(r, &["kd", "k_d", "stats.kd", "stats.kd_ratio", "performance.kd"]),
            "timeToDamageMs": first_number(r, &["time_to_damage", "stats.time_to_damage", "performance.time_to_damage"]),
            "headAccuracy": first_number(r, &["head_accuracy", "stats.head_accuracy", "performance.head_accuracy"]),
            "bans": r.get("bans").cloned().unwrap_or(Value::Null),
            "profileUrl": profile_url(steam_id),
        });
        Ok(ProviderResult::ok(fields.as_object().cloned().unwrap_or_default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "Someone\nTRUST SCORE\n87\n%\nEXCELLENT\nBREAKDOWN\nAccount age\nGood\nPlaytime\nAverage\nACCOUNT FLAGS\nSTATS OVERVIEW\nLAST 25 MATCHES\nTIME TO DAMAGE\n512\nK/D RATIO\n1.12\nADR\n81.4\nHLTV RATING 2.0\n1.05\nKAST\n72\nHEAD ACCURACY\n1,234\nANOMALIES\nAim\nNormal\nReaction\nNormal\nRANKS\n";

    #[test]
    fn parses_public_page_text() {
        let stats = parse_csrep_text(PAGE);
        assert_eq!(stats["trust"], 87.0);
        assert_eq!(stats["timeToDamageMs"], 512.0);
        assert_eq!(stats["headAccuracy"], 1234.0);
        assert_eq!(stats["reactionTimeMs"], Value::Null, "absent labels stay null");
        let assessment = parse_csrep_assessment(PAGE);
        assert_eq!(assessment["trustVerdict"], "EXCELLENT");
        assert_eq!(assessment["breakdown"], json!([{ "label": "Account age", "value": "Good" }, { "label": "Playtime", "value": "Average" }]));
        assert_eq!(assessment["anomalies"][1], json!({ "label": "Reaction", "verdict": "Normal" }));
        assert_eq!(assessment["sample"], "Last 25 matches");
        assert!(ready(PAGE));
        assert!(!ready("Loading"));
        assert!(has_stats(PAGE) && !has_stats("TRUST SCORE\n87\n%"));
        assert_eq!(parse_csrep_text("K/D\n1.30\nADR\n77.2\nHLTV RATING\n1.08\nKAST\n71%")["kast"], 71.0, "alternative labels and percent signs");
        assert_eq!(parse_csrep_text("K/D\n1.30")["kd"], 1.3);
        assert!(headings(PAGE).starts_with("TRUST SCORE | EXCELLENT | BREAKDOWN"));
    }

    #[test]
    fn missing_sections_are_unavailable_not_clean() {
        let assessment = parse_csrep_assessment("TRUST SCORE\n50\n");
        assert_eq!(assessment["trustVerdict"], Value::Null);
        assert_eq!(assessment["anomalies"], json!([]));
    }

    #[test]
    fn names_and_api_numbers() {
        assert_eq!(page_name("Someone - CS2 Stats & Reputation | CSRep"), "Someone");
        let r = json!({ "trust": { "score": "88%" }, "stats": { "adr": 79.5 }, "kd": "" });
        assert_eq!(first_number(&r, &["trust_rating", "trust.score"]), 88.0);
        assert_eq!(first_number(&r, &["adr", "stats.adr"]), 79.5);
        assert_eq!(first_number(&r, &["kd"]), Value::Null);
    }

    #[test]
    fn stats_on_a_page_mean_signed_in() {
        let page = |kd: Value| {
            let mut data = Map::new();
            data.insert("origin".into(), "public-page".into());
            data.insert("trust".into(), json!(87));
            data.insert("kd".into(), kd);
            crate::model::ProviderResult::ok(data)
        };
        assert_eq!(shows_signed_in_stats(&page(json!(1.12))), Some(true));
        assert_eq!(shows_signed_in_stats(&page(Value::Null)), Some(false));
        let mut api = page(json!(1.12));
        api.data.insert("origin".into(), "api".into());
        assert_eq!(shows_signed_in_stats(&api), None, "the API needs no sign-in");
        assert_eq!(shows_signed_in_stats(&crate::model::ProviderResult::status("verification-required")), None);
    }
}
