//! Recent-player candidates from Steam. Timestamp groups narrow the search but do not prove current
//! match membership; only GSI or the user's own selection can provide stronger evidence.

use crate::model::{is_steam_id, now_ms, now_secs, Candidate, Party};
use crate::modes::ModeInfo;
use crate::steam::{CoplayEntry, FriendInGame};

pub const LATE_JOIN_LOOKBACK_SECONDS: i64 = 90 * 60;
const GROUP_SPREAD: i64 = 30;
const GROUP_MIN_SIZE: usize = 3;

/// When the app first sees a match that is already under way, the newest group of at least three
/// entries within 30 seconds of each other marks the current match. Returns its start, or 0.
pub fn latest_match_group_start(entries: &[CoplayEntry], now: i64) -> i64 {
    let mut times: Vec<i64> = entries
        .iter()
        .filter(|e| e.error.is_none() && e.game_id == 730 && e.time >= now - LATE_JOIN_LOOKBACK_SECONDS && e.time <= now + 60)
        .map(|e| e.time)
        .collect();
    times.sort_unstable_by(|a, b| b.cmp(a));
    times
        .iter()
        .find(|&&time| times.iter().filter(|&&other| (other - time).abs() <= GROUP_SPREAD).count() >= GROUP_MIN_SIZE)
        .map(|time| time - GROUP_SPREAD)
        .unwrap_or(0)
}

/// What the roster knows about the current match.
#[derive(Clone, Debug)]
pub struct RosterState {
    pub self_id: String,
    pub self_name: String,
    pub self_team: String,
    /// Unix seconds when this app first saw the match; 0 outside a match.
    pub match_started_at: i64,
    /// The first GSI state of this match was already past warmup, so the app's own start time says
    /// nothing about when the match began.
    pub joined_late: bool,
    /// Most players a lobby can hold (deathmatch and casual lobbies are larger than ten).
    pub max_players: usize,
    /// Players join throughout the match (Deathmatch, Casual), so there is no shared start time.
    pub drop_in: bool,
    /// How far back a late join looks for this match's players.
    pub late_lookback: i64,
    /// Seconds before the first sighting in which this match's players may have been recorded.
    pub pre_start: i64,
    pub last_roster: Vec<Candidate>,
}

impl Default for RosterState {
    fn default() -> Self {
        Self {
            self_id: String::new(),
            self_name: String::new(),
            self_team: String::new(),
            match_started_at: 0,
            joined_late: false,
            max_players: 10,
            drop_in: false,
            late_lookback: LATE_JOIN_LOOKBACK_SECONDS,
            pre_start: 120,
            last_roster: Vec::new(),
        }
    }
}

impl RosterState {
    pub fn begin_match(&mut self, joined_late: bool, mode: &ModeInfo) {
        self.max_players = mode.max_players;
        self.drop_in = mode.drop_in;
        self.late_lookback = mode.late_lookback_minutes * 60;
        self.pre_start = mode.pre_start_minutes * 60;
        if self.match_started_at == 0 {
            self.match_started_at = now_secs();
            self.joined_late = joined_late;
        }
    }

    pub fn end_match(&mut self) {
        self.match_started_at = 0;
        self.joined_late = false;
    }

    fn late(&self) -> bool {
        self.match_started_at > 0 && self.joined_late
    }

    fn strict_start(&self, now: i64) -> i64 {
        if self.match_started_at > 0 {
            self.match_started_at - self.pre_start
        } else {
            now - 900
        }
    }

    /// The earliest co-play time worth asking Steam about.
    pub fn window_start(&self, now: i64) -> i64 {
        if self.drop_in {
            // Drop-in lists are a rolling set of candidates, not a permanent session roster.
            let recent = now - self.late_lookback;
            if self.late() {
                recent
            } else {
                self.strict_start(now).max(recent)
            }
        } else if self.late() {
            now - self.late_lookback
        } else {
            self.strict_start(now)
        }
    }

    /// When CS2 co-play entries were recorded relative to the first sighting of this match, in minutes
    /// with counts (for example "-12m×1, -3m×4, +0m×5"), for diagnosing a short list.
    pub fn timing(&self, entries: &[CoplayEntry], now: i64) -> String {
        let start = if self.match_started_at > 0 { self.match_started_at } else { now };
        let mut minutes: Vec<i64> = entries
            .iter()
            .filter(|e| e.error.is_none() && e.game_id == 730 && e.time >= start - 3600 && e.time <= now + 60)
            .map(|e| (e.time - start).div_euclid(60))
            .collect();
        minutes.sort_unstable();
        let mut parts: Vec<String> = Vec::new();
        for group in minutes.chunk_by(|a, b| a == b) {
            parts.push(format!("{}{}m×{}", if group[0] >= 0 { "+" } else { "" }, group[0], group.len()));
        }
        if parts.is_empty() {
            "none in the last hour".into()
        } else {
            parts.join(", ")
        }
    }

