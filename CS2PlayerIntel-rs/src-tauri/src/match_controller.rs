//! Everything about the current match that is not a window: who is in it (automatic discovery, your own
//! selection, removals, spectated teammates), when it starts and ends, and its history record.

use crate::analysis;
use crate::gsi::GsiSummary;
use crate::lifecycle::{self, MatchLifecycle};
use crate::match_history::{match_result, HistoryPlayer, MatchHistoryStore, Snapshot};
use crate::model::Player;
use crate::model::{is_steam_id, Candidate, SideSource};
use crate::modes::{mode_info, team_locked, ModeInfo};
use crate::player_data::PlayerData;
use crate::prediction::predict;
use crate::roster::RosterState;
use crate::roster_input::parse_roster_input;
use crate::team_inference::{frequent_partners, infer_sides};
use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(Default)]
struct Current {
    /// When this match started (this app's clock); notes written during it are linked by it.
    started_at: i64,
    map: String,
    mode: String,
    self_id: String,
    self_team: String,
}

/// What the engine must do after a GSI update.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Effects {
    /// A new match: per-match caches and the console log position start over.
    pub reset: bool,
    /// A match is now running, so the Steam roster should be polled.
    pub started: bool,
    /// The match ended (gameover, the menu, a disconnect); its players stay until the next one.
    pub ended: bool,
}

pub struct MatchState {
    lifecycle: MatchLifecycle,
    current: Current,
    selected: Option<Vec<Candidate>>,
    excluded: HashSet<String>,
    spectated: Vec<Candidate>,
    pub console_players: Vec<Candidate>,
    /// The players shown belong to a match that has ended.
    finished: bool,
    /// Leetify matches needed before tendencies and mechanics comparisons (from Settings).
    pub min_sample_matches: f64,
    pub players: PlayerData,
    pub roster: RosterState,
    pub history: MatchHistoryStore,
}

/// Values worth keeping per player when a match ends, with the time each provider measured them. Leetify
/// values are never stored (Leetify's developer guidelines).
const SNAPSHOT_METRICS: [(&str, &str); 13] = [
    ("steam", "hoursCs2"),
    ("steam", "createdAt"),
    ("steam", "vacBans"),
    ("steam", "gameBans"),
    ("csrep", "trust"),
    ("csstats", "kd"),
    ("csstats", "hs"),
    ("csstats", "winRate"),
    ("faceit", "elo"),
    ("faceit", "level"),
    ("faceit", "matches"),
    ("faceit", "kd"),
    ("faceit", "hs"),
];

fn snapshots(player: &Player) -> Vec<Snapshot> {
    SNAPSHOT_METRICS
        .iter()
        .filter_map(|(provider, metric)| {
            let result = player.provider(provider).filter(|r| r.is_ok())?;
            Some(Snapshot { provider: provider.to_string(), metric: metric.to_string(), value: result.number(metric)?, measured_at: result.fetched_at? })
        })
        .collect()
}

fn snapshot(g: &GsiSummary) -> lifecycle::Snapshot {
    lifecycle::Snapshot { connected: g.connected, map: g.map.clone(), mode: g.mode.clone(), phase: g.phase.clone(), round: g.round }
}

impl MatchState {
    pub fn new(history: MatchHistoryStore) -> Self {
        Self {
            lifecycle: MatchLifecycle::default(),
            current: Current::default(),
            selected: None,
            excluded: HashSet::new(),
            spectated: Vec::new(),
            console_players: Vec::new(),
            finished: false,
            min_sample_matches: 30.0,
            players: PlayerData::default(),
            roster: RosterState::default(),
            history,
        }
    }

    pub fn active(&self) -> bool {
        self.lifecycle.active
    }

    pub fn mode(&self, g: &GsiSummary) -> ModeInfo {
        mode_info(if self.current.mode.is_empty() { &g.mode } else { &self.current.mode })
    }

