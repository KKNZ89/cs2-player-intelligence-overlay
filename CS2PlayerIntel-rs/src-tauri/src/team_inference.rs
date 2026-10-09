//! Likely sides from a shared current Steam lobby, anchored to a manually assigned or spectated player.
//! Historical teammates and friendship alone do not establish a party or a side in this match.
//!
//! A group whose certain members disagree is left alone, and certain sides are never changed.

use crate::model::Player;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inferred {
    pub side: String,
    pub reason: String,
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(size: usize) -> Self {
        Self { parent: (0..size).collect() }
    }

    fn find(&mut self, mut index: usize) -> usize {
        while self.parent[index] != index {
            self.parent[index] = self.parent[self.parent[index]];
            index = self.parent[index];
        }
        index
    }

    fn union(&mut self, a: usize, b: usize) {
        let (root_a, root_b) = (self.find(a), self.find(b));
        if root_a != root_b {
            self.parent[root_a] = root_b;
        }
    }
}

pub fn infer_sides(players: &[Player]) -> HashMap<String, Inferred> {
    let mut groups = UnionFind::new(players.len());
    let mut lobbies: HashMap<&str, usize> = HashMap::new();

    for (i, player) in players.iter().enumerate() {
        if let Some(party) = player.party.as_ref().filter(|party| !party.lobby.is_empty()) {
            match lobbies.get(party.lobby.as_str()) {
                Some(&first) => groups.union(i, first),
                None => {
                    lobbies.insert(party.lobby.as_str(), i);
                }
            }
        }
    }

    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..players.len() {
        let root = groups.find(i);
        members.entry(root).or_default().push(i);
    }

    let mut result = HashMap::new();
    for group in members.values().filter(|group| group.len() > 1) {
        let anchors: Vec<&Player> = group.iter().map(|&i| &players[i]).filter(|p| p.is_self || (p.side_source.is_certain() && !p.side.is_empty())).collect();
        let mut sides: Vec<&str> = anchors.iter().map(|p| if p.is_self { "team" } else { p.side.as_str() }).collect();
        sides.sort_unstable();
        sides.dedup();
        if sides.len() != 1 {
            continue;
        }
        let names: Vec<String> = anchors
            .iter()
            .take(2)
            .map(|p| {
                if p.is_self {
                    "you".to_string()
                } else if p.name.is_empty() {
                    p.steam_id.clone()
                } else {
                    p.name.clone()
                }
            })
            .collect();
        for &i in group {
            let player = &players[i];
            if player.is_self || player.side_source.is_certain() {
                continue;
            }
            result
                .insert(player.steam_id.clone(), Inferred { side: sides[0].to_string(), reason: format!("Shares a Steam lobby with {}", names.join(" and ")) });
        }
    }
    result
}

/// Shared recent Leetify matches before two players count as frequent partners.
pub const MIN_SHARED_MATCHES: f64 = 2.0;

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Partner {
    pub steam_id: String,
    pub name: String,
    /// Recent Leetify matches the two played on the same team.
    pub count: f64,
}