    /// Builds the roster from a snapshot; returns it with any per-entry errors to log.
    pub fn apply_snapshot(&mut self, entries: &[CoplayEntry], friends: &[FriendInGame], now: i64) -> Vec<String> {
        let window_start = self.window_start(now);
        // Seen from warmup, the match start is known: every entry since then belongs to this match (CS2
        // records players at different moments, so no shared timestamp is required). Drop-in modes work the
        // same way. Only when the app arrived mid-match is the start guessed: re-queueing quickly leaves the
        // previous match's group in the look-back window, so the newest group of shared timestamps (plus
        // anyone recorded after it, such as replacements) is taken as the current match.
        let in_window: Vec<CoplayEntry> = entries.iter().filter(|e| e.time >= window_start).cloned().collect();
        let guessed = self.late() && !self.drop_in;
        let group_start = if guessed { latest_match_group_start(&in_window, now) } else { 0 };
        let minimum_time = if !guessed {
            window_start
        } else if group_start > 0 {
            window_start.max(group_start)
        } else {
            self.strict_start(now)
        };
        let mut errors = Vec::new();
        let mut recent: Vec<&CoplayEntry> = Vec::new();
        for entry in entries {
            if let Some(error) = &entry.error {
                errors.push(error.clone());
                continue;
            }
            if !is_steam_id(&entry.steam_id) || entry.steam_id == self.self_id {
                continue;
            }
            if entry.game_id != 730 || entry.time == 0 || entry.time > now + 60 || entry.time < minimum_time {
                continue;
            }
            recent.push(entry);
        }
        recent.sort_by_key(|e| std::cmp::Reverse(e.time));
        let mut chosen: Vec<Candidate> = Vec::new();
        if !self.self_id.is_empty() {
            chosen.push(Candidate {
                steam_id: self.self_id.clone(),
                name: self.self_name.clone(),
                team: self.self_team.clone(),
                is_self: true,
                source: "gsi".into(),
                confidence: "confirmed".into(),
                seen_at: Some(now_ms()),
                party: None,
            });
        }
        for entry in recent {
            if chosen.len() >= self.max_players {
                break;
            }
            if chosen.iter().any(|c| c.steam_id == entry.steam_id) {
                continue;
            }
            // Steam friends in this match who are in a party (lobby): party members always share a team.
            let party = friends.iter().find(|f| f.steam_id == entry.steam_id).map(|f| Party { friend: true, lobby: f.lobby.clone(), server: f.server.clone() });
            chosen.push(Candidate {
                steam_id: entry.steam_id.clone(),
                name: if entry.name.is_empty() { entry.steam_id.clone() } else { entry.name.clone() },
                source: "steam-coplay".into(),
                confidence: "candidate".into(),
                seen_at: Some(entry.time * 1000),
                party,
                ..Candidate::default()
            });
        }
        // With a known start, a player found once stays for the rest of the match even if Steam's list
        // later drops or reorders them.
        if !guessed && !self.drop_in {
            for previous in &self.last_roster {
                if chosen.len() >= self.max_players {
                    break;
                }
                if !previous.is_self && !chosen.iter().any(|c| c.steam_id == previous.steam_id) {
                    chosen.push(previous.clone());
                }
            }
        }
        self.last_roster = chosen;
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::mode_info;

    fn entry(id: u64, time: i64) -> CoplayEntry {
        CoplayEntry { steam_id: (76561198000000000 + id).to_string(), time, game_id: 730, name: format!("p{id}"), error: None }
    }

    #[test]
    fn finds_the_newest_group_of_shared_timestamps() {
        let now = 100_000;
        let previous: Vec<_> = (1..=9).map(|i| entry(i, now - 3000 + i as i64)).collect();
        let current: Vec<_> = (10..=12).map(|i| entry(i, now - 600 + i as i64)).collect();
        let all: Vec<_> = previous.iter().chain(&current).cloned().collect();
        assert_eq!(latest_match_group_start(&all, now), now - 600 + 12 - GROUP_SPREAD);
        assert_eq!(latest_match_group_start(&all[..2], now), 0, "two entries are not a match");
    }

    #[test]
    fn late_joins_keep_only_the_current_match() {
        let now = now_secs();
        let mut roster = RosterState { self_id: "76561198000000000".into(), ..RosterState::default() };
        roster.begin_match(true, &mode_info("competitive"));
        let mut entries: Vec<_> = (1..=9).map(|i| entry(i, now - 3000)).collect();
        entries.extend((10..=18).map(|i| entry(i, now - 300)));
        entries.push(CoplayEntry { error: Some("bad".into()), ..CoplayEntry::default() });
        let friends = [FriendInGame { steam_id: entry(10, 0).steam_id, lobby: "42".into(), server: String::new() }];
        let errors = roster.apply_snapshot(&entries, &friends, now);
        assert_eq!(errors, ["bad"]);
        assert_eq!(roster.last_roster.len(), 10);
        assert!(roster.last_roster[0].is_self);
        assert!(roster.last_roster[1..].iter().all(|c| c.seen_at == Some((now - 300) * 1000)), "the previous match is excluded");
        let partied = roster.last_roster.iter().find(|c| c.steam_id == entry(10, 0).steam_id).unwrap();
        assert_eq!(partied.party.as_ref().unwrap().lobby, "42");
    }

    #[test]
    fn drop_in_modes_keep_players_who_joined_at_different_times() {
        let now = now_secs();
        let mut roster = RosterState::default();
        roster.begin_match(true, &mode_info("deathmatch"));
        // Fifteen opponents who joined one by one over ten minutes, plus one from an hour ago.
        let mut entries: Vec<_> = (1..=15).map(|i| entry(i, now - 600 + i as i64 * 40)).collect();
        entries.push(entry(40, now - 3600));
        roster.apply_snapshot(&entries, &[], now);
        assert_eq!(roster.last_roster.len(), 15, "everyone from this match, nobody from an hour ago");
        let mut competitive = RosterState::default();
        competitive.begin_match(true, &mode_info("competitive"));
        competitive.apply_snapshot(&entries, &[], now);
        assert!(competitive.last_roster.len() < 15, "fixed lobbies still use the shared start time");
    }

    #[test]
    fn matches_seen_from_warmup_keep_everyone_recorded_since_the_start() {
        let now = now_secs();
        let mut roster = RosterState { self_id: "76561198000000000".into(), ..RosterState::default() };
        roster.begin_match(false, &mode_info("competitive"));
        roster.match_started_at = now - 300;
        // Nine opponents recorded over three minutes, newest three close together, one from the last match.
        let mut entries: Vec<_> = (1..=6).map(|i| entry(i, now - 400 + i as i64 * 25)).collect();
        entries.extend((7..=9).map(|i| entry(i, now - 60)));
        entries.push(entry(30, now - 3000));
        roster.apply_snapshot(&entries, &[], now);
        assert_eq!(roster.last_roster.len(), 10, "you and all nine, without the previous match");
        // Steam's list later loses some of them: they stay.
        roster.apply_snapshot(&entries[6..9], &[], now);
        assert_eq!(roster.last_roster.len(), 10);
    }

    #[test]
    fn competitive_counts_players_recorded_while_the_match_loaded() {
        let now = now_secs();
        let mut roster = RosterState::default();
        roster.begin_match(false, &mode_info("competitive"));
        roster.match_started_at = now - 60;
        // Recorded at accept, four minutes before the app first saw warmup; one from a match 40 minutes ago.
        let mut entries: Vec<_> = (1..=9).map(|i| entry(i, now - 300)).collect();
        entries.push(entry(20, now - 2400));
        roster.apply_snapshot(&entries, &[], now);
        assert_eq!(roster.last_roster.len(), 9, "all nine others; you are added once GSI names you");
        assert_eq!(roster.timing(&entries, now), "-39m×1, -4m×9", "the old match shows up in the timing, not in the list");
    }

    #[test]
    fn deathmatch_lobbies_hold_more_than_ten() {
        let now = now_secs();
        let mut roster = RosterState::default();
        roster.begin_match(false, &mode_info("deathmatch"));
        let entries: Vec<_> = (1..=20).map(|i| entry(i, now - 10)).collect();
        roster.apply_snapshot(&entries, &[], now);
        assert_eq!(roster.last_roster.len(), 16);
    }

    #[test]
    fn drop_in_candidates_expire_and_missing_entries_are_not_restored() {
        let now = now_secs();
        let mut roster = RosterState::default();
        roster.begin_match(false, &mode_info("deathmatch"));
        roster.match_started_at = now - 3600;
        let entries = [entry(1, now - 10), entry(2, now - 901)];
        roster.apply_snapshot(&entries, &[], now);
        assert_eq!(roster.last_roster.len(), 1, "timestamps outside the rolling window expire");
        roster.apply_snapshot(&[], &[], now + 1);
        assert!(roster.last_roster.is_empty(), "a missing entry is not permanently retained");
        roster.apply_snapshot(&entries, &[], now + 901);
        assert!(roster.last_roster.is_empty(), "unchanged coplay timestamps eventually expire");
    }
}