    /// Called for every GSI state and disconnect.
    pub fn update(&mut self, g: &GsiSummary) -> Effects {
        let transition = self.lifecycle.update(&snapshot(g));
        if transition.ended {
            self.record_finished(g);
            self.roster.end_match();
            self.finished = true;
        }
        // The last match's players stay visible (scoreboard, menu, after CS2 closes) until the next starts.
        if transition.started {
            self.finished = false;
            self.current = Current { started_at: crate::model::now_ms(), ..Current::default() };
            self.spectated.clear();
            self.selected = None;
            self.excluded.clear();
            self.players.reset();
            self.roster.last_roster.clear();
            self.console_players.clear();
        }
        if self.lifecycle.active {
            let keep = |value: &str, current: &mut String| {
                if !value.is_empty() {
                    *current = value.to_string();
                }
            };
            keep(&g.map, &mut self.current.map);
            keep(&g.mode, &mut self.current.mode);
            keep(&g.self_steam_id, &mut self.current.self_id);
            keep(&g.self_team, &mut self.current.self_team);
        }
        self.roster.self_id = g.self_steam_id.clone();
        self.roster.self_name = g.self_name.clone();
        self.roster.self_team = g.self_team.clone();
        // A first sighting past warmup means the app started (or restarted) mid-match.
        if transition.started {
            self.roster.begin_match(g.phase != "warmup", &mode_info(&g.mode));
        }
        self.merge(g);
        self.note_spectated(g);
        Effects { reset: transition.started, started: transition.started, ended: transition.ended }
    }

    /// Rebuilds the player list from every source; called whenever one of them changes.
    pub fn merge(&mut self, g: &GsiSummary) {
        if !self.lifecycle.active && self.selected.is_none() {
            return;
        }
        let mut sources: Vec<Candidate> = Vec::new();
        if self.selected.is_none() {
            sources.extend(self.roster.last_roster.iter().cloned());
            sources.extend(self.console_players.iter().cloned());
        }
        match &self.selected {
            Some(selected) => sources.extend(selected.iter().cloned()),
            None => sources.extend(self.players.list().into_iter().filter(|p| p.source == "manual").map(|p| Candidate {
                steam_id: p.steam_id,
                name: p.name,
                source: p.source,
                confidence: p.confidence,
                seen_at: Some(p.seen_at),
                ..Candidate::default()
            })),
        }
        sources.extend(self.spectated.iter().cloned());
        if self.lifecycle.active && !g.self_steam_id.is_empty() {
            // No seen time: the first confirmation time is kept instead of changing on every GSI post.
            sources.push(Candidate {
                steam_id: g.self_steam_id.clone(),
                name: g.self_name.clone(),
                team: g.self_team.clone(),
                is_self: true,
                source: "gsi".into(),
                confidence: "confirmed".into(),
                ..Candidate::default()
            });
        }
        // Later sources win for the same player; the first position is kept.
        let mut merged: Vec<Candidate> = Vec::new();
        for candidate in sources.into_iter().filter(|c| is_steam_id(&c.steam_id)) {
            match merged.iter_mut().find(|m| m.steam_id == candidate.steam_id) {
                Some(slot) => *slot = candidate,
                None => merged.push(candidate),
            }
        }
        merged.sort_by_key(|c| !c.is_self);
        merged.retain(|c| c.is_self || !self.excluded.contains(&c.steam_id));
        self.players.seed_roster(&merged, true);
    }

    /// Competitive, Premier and Wingman only let dead players spectate their own team, so the player GSI
    /// reports while you spectate, on your side, is a confirmed teammate.
    fn note_spectated(&mut self, g: &GsiSummary) {
        let Some(seen) = &g.observed else { return };
        if !self.lifecycle.active || !is_steam_id(&seen.steam_id) || !team_locked(&g.mode) {
            return;
        }
        if self.current.self_team.is_empty() || seen.team != self.current.self_team || self.excluded.contains(&seen.steam_id) {
            return;
        }
        if !self.spectated.iter().any(|c| c.steam_id == seen.steam_id) {
            let name = if seen.name.is_empty() { seen.steam_id.clone() } else { seen.name.clone() };
            self.spectated.push(Candidate {
                steam_id: seen.steam_id.clone(),
                name,
                source: "gsi-spectated".into(),
                confidence: "confirmed".into(),
                ..Candidate::default()
            });
            self.merge(g);
        }
        self.players.confirm_teammate(&seen.steam_id);
    }

