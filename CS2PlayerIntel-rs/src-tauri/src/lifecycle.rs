//! Match boundaries from GSI. GSI has no match ID, so these deliberately discard stale data: a map or
//! mode change, a return to warmup, a round rollback, gameover, the menu, or a disconnect ends a match.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub connected: bool,
    pub map: String,
    pub mode: String,
    pub phase: String,
    pub round: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Transition {
    pub reset: bool,
    pub ended: bool,
    pub started: bool,
}

#[derive(Default)]
pub struct MatchLifecycle {
    previous: Option<Snapshot>,
    pub active: bool,
}

impl MatchLifecycle {
    pub fn update(&mut self, state: &Snapshot) -> Transition {
        let active = state.connected && !state.map.is_empty() && matches!(state.phase.as_str(), "warmup" | "live" | "intermission");
        let (changed, restarted) = match &self.previous {
            Some(previous) => (
                previous.map != state.map || previous.mode != state.mode,
                (state.phase == "warmup" && previous.phase != "warmup") || matches!((state.round, previous.round), (Some(now), Some(before)) if now < before),
            ),
            None => (false, false),
        };
        let reset = if active { !self.active || changed || restarted } else { self.active };
        let ended = self.active && (!active || reset);
        self.previous = Some(state.clone());
        self.active = active;
        Transition { reset, ended, started: active && reset }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(map: &str, round: i64) -> Snapshot {
        Snapshot { connected: true, map: map.into(), mode: "competitive".into(), phase: "live".into(), round: Some(round) }
    }

    #[test]
    fn boundaries_cover_map_changes_rollback_warmup_and_disconnect() {
        let mut lifecycle = MatchLifecycle::default();
        assert_eq!(lifecycle.update(&live("de_mirage", 3)), Transition { reset: true, ended: false, started: true });
        assert_eq!(lifecycle.update(&live("de_mirage", 4)), Transition::default());
        assert!(lifecycle.update(&live("de_mirage", 2)).started, "a round rollback starts a new match");
        assert!(lifecycle.update(&live("de_inferno", 2)).ended, "a map change ends the match");
        let warmup = Snapshot { phase: "warmup".into(), ..live("de_inferno", 0) };
        assert!(lifecycle.update(&warmup).reset, "returning to warmup resets");
        let gone = Snapshot { connected: false, ..warmup };
        let transition = lifecycle.update(&gone);
        assert!(transition.ended && !transition.started && !lifecycle.active);
        let over = Snapshot { phase: "gameover".into(), ..live("de_nuke", 20) };
        lifecycle.update(&live("de_nuke", 19));
        assert!(lifecycle.update(&over).ended, "gameover ends the match");
    }
}