/// Other players in this lobby that each player often played with recently (Leetify). Information only:
/// past matches together do not show who is on which team now, so this never assigns a side.
pub fn frequent_partners(players: &[Player]) -> HashMap<String, Vec<Partner>> {
    let name = |p: &Player| if p.name.is_empty() { p.steam_id.clone() } else { p.name.clone() };
    let mut result: HashMap<String, Vec<Partner>> = HashMap::new();
    for player in players {
        let Some(mates) = player.provider("leetify").and_then(|r| r.data.get("recentTeammates")).and_then(|v| v.as_array()) else { continue };
        for mate in mates {
            let count = mate.get("count").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let Some(other) = mate.get("steamId").and_then(|v| v.as_str()).and_then(|id| players.iter().find(|p| p.steam_id == id)) else { continue };
            if count < MIN_SHARED_MATCHES || other.steam_id == player.steam_id {
                continue;
            }
            for (a, b) in [(player, other), (other, player)] {
                let list = result.entry(a.steam_id.clone()).or_default();
                match list.iter_mut().find(|p| p.steam_id == b.steam_id) {
                    Some(existing) => existing.count = existing.count.max(count),
                    None => list.push(Partner { steam_id: b.steam_id.clone(), name: if b.is_self { "you".into() } else { name(b) }, count }),
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Party, ProviderResult, SideSource};
    use serde_json::json;

    fn player(id: &str, name: &str, side: &str, source: SideSource, mates: &[(&str, u32)]) -> Player {
        let mut p = Player { steam_id: id.into(), name: name.into(), side: side.into(), side_source: source, ..Player::default() };
        if !mates.is_empty() {
            let list: Vec<_> = mates.iter().map(|(steam_id, count)| json!({ "steamId": steam_id, "count": count })).collect();
            let mut data = serde_json::Map::new();
            data.insert("recentTeammates".into(), list.into());
            p.providers.insert("leetify".into(), ProviderResult::ok(data));
        }
        p
    }

    #[test]
    fn parties_share_a_side_without_overriding_certain_ones() {
        let mut me = player("me", "Me", "team", SideSource::Own, &[]);
        me.is_self = true;
        let mut friend = player("f", "Friend in party", "", SideSource::None, &[]);
        friend.party = Some(Party { friend: true, lobby: "111".into(), server: String::new() });
        let mut spectated = player("s", "Spectated", "team", SideSource::Spectated, &[]);
        spectated.party = Some(Party { lobby: "222".into(), ..Party::default() });
        let mut duo = player("d", "Duo partner", "", SideSource::None, &[]);
        duo.party = spectated.party.clone();
        let mut enemy = player("e", "Enemy anchor", "enemy", SideSource::Manual, &[]);
        enemy.party = Some(Party { lobby: "333".into(), ..Party::default() });
        let mut enemy_duo = player("ed", "Enemy duo", "", SideSource::None, &[]);
        enemy_duo.party = enemy.party.clone();
        let players = vec![
            me,
            spectated,
            duo,
            enemy,
            enemy_duo,
            friend,
            player("once", "Historical teammate", "", SideSource::None, &[("s", 20)]),
            player("mine", "Chosen by you", "enemy", SideSource::Manual, &[]),
        ];
        let result = infer_sides(&players);
        assert_eq!(result["d"], Inferred { side: "team".into(), reason: "Shares a Steam lobby with Spectated".into() });
        assert_eq!(result["ed"].side, "enemy");
        assert!(!result.contains_key("f"), "friendship and an unrelated lobby do not establish your party");
        assert!(!result.contains_key("once"), "past teammates may be opponents now");
        assert!(!result.contains_key("mine"), "your own choice is never overridden");
    }

    #[test]
    fn conflicting_groups_are_left_alone() {
        let mut me = player("me", "Me", "team", SideSource::Own, &[]);
        me.is_self = true;
        let mut players = vec![me, player("a", "A", "enemy", SideSource::Manual, &[]), player("b", "B", "", SideSource::None, &[])];
        for p in &mut players {
            p.party = Some(Party { lobby: "111".into(), ..Party::default() });
        }
        assert!(infer_sides(&players).is_empty());
    }

    #[test]
    fn frequent_partners_are_reported_without_setting_sides() {
        let players = vec![
            player("a", "A", "", SideSource::None, &[("b", 5), ("gone", 9)]),
            player("b", "B", "", SideSource::None, &[]),
            player("c", "C", "", SideSource::None, &[("a", 1)]),
        ];
        let partners = frequent_partners(&players);
        assert_eq!(partners["a"], [Partner { steam_id: "b".into(), name: "B".into(), count: 5.0 }]);
        assert_eq!(partners["b"][0].steam_id, "a", "the link shows on both players");
        assert!(!partners.contains_key("c"), "one shared match is not a pattern, and players not in this lobby are ignored");
        assert!(infer_sides(&players).is_empty(), "history never assigns a side");
    }
}