    fn record_finished(&mut self, g: &GsiSummary) {
        let players = self
            .players
            .list()
            .into_iter()
            .filter(|p| !p.is_self)
            .map(|p| HistoryPlayer { snapshots: snapshots(&p), steam_id: p.steam_id, side: p.side, name: p.name })
            .collect();
        let result = match_result(g, &self.current.self_team);
        let started_at = Some(self.current.started_at).filter(|t| *t > 0);
        // Your own line from CS2 at the end of the match (kept from your last report while spectating).
        let own = crate::match_history::OwnMatchStats { kills: g.kills, deaths: g.deaths, assists: g.assists, mvps: g.mvps, score: g.score };
        self.history.record_match(started_at, &own, &self.current.map, &self.current.mode, result, &self.current.self_id, players);
    }

    /// Applies likely sides from party links; never touches sides that are certain.
    pub fn infer_teams(&mut self, g: &GsiSummary) -> bool {
        if !self.mode(g).teams {
            return false;
        }
        let players = self.players.list();
        let inferred = infer_sides(&players);
        let mut changed = false;
        for player in players.iter().filter(|p| !p.is_self) {
            changed |= match inferred.get(&player.steam_id) {
                Some(result) => self.players.set_inferred_side(&player.steam_id, &result.side, &result.reason),
                None if player.side_source == SideSource::Likely => self.players.set_inferred_side(&player.steam_id, "", ""),
                None => false,
            };
        }
        changed
    }

    pub fn select_players(&mut self, input: &str, g: &GsiSummary) -> Result<usize, String> {
        let players = parse_roster_input(input, &g.self_steam_id, self.mode(g).max_players.saturating_sub(1))?;
        let count = players.len();
        self.selected = Some(players);
        self.excluded.clear();
        self.merge(g);
        Ok(count)
    }

    pub fn add_player(&mut self, input: &str, g: &GsiSummary) -> Result<(), String> {
        match &self.selected {
            Some(selected) => {
                let list: Vec<String> = selected.iter().map(|p| p.steam_id.clone()).chain([input.to_string()]).collect();
                self.selected = Some(parse_roster_input(&list.join("\n"), &g.self_steam_id, self.mode(g).max_players.saturating_sub(1))?);
                self.excluded.clear();
                self.merge(g);
            }
            None => {
                let id = self.players.add_manual(input)?;
                self.excluded.remove(&id);
            }
        }
        Ok(())
    }

    pub fn use_automatic(&mut self, g: &GsiSummary) {
        self.selected = None;
        self.excluded.clear();
        let automatic: Vec<Candidate> = self
            .players
            .list()
            .into_iter()
            .filter(|p| p.source != "manual")
            .map(|p| Candidate {
                steam_id: p.steam_id,
                name: p.name,
                is_self: p.is_self,
                team: p.team,
                source: p.source,
                confidence: p.confidence,
                seen_at: Some(p.seen_at),
                party: p.party,
            })
            .collect();
        self.players.seed_roster(&automatic, true);
        self.merge(g);
    }

    pub fn remove_player(&mut self, steam_id: &str, g: &GsiSummary) -> Result<(), String> {
        if steam_id == g.self_steam_id {
            return Err("Your own GSI player cannot be removed.".into());
        }
        self.excluded.insert(steam_id.to_string());
        self.spectated.retain(|c| c.steam_id != steam_id);
        if let Some(selected) = &mut self.selected {
            selected.retain(|c| c.steam_id != steam_id);
        }
        self.players.remove(steam_id);
        Ok(())
    }

