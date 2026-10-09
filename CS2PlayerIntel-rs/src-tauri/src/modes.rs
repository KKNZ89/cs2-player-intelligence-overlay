//! Lobby and team sizes per CS2 game mode (GSI `map.mode`). Free-for-all modes have no teams, so they
//! get no side grouping or win estimate. Unknown modes allow a full casual lobby with no team-size rules.
//!
//! In fixed-lobby modes (Premier, Competitive, Wingman, Rush) everyone is in the match from the start. In
//! drop-in modes (Deathmatch, Casual, Arms Race and unknown modes) players join and leave throughout.

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeInfo {
    pub label: String,
    pub max_players: usize,
    pub team_size: usize,
    pub teams: bool,
    /// Players join and leave during the match, so they are not all recorded when it starts.
    pub drop_in: bool,
    /// How far back to look for players when the app first sees a match already under way.
    pub late_lookback_minutes: i64,
    /// How long before the app first sees warmup players may have been recorded: matchmaking accept,
    /// loading and the part of warmup the app missed. Kept shorter than a match of this mode, so the
    /// previous match's players stay out.
    pub pre_start_minutes: i64,
}

pub fn mode_info(mode: &str) -> ModeInfo {
    let fixed = |label: &str, max_players, team_size, pre_start_minutes| ModeInfo {
        label: label.into(),
        max_players,
        team_size,
        teams: true,
        drop_in: false,
        late_lookback_minutes: 90,
        pre_start_minutes,
    };
    let drop_in = |label: &str, max_players, team_size, teams, late_lookback_minutes| ModeInfo {
        label: label.into(),
        max_players,
        team_size,
        teams,
        drop_in: true,
        late_lookback_minutes,
        pre_start_minutes: 2,
    };
    match mode.to_ascii_lowercase().as_str() {
        "competitive" => fixed("Competitive", 10, 5, 10),
        "premier" => fixed("Premier", 10, 5, 10),
        "scrimcomp2v2" | "wingman" => fixed("Wingman", 4, 2, 5),
        // Queued 3v3 across a series of arenas (added September 2026; GSI maps such as rush_001).
        "rush" => fixed("Rush", 6, 3, 5),
        "casual" => drop_in("Casual", 20, 10, true, 45),
        "deathmatch" => drop_in("Deathmatch", 16, 0, false, 15),
        "gungameprogressive" | "armsrace" => drop_in("Arms Race", 10, 0, false, 15),
        _ => {
            let mut chars = mode.chars();
            let label: String = chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default();
            drop_in(&label, 20, 0, true, 30)
        }
    }
}

/// Modes where dead players can only spectate their own team, so spectating confirms a teammate.
pub fn team_locked(mode: &str) -> bool {
    matches!(mode, "competitive" | "premier" | "scrimcomp2v2")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_set_lobby_and_team_sizes() {
        assert_eq!((mode_info("competitive").max_players, mode_info("competitive").team_size), (10, 5));
        assert_eq!((mode_info("scrimcomp2v2").max_players, mode_info("scrimcomp2v2").team_size), (4, 2));
        assert_eq!(mode_info("casual").max_players, 20);
        assert_eq!(mode_info("deathmatch").max_players, 16);
        assert!(!mode_info("deathmatch").teams);
        assert!(mode_info("deathmatch").drop_in && mode_info("casual").drop_in && !mode_info("premier").drop_in);
        let rush = mode_info("rush");
        assert_eq!((rush.label.as_str(), rush.max_players, rush.team_size, rush.teams, rush.drop_in), ("Rush", 6, 3, true, false));
        let unknown = mode_info("somethingnew");
        assert_eq!((unknown.max_players, unknown.label.as_str()), (20, "Somethingnew"));
        assert!(team_locked("premier") && !team_locked("casual"));
    }
}
