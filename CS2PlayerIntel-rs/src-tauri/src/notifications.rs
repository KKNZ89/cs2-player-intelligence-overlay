//! Short Windows notifications for information that can change a decision: a player met before, a strong
//! historical opponent, a profile flagged for review, or a data source that is down. Each is sent at most
//! once per match, batched into one notification per kind, and only at quiet moments (warmup, freeze time,
//! between rounds), so nothing pops up mid-fight.

use crate::analysis::Analysis;
use crate::gsi::GsiSummary;
use crate::match_history::HistorySummary;
use crate::model::Player;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    /// Deduplication keys covered by this notice ("encounter:<id>", "provider:csrep", ...).
    pub keys: Vec<String>,
    pub title: String,
    pub body: String,
}

/// Quiet moments: warmup, freeze time and the end of a round.
pub fn quiet_moment(g: &GsiSummary) -> bool {
    g.phase == "warmup" || matches!(g.round_phase.as_str(), "freezetime" | "over")
}

fn name(p: &Player) -> &str {
    if p.name.is_empty() {
        &p.steam_id
    } else {
        &p.name
    }
}

fn record(summary: &HistorySummary) -> String {
    let a = &summary.all;
    let mut parts = vec![format!("{}×", a.played)];
    if a.won + a.lost + a.tied > 0 {
        parts.push(format!("{}–{}{}", a.won, a.lost, if a.tied > 0 { format!("–{}", a.tied) } else { String::new() }));
    }
    parts.join(" ")
}

const PROVIDER_NAMES: [(&str, &str); 4] = [("csrep", "CSRep"), ("csstats", "CSStats"), ("leetify", "Leetify"), ("steam", "Steam")];
const PROVIDER_DOWN: [&str; 6] = ["verification-required", "auth-required", "rate-limited", "timeout", "error", "paused"];

