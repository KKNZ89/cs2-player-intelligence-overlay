//! Explained comparisons between a player's numbers, for the profile indicator, playstyle tendencies and
//! notable players. Every rule names the values it uses and runs only when they exist; nothing here is a
//! verdict. A "Review" profile is a statistical discrepancy between sources, not evidence of cheating, and
//! there are no cheating probabilities. Thresholds are fixed rules of thumb, not reference-group
//! percentiles (no reference data is available), and are named as such.

use crate::model::Player;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    /// "normal", "review" or "insufficient".
    pub indicator: &'static str,
    /// "low", "medium" or "high": how much of the comparison could be made, and how fresh the data is.
    pub confidence: &'static str,
    /// Discrepancies found, in plain words.
    pub reasons: Vec<String>,
    /// Comparisons that could be made.
    pub checked: Vec<String>,
    /// Historical tendencies (Leetify), when the sample is large enough.
    pub tendencies: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Leetify matches needed before mechanics-based rules and tendencies are used.
    pub min_matches: f64,
    pub now_ms: i64,
}

struct Inputs {
    premier: Option<f64>,
    faceit_level: Option<f64>,
    faceit_matches: Option<f64>,
    hours: Option<f64>,
    created_at: Option<f64>,
    aim: Option<f64>,
    leetify_matches: Option<f64>,
    stale: bool,
}

fn inputs(p: &Player) -> Inputs {
    let ok = |name: &str| p.provider(name).filter(|r| r.is_ok());
    let number = |name: &str, key: &str| ok(name).and_then(|r| r.number(key));
    Inputs {
        premier: p.premier(),
        // FACEIT's own API first; Leetify reports the level too.
        faceit_level: number("faceit", "level").or_else(|| number("leetify", "faceitLevel")).filter(|l| *l > 0.0),
        faceit_matches: number("faceit", "matches"),
        hours: number("steam", "hoursCs2"),
        created_at: number("steam", "createdAt"),
        aim: number("leetify", "aim"),
        leetify_matches: number("leetify", "totalMatches"),
        stale: ["leetify", "steam", "faceit"].iter().any(|name| ok(name).is_some_and(|r| r.stale)),
    }
}

const YEAR_MS: f64 = 365.25 * 86_400_000.0;

pub fn analyze(p: &Player, options: Options) -> Analysis {
    let i = inputs(p);
    let mut checked = Vec::new();
    let mut reasons = Vec::new();
    let mut rule = |name: &str, evaluable: bool, hit: bool, reason: String| {
        if evaluable {
            checked.push(name.to_string());
            if hit {
                reasons.push(reason);
            }
        }
    };
    let enough_matches = i.leetify_matches.is_some_and(|m| m >= options.min_matches);
    if let (Some(premier), Some(level)) = (i.premier, i.faceit_level) {
        // A level from only a handful of FACEIT matches says little.
        let evaluable = i.faceit_matches.is_none_or(|m| m >= 20.0);
        rule("Premier vs FACEIT", evaluable, premier >= 20_000.0 && level <= 3.0, format!("Premier {:.0} is well above FACEIT level {level:.0}", premier));
    }
    if let (Some(premier), Some(hours)) = (i.premier, i.hours) {
        rule("Premier vs CS2 hours", true, premier >= 18_000.0 && hours < 300.0, format!("Premier {premier:.0} with {hours:.0} CS2 hours on the account"));
    }
    if let (Some(premier), Some(created)) = (i.premier, i.created_at) {
        let years = (options.now_ms as f64 - created) / YEAR_MS;
        rule("Premier vs account age", true, premier >= 20_000.0 && years < 1.0, format!("Premier {premier:.0} on an account under a year old"));
    }
    if let (Some(aim), Some(premier)) = (i.aim, i.premier) {
        rule(
            "Leetify aim vs Premier",
            enough_matches,
            aim >= 90.0 && premier < 12_000.0,
            format!("Leetify aim {aim:.0} is unusually high for Premier {premier:.0}"),
        );
    }
    let indicator = if checked.len() < 2 {
        "insufficient"
    } else if reasons.len() >= 2 {
        "review"
    } else {
        "normal"
    };
    let mut confidence = match checked.len() {
        0..=2 => 0,
        3 => 1,
        _ => 2,
    };
    if i.stale && confidence > 0 {
        confidence -= 1;
    }
    Analysis {
        indicator,
        confidence: ["low", "medium", "high"][confidence],
        reasons,
        checked,
        tendencies: if enough_matches { tendencies(p) } else { Vec::new() },
    }
}

