//! Rough win chance from average Premier rating (as reported through Leetify) of your team versus the
//! opponents. It is this app's own unvalidated estimate, not a Leetify metric: an Elo-style curve where a
//! 1,000-point average advantage is about 64%. Free-for-all modes have no sides to compare.

use crate::model::Player;
use crate::modes::ModeInfo;
use serde::Serialize;

const POINTS_PER_TENFOLD: f64 = 4000.0;
const MIN_RATED_PER_TEAM: usize = 2;

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Prediction {
    pub status: &'static str,
    pub team_size: usize,
    pub enemy_size: usize,
    pub inferred_enemies: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_rated: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enemy_rated: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_average: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enemy_average: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probability: Option<f64>,
}

pub fn win_probability(team_average: f64, enemy_average: f64) -> f64 {
    1.0 / (1.0 + 10f64.powf(-(team_average - enemy_average) / POINTS_PER_TENFOLD))
}

pub fn predict(players: &[Player], mode: &ModeInfo) -> Prediction {
    if !mode.teams {
        return Prediction { status: "ffa", ..Prediction::default() };
    }
    let team: Vec<&Player> = players.iter().filter(|p| p.is_self || p.side == "team").collect();
    let mut enemy: Vec<&Player> = players.iter().filter(|p| !p.is_self && p.side == "enemy").collect();
    let unassigned: Vec<&Player> = players.iter().filter(|p| !p.is_self && p.side.is_empty()).collect();
    // With your whole team known, the rest are opponents, but only when exactly the missing opponents
    // remain; extra unassigned players mean the list holds someone not in this match.
    let size = mode.team_size;
    let inferred: Vec<&Player> = if size > 0 && team.len() == size && enemy.len() + unassigned.len() == size { unassigned } else { Vec::new() };
    enemy.extend(inferred.iter().copied());
    let mut result = Prediction {
        status: "needs-teams",
        team_size: team.len(),
        enemy_size: enemy.len(),
        inferred_enemies: inferred.iter().map(|p| p.steam_id.clone()).collect(),
        ..Prediction::default()
    };
    if enemy.is_empty() || team.len() < 2 {
        return result;
    }
    let team_ratings: Vec<f64> = team.iter().filter_map(|p| p.premier()).collect();
    let enemy_ratings: Vec<f64> = enemy.iter().filter_map(|p| p.premier()).collect();
    result.team_rated = Some(team_ratings.len());
    result.enemy_rated = Some(enemy_ratings.len());
    let min_rated = MIN_RATED_PER_TEAM.min(if size > 0 { size } else { MIN_RATED_PER_TEAM });
    if team_ratings.len() < min_rated || enemy_ratings.len() < min_rated {
        result.status = "not-enough-data";
        return result;
    }
    let average = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;
    let (team_average, enemy_average) = (average(&team_ratings), average(&enemy_ratings));
    result.status = "ok";
    result.team_average = Some(team_average);
    result.enemy_average = Some(enemy_average);
    result.probability = Some(win_probability(team_average, enemy_average));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderResult;
    use crate::modes::mode_info;

    pub fn rated(id: &str, premier: f64, side: &str, is_self: bool) -> Player {
        let mut data = serde_json::Map::new();
        data.insert("premier".into(), premier.into());
        let mut player = Player { steam_id: id.into(), side: side.into(), is_self, ..Player::default() };
        player.providers.insert("leetify".into(), ProviderResult::ok(data));
        player
    }

    #[test]
    fn needs_teams_and_ratings() {
        let mode = mode_info("competitive");
        assert_eq!(predict(&[rated("0", 15000.0, "team", true), rated("1", 15000.0, "", false)], &mode).status, "needs-teams");
        let sparse = [rated("0", 15000.0, "team", true), rated("1", 15000.0, "team", false), rated("2", 0.0, "enemy", false)];
        assert_eq!(predict(&sparse, &mode).status, "not-enough-data");
    }

    #[test]
    fn infers_opponents_only_when_exactly_the_missing_players_remain() {
        let mode = mode_info("competitive");
        let mut players: Vec<Player> = (0..5).map(|i| rated(&i.to_string(), 16000.0, "team", i == 0)).collect();
        players.extend((5..10).map(|i| rated(&i.to_string(), 15000.0, "", false)));
        let result = predict(&players, &mode);
        assert_eq!(result.status, "ok");
        assert_eq!(result.inferred_enemies, ["5", "6", "7", "8", "9"]);
        assert!((result.probability.unwrap() - win_probability(16000.0, 15000.0)).abs() < 1e-12);
        assert!((win_probability(16000.0, 15000.0) - 0.64).abs() < 0.01, "1,000 points is about 64%");
        players.push(rated("10", 15000.0, "", false));
        assert!(predict(&players, &mode).inferred_enemies.is_empty(), "six unassigned players cannot all be opponents");
    }

    #[test]
    fn rush_infers_the_three_opponents() {
        let mut players: Vec<Player> = (0..3).map(|i| rated(&i.to_string(), 12000.0, "team", i == 0)).collect();
        players.extend((3..6).map(|i| rated(&i.to_string(), 11000.0, "", false)));
        let result = predict(&players, &mode_info("rush"));
        assert_eq!(result.inferred_enemies, ["3", "4", "5"]);
        assert_eq!(result.status, "ok");
    }

    #[test]
    fn free_for_all_and_wingman() {
        assert_eq!(predict(&[rated("0", 1.0, "team", true)], &mode_info("deathmatch")).status, "ffa");
        let wingman = [rated("0", 9000.0, "", true), rated("a", 9000.0, "team", false), rated("b", 8000.0, "", false), rated("c", 8000.0, "", false)];
        let result = predict(&wingman, &mode_info("scrimcomp2v2"));
        assert_eq!(result.inferred_enemies, ["b", "c"]);
        assert_eq!(result.status, "ok");
    }
}