/// What is worth telling now, leaving out anything already sent this match.
pub fn due(
    players: &[Player],
    history: &HashMap<String, HistorySummary>,
    analyses: &HashMap<String, Analysis>,
    types: &[String],
    sent: &HashSet<String>,
) -> Vec<Notice> {
    let wants = |kind: &str| types.iter().any(|t| t == kind);
    let others: Vec<&Player> = players.iter().filter(|p| !p.is_self).collect();
    let mut notices = Vec::new();

    if wants("encounter") {
        let met: Vec<(&Player, &HistorySummary)> = others
            .iter()
            .filter(|p| !sent.contains(&format!("encounter:{}", p.steam_id)))
            .filter_map(|p| history.get(&p.steam_id).filter(|h| h.all.played > 0).map(|h| (*p, h)))
            .collect();
        if !met.is_empty() {
            let body = met.iter().map(|(p, h)| format!("{} ({})", name(p), record(h))).collect::<Vec<_>>().join(", ");
            let title = if met.len() == 1 { "Played with before".to_string() } else { format!("{} players met before", met.len()) };
            notices.push(Notice { keys: met.iter().map(|(p, _)| format!("encounter:{}", p.steam_id)).collect(), title, body });
        }
    }

    if wants("strong") {
        let strong: Vec<(&Player, String)> = others
            .iter()
            .filter(|p| p.side != "team" && !sent.contains(&format!("strong:{}", p.steam_id)))
            .filter_map(|p| {
                let premier = p.premier().filter(|r| *r >= 25_000.0);
                let faceit = p
                    .provider("faceit")
                    .and_then(|r| r.number("level"))
                    .or_else(|| p.provider("leetify").and_then(|r| r.number("faceitLevel")))
                    .filter(|l| *l >= 10.0);
                let detail = match (premier, faceit) {
                    (Some(r), Some(_)) => format!("Premier {r:.0}, FACEIT 10"),
                    (Some(r), None) => format!("Premier {r:.0}"),
                    (None, Some(_)) => "FACEIT 10".into(),
                    (None, None) => return None,
                };
                Some((*p, detail))
            })
            .collect();
        if !strong.is_empty() {
            let body = strong.iter().map(|(p, detail)| format!("{} ({detail})", name(p))).collect::<Vec<_>>().join(", ");
            notices.push(Notice {
                keys: strong.iter().map(|(p, _)| format!("strong:{}", p.steam_id)).collect(),
                title: "Strong historical opponents".into(),
                body,
            });
        }
    }

    if wants("review") {
        for p in others.iter().filter(|p| !sent.contains(&format!("review:{}", p.steam_id))) {
            let Some(a) = analyses.get(&p.steam_id).filter(|a| a.indicator == "review") else { continue };
            notices.push(Notice {
                keys: vec![format!("review:{}", p.steam_id)],
                title: format!("Profile review: {}", name(p)),
                body: format!("{}. A statistical discrepancy ({} confidence), not evidence of cheating.", a.reasons.join("; "), a.confidence),
            });
        }
    }

    if wants("provider") && others.len() >= 2 {
        for (provider, label) in PROVIDER_NAMES {
            let key = format!("provider:{provider}");
            if sent.contains(&key) {
                continue;
            }
            let down: Vec<&str> =
                others.iter().filter_map(|p| p.provider(provider)).filter(|r| PROVIDER_DOWN.contains(&r.status.as_str())).map(|r| r.status.as_str()).collect();
            // Most of the lobby, so it is the source and not one profile.
            if down.len() * 2 > others.len() {
                let reason = down.first().map(|s| s.replace('-', " ")).unwrap_or_default();
                notices.push(Notice {
                    keys: vec![key],
                    title: format!("{label} unavailable"),
                    body: format!("{label} gave no data for {} of {} players ({reason}).", down.len(), others.len()),
                });
            }
        }
    }
    notices
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::match_history::Tally;
    use crate::model::ProviderResult;
    use serde_json::json;

    fn player(id: &str, name: &str, side: &str, premier: f64) -> Player {
        let mut p = Player { steam_id: id.into(), name: name.into(), side: side.into(), ..Player::default() };
        p.providers.insert("leetify".into(), ProviderResult::ok(json!({ "premier": premier }).as_object().unwrap().clone()));
        p
    }

    fn types() -> Vec<String> {
        ["encounter", "strong", "review", "provider"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn batches_encounters_and_strong_opponents_once() {
        let mut me = player("me", "Me", "team", 15000.0);
        me.is_self = true;
        let players = vec![me, player("a", "Kestrel", "team", 26000.0), player("b", "ZeroDay", "enemy", 27000.0), player("c", "nova", "", 9000.0)];
        let mut history = HashMap::new();
        history.insert("a".to_string(), HistorySummary { all: Tally { played: 3, won: 2, lost: 1, ..Tally::default() }, ..HistorySummary::default() });
        history.insert("c".to_string(), HistorySummary { all: Tally { played: 1, unknown: 1, ..Tally::default() }, ..HistorySummary::default() });
        let notices = due(&players, &history, &HashMap::new(), &types(), &HashSet::new());
        assert_eq!(notices[0].title, "2 players met before");
        assert_eq!(notices[0].body, "Kestrel (3× 2–1), nova (1×)");
        assert_eq!(notices[1].body, "ZeroDay (Premier 27000)", "teammates are not announced as strong opponents");
        let sent: HashSet<String> = notices.iter().flat_map(|n| n.keys.clone()).collect();
        assert!(due(&players, &history, &HashMap::new(), &types(), &sent).is_empty(), "each is sent once per match");
        assert!(due(&players, &history, &HashMap::new(), &["review".to_string()], &HashSet::new()).is_empty(), "types can be switched off");
    }

    #[test]
    fn a_source_is_reported_down_only_for_most_of_the_lobby() {
        let mut players: Vec<Player> = (0..4).map(|i| player(&i.to_string(), "P", "", 10000.0)).collect();
        for p in &mut players[..3] {
            p.providers.insert("csrep".into(), ProviderResult::status("verification-required"));
        }
        let notices = due(&players, &HashMap::new(), &HashMap::new(), &["provider".to_string()], &HashSet::new());
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].body, "CSRep gave no data for 3 of 4 players (verification required).");
        players[2].providers.insert("csrep".into(), ProviderResult::status("ok"));
        players[1].providers.insert("csrep".into(), ProviderResult::status("ok"));
        assert!(due(&players, &HashMap::new(), &HashMap::new(), &["provider".to_string()], &HashSet::new()).is_empty());
    }

    #[test]
    fn quiet_moments_only() {
        assert!(quiet_moment(&GsiSummary { phase: "warmup".into(), ..GsiSummary::default() }));
        assert!(quiet_moment(&GsiSummary { phase: "live".into(), round_phase: "freezetime".into(), ..GsiSummary::default() }));
        assert!(!quiet_moment(&GsiSummary { phase: "live".into(), round_phase: "live".into(), ..GsiSummary::default() }));
    }
}
