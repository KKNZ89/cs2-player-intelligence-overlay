//! The players in the current match with every provider's answer. Lookups themselves run in the engine;
//! this keeps the bookkeeping: who needs which lookup, which answers still apply, and side assignments.

use crate::model::{now_ms, Candidate, Player, ProviderResult, SideSource};
use crate::providers::PROVIDERS;
use crate::roster_input::numeric_steam_id;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Failures worth a diagnostics line.
const NOTEWORTHY: [&str; 6] = ["auth-failed", "rate-limited", "timeout", "verification-required", "auth-required", "consent-required"];
/// Expected answers that explain an N/A but are not problems worth logging.
const QUIET: [&str; 8] = ["not-found", "no-own-history", "waiting-for-self", "self", "disabled", "cancelled", "paused", "missing-api-key"];
/// Transient failures worth retrying automatically; definitive answers (not-found, disabled, ...) are not.
/// "paused" answers at once without a lookup while a site is paused, so retrying it costs nothing.
const RETRYABLE: [&str; 7] = ["error", "timeout", "rate-limited", "verification-required", "consent-required", "cancelled", "paused"];

fn retryable(result: Option<&ProviderResult>) -> bool {
    result.is_some_and(|r| RETRYABLE.contains(&r.status.as_str()) || r.status.starts_with("http-5") || r.stale)
}