    /// One status for the UI: whether CS2 is reporting, and whether that report is a match in progress.
    pub fn status(&self, g: &GsiSummary) -> Value {
        if !g.connected {
            return json!({ "state": "offline", "label": if g.error.is_empty() { "Waiting for CS2" } else { "GSI problem" } });
        }
        if !self.lifecycle.active {
            let over = g.phase == "gameover";
            return json!({ "state": if over { "over" } else { "idle" }, "label": if over { "Match over" } else { "Not in a match" } });
        }
        let label = match g.phase.as_str() {
            "warmup" => "Warmup",
            "intermission" => "Half-time",
            _ => "Live",
        };
        let mode = self.mode(g);
        json!({
            "state": if g.phase == "warmup" { "warmup" } else { "live" },
            "label": label,
            "side": if mode.teams { self.current.self_team.as_str() } else { "" },
            "mode": mode.label,
            "maxPlayers": mode.max_players,
            "teams": mode.teams,
            "teamSize": mode.team_size,
        })
    }

    /// Where a note written now about `steam_id` belongs: this match (running, or the one just finished),
    /// its map and mode, and the player's side.
    pub fn note_context(&self, steam_id: &str) -> crate::match_history::NoteContext {
        let side = self.players.get(steam_id).map(|p| p.side.clone()).unwrap_or_default();
        crate::match_history::NoteContext {
            map: self.current.map.clone(),
            mode: self.current.mode.clone(),
            side,
            match_started_at: Some(self.current.started_at).filter(|t| *t > 0),
            match_id: None,
        }
    }

