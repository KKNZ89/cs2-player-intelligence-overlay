//! Steam IDs typed or pasted by the user.

use crate::model::{now_ms, Candidate};
use regex::Regex;
use std::sync::LazyLock;

static PROFILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:https?://steamcommunity\.com/profiles/)?(7656119\d{10})(?:/?(?:\?\S*)?)$").unwrap());

/// A SteamID64 from either the bare ID or a numeric profile link; empty when neither.
pub fn numeric_steam_id(input: &str) -> String {
    PROFILE.captures(input.trim()).map(|c| c[1].to_string()).unwrap_or_default()
}

/// One player per line. Your own ID is skipped because GSI already provides it.
pub fn parse_roster_input(input: &str, self_steam_id: &str, max_others: usize) -> Result<Vec<Candidate>, String> {
    if input.len() > 10_000 {
        return Err(format!("Paste up to {max_others} numeric Steam profile links or SteamID64 values."));
    }
    let mut players: Vec<Candidate> = Vec::new();
    for (index, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let steam_id = numeric_steam_id(line);
        if steam_id.is_empty() {
            return Err(format!("Line {}: use a SteamID64 or https://steamcommunity.com/profiles/SteamID64. Custom /id/ links need a numeric ID.", index + 1));
        }
        if steam_id == self_steam_id || players.iter().any(|p| p.steam_id == steam_id) {
            continue;
        }
        players.push(Candidate {
            name: steam_id.clone(),
            steam_id,
            source: "manual".into(),
            confidence: "user-selected".into(),
            seen_at: Some(now_ms()),
            ..Candidate::default()
        });
    }
    if players.is_empty() {
        return Err("Add at least one other player. Your own ID is already provided by GSI.".into());
    }
    if players.len() > max_others {
        return Err(format!("Select at most {max_others} other players for this mode."));
    }
    Ok(players)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ids_and_numeric_profile_links() {
        assert_eq!(numeric_steam_id(" 76561198012345678 "), "76561198012345678");
        assert_eq!(numeric_steam_id("https://steamcommunity.com/profiles/76561198012345678/"), "76561198012345678");
        assert_eq!(numeric_steam_id("https://steamcommunity.com/id/someone"), "");
        assert_eq!(numeric_steam_id("765611980123456789"), "");
    }

    #[test]
    fn parses_lists_with_limits() {
        let me = "76561198000000000";
        let list = format!("{me}\n76561198000000001\n\n76561198000000001\r\n76561198000000002");
        let players = parse_roster_input(&list, me, 9).unwrap();
        assert_eq!(players.len(), 2);
        assert_eq!(players[0].confidence, "user-selected");
        assert!(parse_roster_input("nope", me, 9).unwrap_err().starts_with("Line 1"));
        assert!(parse_roster_input(me, me, 9).is_err());
        assert!(parse_roster_input("76561198000000001\n76561198000000002", me, 1).unwrap_err().contains("at most 1"));
    }
}