/// Historical tendencies from Leetify's aggregates; described as past behaviour, not predictions.
fn tendencies(p: &Player) -> Vec<String> {
    let Some(leetify) = p.provider("leetify").filter(|r| r.is_ok()) else { return Vec::new() };
    let mut list = Vec::new();
    if let (Some(ct), Some(t)) = (leetify.number("ctOpeningDuelSuccess"), leetify.number("tOpeningDuelSuccess")) {
        let average = (ct + t) / 2.0;
        if average >= 55.0 {
            list.push(format!("Usually wins opening duels ({average:.0}% CT/T average)"));
        } else if average <= 45.0 {
            list.push(format!("Usually loses opening duels ({average:.0}% CT/T average)"));
        }
    }
    if let (Some(ct), Some(t)) = (leetify.number("ctLeetify"), leetify.number("tLeetify")) {
        if ct - t >= 0.02 {
            list.push("Stronger on CT than T (Leetify side ratings)".into());
        } else if t - ct >= 0.02 {
            list.push("Stronger on T than CT (Leetify side ratings)".into());
        }
    }
    if let Some(trades) = leetify.number("tradeKillsSuccess") {
        if trades >= 60.0 {
            list.push(format!("Trades teammates well ({trades:.0}% of trade chances)"));
        }
    }
    list
}

/// The player with the highest Premier rating, when at least four players in the lobby have one.
pub fn strongest(players: &[Player]) -> Option<String> {
    let rated: Vec<(&Player, f64)> = players.iter().filter_map(|p| p.premier().map(|r| (p, r))).collect();
    if rated.len() < 4 {
        return None;
    }
    rated.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(p, _)| p.steam_id.clone())
}

pub fn analyze_all(players: &[Player], options: Options) -> HashMap<String, Analysis> {
    players.iter().map(|p| (p.steam_id.clone(), analyze(p, options))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderResult;
    use serde_json::{json, Map, Value};

    const NOW: i64 = 1_800_000_000_000;

    fn result(fields: Value) -> ProviderResult {
        ProviderResult::ok(fields.as_object().cloned().unwrap_or_else(Map::new))
    }

    fn player(leetify: Value, steam: Value) -> Player {
        let mut p = Player { steam_id: "1".into(), ..Player::default() };
        p.providers.insert("leetify".into(), result(leetify));
        p.providers.insert("steam".into(), result(steam));
        p
    }

    fn options() -> Options {
        Options { min_matches: 30.0, now_ms: NOW }
    }

    #[test]
    fn two_discrepancies_make_a_review_with_reasons() {
        let p = player(
            json!({ "premier": 24000, "faceitLevel": 2, "aim": 70, "totalMatches": 200 }),
            json!({ "hoursCs2": 150, "createdAt": NOW - 5 * 365 * 86_400_000_i64 }),
        );
        let a = analyze(&p, options());
        assert_eq!(a.indicator, "review");
        assert_eq!(a.reasons.len(), 2);
        assert!(a.reasons[0].contains("FACEIT level 2"));
        assert_eq!((a.checked.len(), a.confidence), (4, "high"));
    }

    #[test]
    fn one_discrepancy_or_little_data_is_not_a_review() {
        let single = player(json!({ "premier": 24000, "faceitLevel": 2, "totalMatches": 200 }), json!({ "hoursCs2": 2500 }));
        let a = analyze(&single, options());
        assert_eq!((a.indicator, a.reasons.len(), a.confidence), ("normal", 1, "low"));
        let sparse = player(json!({ "premier": 24000 }), json!({}));
        assert_eq!(analyze(&sparse, options()).indicator, "insufficient");
        let few = player(json!({ "premier": 9000, "aim": 95, "totalMatches": 5 }), json!({ "hoursCs2": 900 }));
        assert_eq!(analyze(&few, options()).checked, ["Premier vs CS2 hours"], "mechanics need the minimum sample");
    }

    #[test]
    fn tendencies_need_a_sample_and_strongest_needs_a_rated_lobby() {
        let p = player(
            json!({ "premier": 15000, "totalMatches": 300, "ctOpeningDuelSuccess": 60, "tOpeningDuelSuccess": 56, "ctLeetify": 0.05, "tLeetify": 0.01, "tradeKillsSuccess": 64 }),
            json!({}),
        );
        let t = analyze(&p, options()).tendencies;
        assert_eq!(t.len(), 3);
        assert!(t[0].starts_with("Usually wins opening duels (58%"));
        let small = player(json!({ "premier": 15000, "totalMatches": 10, "ctOpeningDuelSuccess": 60, "tOpeningDuelSuccess": 60 }), json!({}));
        assert!(analyze(&small, options()).tendencies.is_empty());
        let lobby: Vec<Player> = (0..4)
            .map(|i| {
                let mut p = player(json!({ "premier": 10000 + i * 1000 }), json!({}));
                p.steam_id = i.to_string();
                p
            })
            .collect();
        assert_eq!(strongest(&lobby).as_deref(), Some("3"));
        assert_eq!(strongest(&lobby[..3]), None);
    }
}