    /// The match part of the UI state, with this app's played-before records attached to each player.
    pub fn view(&mut self, g: &GsiSummary) -> Value {
        self.infer_teams(g);
        let self_id = if g.self_steam_id.is_empty() { self.current.self_id.clone() } else { g.self_steam_id.clone() };
        let players = self.players.list();
        let prediction = predict(&players, &self.mode(g));
        let partners = frequent_partners(&players);
        let analyses = analysis::analyze_all(&players, analysis::Options { min_matches: self.min_sample_matches, now_ms: crate::model::now_ms() });
        let strongest = analysis::strongest(&players);
        let rows: Vec<Value> = players
            .iter()
            .map(|p| {
                let mut row = serde_json::to_value(p).unwrap_or(Value::Null);
                if !p.is_self {
                    row["analysis"] = serde_json::to_value(&analyses[&p.steam_id]).unwrap_or(Value::Null);
                }
                row["strongest"] = (strongest.as_deref() == Some(p.steam_id.as_str())).into();
                // Your notes on this player, newest first: the latest few travel with the match view.
                let notes = self.history.notes_for(&p.steam_id);
                row["hasNote"] = (!notes.is_empty()).into();
                row["notes"] = Value::Array(notes.into_iter().take(5).collect());
                if let Some(list) = partners.get(&p.steam_id) {
                    row["partners"] = serde_json::to_value(list).unwrap_or(Value::Null);
                }
                if !p.is_self {
                    row["localHistory"] = serde_json::to_value(self.history.summary_for(&self_id, &p.steam_id)).unwrap_or(Value::Null);
                }
                row
            })
            .collect();
        json!({
            "match": self.status(g),
            "players": rows,
            "prediction": prediction,
            "rosterSelection": if self.selected.is_some() { "selected" } else { "automatic" },
            "lastMatch": if self.finished && !self.lifecycle.active && !players.is_empty() {
                json!({ "map": self.current.map, "mode": mode_info(&self.current.mode).label })
            } else {
                Value::Null
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gsi::Observed;

    const ME: &str = "76561198000000000";

    fn id(n: u64) -> String {
        (76561198000000000 + n).to_string()
    }

    fn live(phase: &str) -> GsiSummary {
        GsiSummary {
            connected: true,
            map: "de_mirage".into(),
            mode: "competitive".into(),
            phase: phase.into(),
            round: Some(1),
            self_steam_id: ME.into(),
            self_name: "Me".into(),
            self_team: "CT".into(),
            ..GsiSummary::default()
        }
    }

    fn state() -> (MatchState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (MatchState::new(MatchHistoryStore::open(dir.path())), dir)
    }

    fn coplay(n: u64) -> Candidate {
        Candidate { steam_id: id(n), name: format!("p{n}"), source: "steam-coplay".into(), confidence: "candidate".into(), ..Candidate::default() }
    }

    #[test]
    fn a_match_collects_players_from_every_source() {
        let (mut m, _dir) = state();
        let g = live("warmup");
        assert_eq!(m.update(&g), Effects { reset: true, started: true, ended: false });
        assert!(!m.roster.joined_late);
        m.roster.last_roster = vec![coplay(1), coplay(2)];
        m.console_players = vec![Candidate { source: "console-status".into(), ..coplay(3) }];
        m.merge(&g);
        assert_eq!(m.players.list().len(), 4);
        assert!(m.players.list()[0].is_self);
        m.remove_player(&id(2), &g).unwrap();
        m.merge(&g);
        assert!(m.players.get(&id(2)).is_none(), "removed players stay removed");
        assert!(m.remove_player(ME, &g).is_err());
        let view = m.view(&g);
        assert_eq!(view["match"]["label"], "Warmup");
        assert_eq!(view["rosterSelection"], "automatic");
        assert!(view["players"][1]["localHistory"]["all"].is_object());
    }

    #[test]
    fn selection_replaces_discovery_until_automatic_is_restored() {
        let (mut m, _dir) = state();
        let g = live("live");
        m.update(&g);
        assert!(m.roster.joined_late);
        m.roster.last_roster = vec![coplay(1)];
        m.merge(&g);
        assert_eq!(m.select_players(&format!("{}\n{}", id(5), id(6)), &g).unwrap(), 2);
        assert!(m.players.get(&id(1)).is_none());
        m.add_player(&id(7), &g).unwrap();
        assert!(m.players.get(&id(7)).is_some());
        m.use_automatic(&g);
        assert!(m.players.get(&id(5)).is_none() && m.players.get(&id(1)).is_some());
        assert!(m.select_players(&(1..=10).map(id).collect::<Vec<_>>().join("\n"), &g).is_err(), "competitive allows nine others");
    }

    #[test]
    fn spectating_confirms_teammates_in_team_locked_modes() {
        let (mut m, _dir) = state();
        let mut g = live("live");
        m.update(&g);
        g.observed = Some(Observed { steam_id: id(4), name: "Mate".into(), team: "CT".into() });
        m.update(&g);
        let mate = m.players.get(&id(4)).unwrap();
        assert_eq!((mate.side.as_str(), mate.side_source), ("team", SideSource::Spectated));
        g.observed = Some(Observed { steam_id: id(8), name: "Enemy".into(), team: "T".into() });
        m.update(&g);
        assert!(m.players.get(&id(8)).is_none(), "the other team is never marked as yours");
    }

    #[test]
    fn finished_matches_are_recorded_once() {
        let (mut m, _dir) = state();
        m.update(&live("live"));
        m.roster.last_roster = vec![coplay(1)];
        m.merge(&live("live"));
        m.players.set_side(&id(1), "team").unwrap();
        let mut over = live("gameover");
        over.score_ct = Some(13);
        over.score_t = Some(7);
        m.update(&over);
        assert!(!m.active());
        let summary = m.history.summary_for(ME, &id(1));
        assert_eq!((summary.together.played, summary.together.won), (1, 1));
        assert_eq!(m.status(&over)["label"], "Match over");
        assert!(m.players.get(&id(1)).is_some(), "the last match's players stay after it ends");
        assert_eq!(m.view(&over)["lastMatch"]["map"], "de_mirage");
        m.update(&GsiSummary::default());
        assert_eq!(m.history.summary_for(ME, &id(1)).all.played, 1);
        assert!(m.players.get(&id(1)).is_some(), "and after CS2 disconnects");
        let next = m.update(&live("warmup"));
        assert!(next.started && next.reset);
        assert!(m.players.get(&id(1)).is_none(), "the next match starts empty");
        assert!(m.view(&live("warmup"))["lastMatch"].is_null());
        assert_eq!(m.status(&GsiSummary::default())["state"], "offline");
    }

    #[test]
    fn deathmatch_has_no_teams() {
        let (mut m, _dir) = state();
        let mut g = live("live");
        g.mode = "deathmatch".into();
        m.update(&g);
        assert_eq!(m.roster.max_players, 16);
        let view = m.view(&g);
        assert_eq!(view["match"]["teams"], false);
        assert_eq!(view["match"]["side"], "");
        assert_eq!(view["prediction"]["status"], "ffa");
    }
}