/// Whether a lookup result deserves a diagnostics line, and the line.
pub fn noteworthy(source: &str, result: &ProviderResult) -> Option<String> {
    if QUIET.contains(&result.status.as_str()) || (result.error.is_none() && !NOTEWORTHY.contains(&result.status.as_str())) {
        return None;
    }
    Some(format!("{source}-lookup: {}: {}", result.status, result.error.as_deref().unwrap_or("Open provider profile or check setup.")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Force {
    /// Only providers that have not answered yet.
    Pending,
    /// Every provider.
    All,
    /// Only transient failures and stale values.
    Failed,
}

pub struct InFlight {
    pub task: u64,
    pub abort: tokio::task::AbortHandle,
    pub done: tokio::sync::watch::Receiver<bool>,
}

#[derive(Default)]
pub struct PlayerData {
    players: Vec<Player>,
    /// Increments on every reset, so answers for a previous match are dropped.
    pub generation: u64,
    pub in_flight: HashMap<String, InFlight>,
    next_task: u64,
}

impl PlayerData {
    pub fn reset(&mut self) {
        self.generation += 1;
        for task in self.in_flight.values() {
            task.abort.abort();
        }
        self.in_flight.clear();
        self.players.clear();
    }

    pub fn get(&self, steam_id: &str) -> Option<&Player> {
        self.players.iter().find(|p| p.steam_id == steam_id)
    }

    /// Records a teammate colour read from the scoreboard; true when it changed.
    pub fn set_colour(&mut self, steam_id: &str, colour: &str) -> bool {
        match self.get_mut(steam_id) {
            Some(player) if player.colour.as_deref() != Some(colour) => {
                player.colour = Some(colour.to_string());
                true
            }
            _ => false,
        }
    }

    /// Whether reading the scoreboard could still add colours: CS2 colours at most five teammates.
    pub fn wants_colours(&self) -> bool {
        self.players.iter().filter(|p| p.colour.is_some()).count() < 5 && self.players.iter().any(|p| p.colour.is_none())
    }

    fn get_mut(&mut self, steam_id: &str) -> Option<&mut Player> {
        self.players.iter_mut().find(|p| p.steam_id == steam_id)
    }

    fn cancel(&mut self, steam_id: &str) {
        if let Some(task) = self.in_flight.remove(steam_id) {
            task.abort.abort();
        }
    }

    /// Adds or refreshes roster entries; `replace` drops everyone not in the new roster.
    pub fn seed_roster(&mut self, roster: &[Candidate], replace: bool) {
        if replace {
            let gone: Vec<String> = self.players.iter().filter(|p| !roster.iter().any(|c| c.steam_id == p.steam_id)).map(|p| p.steam_id.clone()).collect();
            for id in gone {
                self.cancel(&id);
                self.players.retain(|p| p.steam_id != id);
            }
        }
        for item in roster.iter().filter(|c| !c.steam_id.is_empty()) {
            let existing = self.get(&item.steam_id).cloned().unwrap_or_default();
            let name = if !item.name.is_empty() && item.name != item.steam_id {
                item.name.clone()
            } else if !existing.name.is_empty() {
                existing.name.clone()
            } else {
                item.steam_id.clone()
            };
            let or = |a: &str, b: &str, c: &str| {
                if !a.is_empty() {
                    a.to_string()
                } else if !b.is_empty() {
                    b.to_string()
                } else {
                    c.to_string()
                }
            };
            let mut player = Player {
                steam_id: item.steam_id.clone(),
                name,
                is_self: item.is_self,
                team: or(&item.team, &existing.team, ""),
                side: if item.is_self { "team".into() } else { existing.side.clone() },
                side_source: if item.is_self { SideSource::Own } else { existing.side_source },
                side_reason: existing.side_reason.clone(),
                source: or(&item.source, &existing.source, "manual"),
                confidence: or(&item.confidence, &existing.confidence, "candidate"),
                seen_at: item.seen_at.unwrap_or(if existing.steam_id.is_empty() { now_ms() } else { existing.seen_at }),
                party: item.party.clone().or(existing.party.clone()),
                colour: existing.colour.clone(),
                providers: Default::default(),
            };
            for name in PROVIDERS {
                let value = if name == "history" && item.is_self {
                    ProviderResult::status("self")
                } else {
                    existing.providers.get(name).cloned().unwrap_or_else(|| ProviderResult::status("pending"))
                };
                player.providers.insert(name.into(), value);
            }
            match self.get_mut(&item.steam_id) {
                Some(slot) => *slot = player,
                None => self.players.push(player),
            }
        }
    }

    pub fn add_manual(&mut self, input: &str) -> Result<String, String> {
        let id = numeric_steam_id(input);
        if id.is_empty() {
            return Err("Use a numeric Steam profile link or a 17-digit SteamID64 beginning with 7656119.".into());
        }
        let candidate =
            Candidate { steam_id: id.clone(), name: id.clone(), source: "manual".into(), confidence: "user-selected".into(), ..Candidate::default() };
        self.seed_roster(&[candidate], false);
        Ok(id)
    }

    pub fn set_side(&mut self, steam_id: &str, side: &str) -> Result<(), String> {
        if !matches!(side, "" | "team" | "enemy") {
            return Err("Side must be team, enemy or empty.".into());
        }
        let player = self.get_mut(steam_id).ok_or("Unknown player.")?;
        if player.is_self {
            return Err("You are always on your own team.".into());
        }
        player.side = side.into();
        player.side_source = if side.is_empty() { SideSource::None } else { SideSource::Manual };
        player.side_reason.clear();
        Ok(())
    }

    /// A side inferred from who plays together. Only changes players without a certain side; returns
    /// whether anything changed.
    pub fn set_inferred_side(&mut self, steam_id: &str, side: &str, reason: &str) -> bool {
        let Some(player) = self.get_mut(steam_id) else { return false };
        if player.is_self || matches!(player.side_source, SideSource::Manual | SideSource::Spectated | SideSource::Scoreboard) {
            return false;
        }
        let (source, reason) = if side.is_empty() { (SideSource::None, "") } else { (SideSource::Likely, reason) };
        if player.side == side && player.side_source == source && player.side_reason == reason {
            return false;
        }
        player.side = side.into();
        player.side_source = source;
        player.side_reason = reason.into();
        true
    }

    /// A side read from CS2's scoreboard. Your own choice and spectating (which proves a teammate) win.
    pub fn set_scoreboard_side(&mut self, steam_id: &str, side: &str) -> bool {
        let Some(player) = self.get_mut(steam_id) else { return false };
        if player.is_self || matches!(player.side_source, SideSource::Manual | SideSource::Spectated) || !matches!(side, "team" | "enemy") {
            return false;
        }
        if player.side == side && player.side_source == SideSource::Scoreboard {
            return false;
        }
        player.side = side.into();
        player.side_source = SideSource::Scoreboard;
        player.side_reason = if side == "team" { "In your team on CS2's scoreboard" } else { "In the other team on CS2's scoreboard" }.into();
        true
    }

    /// Whether some player's side is still not certain.
    pub fn wants_sides(&self) -> bool {
        self.players.iter().any(|p| !p.is_self && !p.side_source.is_certain())
    }

    /// A teammate revealed by GSI while you spectate. Your own choice for that player always wins.
    pub fn confirm_teammate(&mut self, steam_id: &str) -> bool {
        let Some(player) = self.get_mut(steam_id) else { return false };
        if player.is_self || matches!(player.side_source, SideSource::Manual | SideSource::Spectated) {
            return false;
        }
        player.side = "team".into();
        player.side_source = SideSource::Spectated;
        player.side_reason = "Seen while spectating".into();
        true
    }

    pub fn remove(&mut self, steam_id: &str) {
        self.cancel(steam_id);
        self.players.retain(|p| p.steam_id != steam_id);
    }

    /// Providers a lookup should run for this player now.
    pub fn due(&self, steam_id: &str, force: Force, self_id: &str) -> Vec<&'static str> {
        let Some(player) = self.get(steam_id) else { return Vec::new() };
        PROVIDERS
            .into_iter()
            .filter(|name| {
                // "history" compares a player with you, so it waits for your own SteamID and skips your row.
                if *name == "history" && (player.is_self || self_id.is_empty()) {
                    return false;
                }
                let current = player.provider(name);
                match force {
                    Force::Failed => retryable(current),
                    Force::All => true,
                    Force::Pending => current.is_none_or(|r| r.status == "pending"),
                }
            })
            .collect()
    }

    pub fn next_task(&mut self) -> u64 {
        self.next_task += 1;
        self.next_task
    }

    pub fn is_current(&self, steam_id: &str, generation: u64, task: u64) -> bool {
        generation == self.generation && self.in_flight.get(steam_id).is_some_and(|f| f.task == task) && self.get(steam_id).is_some()
    }

    /// Stores one provider's answer. A failed refresh never discards good data: the previous value is
    /// kept and marked stale with the reason.
    pub fn apply_result(&mut self, steam_id: &str, source: &str, result: ProviderResult) {
        let Some(player) = self.get_mut(steam_id) else { return };
        if let Some(name) = result.text("name").filter(|n| !n.is_empty()) {
            if player.name.is_empty() || player.name == player.steam_id {
                player.name = name.to_string();
            }
        }
        let previous = player.providers.get(source);
        let next = match previous {
            Some(previous) if !result.is_ok() && previous.is_ok() => {
                let mut kept = previous.clone();
                kept.stale = true;
                kept.stale_reason = Some(result.error.clone().unwrap_or(result.status.clone()));
                kept.data.insert("checkedAt".into(), now_ms().into());
                kept
            }
            _ => {
                let mut fresh = result;
                fresh.fetched_at = Some(now_ms());
                fresh.data.insert("source".into(), source.into());
                fresh
            }
        };
        player.providers.insert(source.into(), next);
    }

    /// Uses a result fetched earlier (for a player met again), keeping its original fetch time.
    pub fn apply_cached(&mut self, steam_id: &str, source: &str, result: ProviderResult) {
        if let Some(player) = self.get_mut(steam_id) {
            if result.text("name").is_some_and(|n| !n.is_empty()) && (player.name.is_empty() || player.name == player.steam_id) {
                player.name = result.text("name").unwrap_or_default().to_string();
            }
            player.providers.insert(source.into(), result);
        }
    }

    /// One line per player explaining any gaps, so missing columns can be traced in the log.
    pub fn gaps(&self, steam_id: &str, providers: &[&str]) -> Option<String> {
        let player = self.get(steam_id)?;
        let gaps: Vec<String> = providers
            .iter()
            .filter_map(|name| {
                let status = player.provider(name).map(|r| r.status.as_str()).unwrap_or("pending");
                // Optional sources that are switched off are not gaps.
                (!matches!(status, "ok" | "missing-api-key" | "disabled")).then(|| format!("{name} {status}"))
            })
            .collect();
        (!gaps.is_empty()).then(|| format!("…{}: {}", &steam_id[steam_id.len().saturating_sub(4)..], gaps.join(", ")))
    }

    /// Counts for the live panel: how many players each provider has answered for.
    pub fn coverage(&self) -> Value {
        let mut result = json!({ "players": self.players.len() });
        let mut latest: Option<i64> = None;
        for name in PROVIDERS {
            let count = |status: &str| self.players.iter().filter(|p| p.provider(name).is_some_and(|r| r.status == status)).count();
            result[name] = json!({ "loaded": count("ok"), "pending": count("pending") });
            for player in &self.players {
                if let Some(at) = player.provider(name).and_then(|r| r.fetched_at) {
                    latest = Some(latest.map_or(at, |l| l.max(at)));
                }
            }
        }
        result["lastFetchedAt"] = latest.map(Value::from).unwrap_or(Value::Null);
        result
    }

    /// You first, then players you added, then console sightings, then Steam candidates.
    pub fn list(&self) -> Vec<Player> {
        let priority = |p: &Player| {
            if p.is_self {
                3
            } else if p.source == "manual" {
                2
            } else if p.source == "console-status" {
                1
            } else {
                0
            }
        };
        let mut players = self.players.clone();
        players.sort_by_key(|p| std::cmp::Reverse(priority(p)));
        players
    }

    pub fn ids(&self) -> Vec<String> {
        self.players.iter().map(|p| p.steam_id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    const ME: &str = "76561198000000000";
    const A: &str = "76561198000000001";

    fn candidate(id: &str, is_self: bool) -> Candidate {
        Candidate { steam_id: id.into(), name: id.into(), is_self, source: if is_self { "gsi" } else { "steam-coplay" }.into(), ..Candidate::default() }
    }

    fn ok_with(key: &str, value: Value) -> ProviderResult {
        let mut data = Map::new();
        data.insert(key.into(), value);
        ProviderResult::ok(data)
    }

    #[test]
    fn seeding_keeps_existing_data_and_marks_self() {
        let mut data = PlayerData::default();
        data.seed_roster(&[candidate(ME, true), candidate(A, false)], true);
        assert_eq!(data.get(ME).unwrap().provider("history").unwrap().status, "self");
        assert_eq!(data.get(ME).unwrap().side, "team");
        assert_eq!(data.due(A, Force::Pending, ""), ["csrep", "csstats", "leetify", "steam", "faceit"], "history waits for your ID");
        assert_eq!(data.due(A, Force::Pending, ME).len(), 6);
        data.apply_result(A, "leetify", ok_with("name", "Real Name".into()));
        assert_eq!(data.get(A).unwrap().name, "Real Name");
        data.seed_roster(&[candidate(A, false)], true);
        assert!(data.get(ME).is_none(), "replace drops players not in the roster");
        assert!(data.get(A).unwrap().provider("leetify").unwrap().is_ok(), "known data is kept");
        assert_eq!(data.get(A).unwrap().name, "Real Name");
    }

    #[test]
    fn failed_refreshes_keep_good_data_as_stale() {
        let mut data = PlayerData::default();
        data.seed_roster(&[candidate(A, false)], false);
        data.apply_result(A, "csrep", ok_with("trust", 87.into()));
        data.apply_result(A, "csrep", ProviderResult::failed("timeout", "slow"));
        let kept = data.get(A).unwrap().provider("csrep").unwrap();
        assert!(kept.is_ok() && kept.stale);
        assert_eq!(kept.stale_reason.as_deref(), Some("slow"));
        assert_eq!(data.due(A, Force::Failed, ME), ["csrep"], "stale values are retried");
        data.apply_result(A, "steam", ProviderResult::status("not-found"));
        assert!(!data.due(A, Force::Failed, ME).contains(&"steam"), "definitive answers are not retried");
        assert_eq!(data.gaps(A, &["csrep", "steam"]).unwrap(), "…0001: steam not-found");
        assert_eq!(data.coverage()["csrep"]["loaded"], 1);
    }

    #[test]
    fn sides_respect_certainty() {
        let mut data = PlayerData::default();
        data.seed_roster(&[candidate(ME, true), candidate(A, false)], false);
        assert!(data.set_side(ME, "enemy").is_err());
        assert!(data.set_inferred_side(A, "enemy", "party"));
        assert!(!data.set_inferred_side(A, "enemy", "party"), "no change, no update");
        assert!(data.confirm_teammate(A));
        assert!(!data.set_inferred_side(A, "enemy", "party"), "spectated sides are certain");
        data.set_side(A, "enemy").unwrap();
        assert!(!data.confirm_teammate(A), "your choice wins");
        assert!(data.set_side(A, "bogus").is_err());
        assert_eq!(data.add_manual("https://steamcommunity.com/profiles/76561198000000002").unwrap(), "76561198000000002");
        assert_eq!(data.list()[0].steam_id, ME);
        assert_eq!(data.list()[1].steam_id, "76561198000000002", "added players come next");
    }

    #[test]
    fn noteworthy_failures_are_logged() {
        assert!(noteworthy("csrep", &ProviderResult::status("not-found")).is_none());
        assert!(noteworthy("csrep", &ProviderResult::status("verification-required")).is_some());
        assert_eq!(noteworthy("leetify", &ProviderResult::failed("error", "boom")).unwrap(), "leetify-lookup: error: boom");
    }
}
