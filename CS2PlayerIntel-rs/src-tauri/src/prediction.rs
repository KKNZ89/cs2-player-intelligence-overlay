//! Win chance: this app's own estimate, not a provider metric.
//!
//! Before the match it compares the two teams' strength. A player's strength has two parts:
//! - **Level**: ratings matchmaking itself uses, comparable between any two players: Premier, FACEIT Elo
//!   (or level), the Competitive skill group on this map, the Wingman rank. Each counts most in its own
//!   mode. A player whose level is unknown is taken to be at the lobby's level, since matchmaking puts
//!   players of similar level together.
//! - **Form**: how well they do against the players they usually meet (HLTV and Leetify ratings, K/D,
//!   ADR, win rate). These are relative to their own level, so they shift a player up or down from it
//!   rather than standing in for it, and a stat from few matches counts for less.
//!
//! A team is always its full size: a slot with nobody known counts as the lobby average, and a player whose
//! side or presence in this match is not confirmed counts partly as the lobby average.
//!
//! During Competitive, Premier and Wingman matches the rest of the match is played out round by round
//! (ties and Premier overtime included). Each round's chance combines:
//! - your team's strength per round, from the stats and then from the rounds so far;
//! - the economy: the pistol round's winner usually takes the next round and often the one after, and
//!   any round's winner keeps their weapons;
//! - the map: on a CT-sided map the CT half is easier, so a lead built on the strong side is worth less
//!   with the weak side still to play.
//!
//! Rounds so far count as evidence of strength by how surprising they were: winning the round after your
//! own pistol win says little. CS2's round-by-round results (GSI `map_round_wins`) place each round; without
//! them only the score is known and it counts for less. Your own kills and deaths, the only live player
//! stats CS2 shares, adjust your part of your team.
//!
//! The scale from strength to chance starts as a rule of thumb (about 64% for a team 1,000 Premier ahead)
//! and is refitted to the results of your recorded matches as they accumulate (see [`calibrate`]). The
//! map's CT edge starts as a rough figure per map and moves towards the rounds recorded on that map.
//! Free-for-all modes have no sides to compare.

use crate::model::{Player, SideSource};
use crate::modes::ModeInfo;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// Bumped whenever strengths are computed differently, so recorded gaps from another model are not mixed
/// into the calibration.
pub const MODEL_VERSION: i64 = 2;
/// Logistic slope per standard deviation of average team strength before any calibration: a team 1,000
/// Premier ahead in every player is about a 64% favourite.
pub const SLOPE: f64 = 3.5;
/// How sure the rule of thumb is: the calibration needs a few dozen matches to move the slope far.
const SLOPE_PRIOR_SPREAD: f64 = 1.0;
const MIN_KNOWN_PER_TEAM: usize = 2;
/// Before any round is played the estimate stays within these bounds; history only says so much.
const PRE_MATCH_BOUNDS: (f64, f64) = (0.1, 0.9);
/// Form counts this much compared with level: a K/D one spread above typical is worth about 1,750 Premier.
const FORM_WEIGHT: f64 = 0.35;
/// How much level evidence (in source weights) counts as much as the lobby's level.
const LEVEL_SHRINK: f64 = 0.15;
/// Your own K/D this match counts fully after about this many rounds (half after this many).
const LIVE_HALF_ROUNDS: f64 = 12.0;
/// Log-odds edges from the economy (rules of thumb): the pistol winner takes the next round about 80% of
/// the time, the round after that about 67% if they won both, and any round's winner the next about 54%
/// (kept small: much of a winner's streak is the stronger team winning, which strength already covers).
const PISTOL_EDGE: f64 = 1.4;
const CONVERTED_EDGE: f64 = 0.7;
const LAST_ROUND_EDGE: f64 = 0.15;
/// Rounds known only from the score (not which round or on which side) count this much.
const UNPLACED_ROUND_WEIGHT: f64 = 0.6;
/// Share of rounds CT wins between equal teams per map in Competitive and Premier: rough figures from
/// public match statistics, moved towards the rounds you record (other maps and Wingman start even).
const MAP_CT_RATES: [(&str, f64); 9] = [
    ("de_nuke", 0.55),
    ("de_train", 0.54),
    ("de_ancient", 0.53),
    ("de_overpass", 0.53),
    ("de_inferno", 0.52),
    ("de_mirage", 0.51),
    ("de_vertigo", 0.51),
    ("de_dust2", 0.49),
    ("de_anubis", 0.47),
];
/// How many recorded rounds the rough figure for a map is worth.
const MAP_PRIOR_ROUNDS: f64 = 400.0;
/// Matches assumed behind a provider's stats when it does not say (CSRep's overview covers 25).
const DEFAULT_SAMPLE: f64 = 25.0;

/// A rating comparable between players: a typical player's value and spread.
struct Level {
    key: &'static str,
    mean: f64,
    spread: f64,
}

const LEVELS: [Level; 4] = [
    Level { key: "premier", mean: 12_000.0, spread: 5_000.0 },
    Level { key: "faceitElo", mean: 1_200.0, spread: 450.0 },
    // Competitive skill groups run 1 (Silver I) to 18 (Global Elite); Wingman uses the same names.
    Level { key: "mapRank", mean: 9.0, spread: 4.0 },
    Level { key: "wingmanRank", mean: 9.0, spread: 4.0 },
];

/// How much a rating says in this mode: the one matchmaking uses here counts fully.
fn level_weight(key: &str, mode: &str) -> f64 {
    match (key, mode) {
        ("premier", "premier") | ("mapRank", "competitive") | ("wingmanRank", "scrimcomp2v2" | "wingman") => 1.0,
        ("premier", _) => 0.8,
        ("faceitElo", _) => 0.7,
        ("mapRank", _) => 0.4,
        _ => 0.3,
    }
}

/// A performance stat relative to the player's usual opposition: where it comes from (provider, key,
/// scale, and the provider's field holding the matches it covers), a typical value and spread, how much it
/// counts, and after how many matches it is half reliable.
struct Form {
    key: &'static str,
    sources: &'static [(&'static str, &'static str, f64, &'static str)],
    mean: f64,
    spread: f64,
    weight: f64,
    half_reliable: f64,
}

const FORMS: [Form; 5] = [
    Form { key: "hltv", sources: &[("csstats", "hltv", 1.0, "matches"), ("csrep", "hltv", 1.0, "")], mean: 1.0, spread: 0.15, weight: 1.0, half_reliable: 5.0 },
    Form { key: "leetifyRating", sources: &[("leetify", "leetifyRating", 1.0, "totalMatches")], mean: 0.0, spread: 1.5, weight: 0.8, half_reliable: 5.0 },
    Form {
        key: "kd",
        sources: &[("csstats", "kd", 1.0, "matches"), ("csrep", "kd", 1.0, ""), ("faceit", "kd", 1.0, "matches")],
        mean: 1.0,
        spread: 0.25,
        weight: 0.7,
        half_reliable: 5.0,
    },
    Form { key: "adr", sources: &[("csstats", "adr", 1.0, "matches"), ("csrep", "adr", 1.0, "")], mean: 75.0, spread: 12.0, weight: 0.6, half_reliable: 5.0 },
    // A win rate drifts towards 50% as ratings settle and is noisy over few matches.
    Form {
        key: "winRate",
        sources: &[("csstats", "winRate", 1.0, "matches"), ("faceit", "winRate", 1.0, "matches"), ("leetify", "winrate", 100.0, "totalMatches")],
        mean: 50.0,
        spread: 7.0,
        weight: 0.5,
        half_reliable: 40.0,
    },
];

/// The middle of each FACEIT level's Elo range, for players whose level is known but not their Elo.
const FACEIT_LEVEL_ELO: [f64; 10] = [300.0, 625.0, 825.0, 975.0, 1_125.0, 1_275.0, 1_440.0, 1_640.0, 1_875.0, 2_200.0];

/// What CS2 shows live: the map, the round score from your side's view, your side, which side won each
/// round, and your own kills and deaths.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveInput {
    pub mode: String,
    pub map: String,
    pub ours: Option<i64>,
    pub theirs: Option<i64>,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    /// "CT" or "T" now, and in the first half ("" when not known).
    pub side: String,
    pub start_side: String,
    /// Between rounds or at half-time, when `side` still belongs to the round just played.
    pub round_over: bool,
    /// The side that won each finished round ("CT", "T" or ""), from GSI `map_round_wins`.
    pub round_wins: Vec<String>,
}

/// One stat compared between the teams: averages over the players who have it.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Factor {
    pub key: &'static str,
    pub team: f64,
    pub enemy: f64,
    pub team_count: usize,
    pub enemy_count: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveView {
    pub ours: i64,
    pub theirs: i64,
    /// Rounds a side needs to win in regulation.
    pub target: i64,
    /// CS2 reported which side won each round, so each round could be placed.
    pub rounds_known: bool,
}

/// How the map's sides were counted: CT's share of rounds between equal teams, the recorded rounds it
/// rests on besides the rough figure, your side now and after half-time.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SideView {
    pub map: String,
    pub ct_rate: f64,
    pub recorded_rounds: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_half: Option<&'static str>,
}

/// Rounds recorded on one map: how many CT won.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MapRounds {
    pub ct: u32,
    pub rounds: u32,
}

/// Recorded rounds are pooled per map and kind of match: Wingman maps are played differently.
pub fn map_key(mode: &str, map: &str) -> String {
    let kind = if matches!(mode.to_ascii_lowercase().as_str(), "scrimcomp2v2" | "wingman") { "wingman" } else { "5v5" };
    format!("{kind}:{map}")
}

/// How the estimate has done on your recorded matches, and the slope fitted to them.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    /// Recorded matches with an estimate from this model and a known result.
    pub matches: usize,
    /// Matches with a favourite (estimate not 50%) and a winner.
    pub decided: usize,
    /// How many of those the favourite won.
    pub favourite_won: usize,
    /// Mean squared error of the recorded estimates (a coin flip scores 0.25; lower is better).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brier: Option<f64>,
    pub slope: f64,
    /// Rounds recorded per map (see [`map_key`]).
    #[serde(skip)]
    pub map_rounds: BTreeMap<String, MapRounds>,
}

impl Default for Calibration {
    fn default() -> Self {
        Self { matches: 0, decided: 0, favourite_won: 0, brier: None, slope: SLOPE, map_rounds: BTreeMap::new() }
    }
}

impl Calibration {
    /// CT's share of rounds between equal teams on this map: the rough figure moved towards your recorded
    /// rounds there, and how many rounds those are.
    pub fn ct_rate(&self, mode: &str, map: &str) -> (f64, u32) {
        let key = map_key(mode, map);
        let rough = if key.starts_with("5v5:") { MAP_CT_RATES.iter().find(|(m, _)| *m == map).map_or(0.5, |(_, rate)| *rate) } else { 0.5 };
        let recorded = self.map_rounds.get(&key).copied().unwrap_or_default();
        ((rough * MAP_PRIOR_ROUNDS + recorded.ct as f64) / (MAP_PRIOR_ROUNDS + recorded.rounds as f64), recorded.rounds)
    }
}

/// One recorded match for the calibration: the estimate shown, the strength gap behind it, and your
/// points (1 a win, 0.5 a tie, 0 a loss).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outcome {
    pub estimate: f64,
    pub gap: f64,
    pub points: f64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Prediction {
    pub status: &'static str,
    pub team_size: usize,
    pub enemy_size: usize,
    pub inferred_enemies: Vec<String>,
    /// Players per side with at least one stat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_rated: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enemy_rated: Option<usize>,
    /// Players counted only partly because their side or presence in this match is not confirmed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unconfirmed: Option<usize>,
    /// Places in the two teams with nobody known, counted as the lobby average.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_slots: Option<usize>,
    /// Chance your team wins (a tie, where the mode has one, is neither).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probability: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tie: Option<f64>,
    /// The estimate from stats alone, before the live score and your own K/D (a tie counting half).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_match: Option<f64>,
    /// Your team's average strength minus the opponents', in spreads; recorded for the calibration.
    #[serde(skip)]
    pub gap: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub factors: Vec<Factor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<LiveView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<SideView>,
    /// "pistol-won", "pistol-lost", "converted-won" or "converted-lost" when the economy favours a side
    /// in the next round.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub economy: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibration: Option<Calibration>,
}

fn ok_number(player: &Player, provider: &str, key: &str) -> Option<f64> {
    player.provider(provider).filter(|r| r.is_ok()).and_then(|r| r.number(key))
}

/// A rating as reported, `None` when unknown or unranked (zero is how sources say "no rating").
fn level_value(player: &Player, key: &str, live: &LiveInput) -> Option<f64> {
    let value = match key {
        "premier" => ok_number(player, "leetify", "premier"),
        "faceitElo" => {
            let elo = ["faceit", "leetify"].iter().zip(["elo", "faceitElo"]).find_map(|(p, k)| ok_number(player, p, k).filter(|v| *v > 0.0));
            elo.or_else(|| {
                let level = ok_number(player, "faceit", "level").or_else(|| ok_number(player, "leetify", "faceitLevel"))?;
                FACEIT_LEVEL_ELO.get((level.round() as usize).checked_sub(1)?).copied()
            })
        }
        "mapRank" => player
            .provider("leetify")
            .filter(|r| r.is_ok() && !live.map.is_empty())
            .and_then(|r| r.data.get("mapRanks")?.as_array()?.iter().find(|m| m["map"].as_str() == Some(live.map.as_str()))?["rank"].as_f64()),
        "wingmanRank" => ok_number(player, "leetify", "wingman"),
        _ => None,
    };
    value.filter(|v| *v > 0.0)
}

/// A form stat as reported (averaged over the sources that have it), and how reliable it is.
fn form_value(player: &Player, form: &Form) -> Option<(f64, f64)> {
    let mut found: Vec<(f64, f64)> = Vec::new();
    for (provider, key, scale, sample) in form.sources {
        let Some(value) = ok_number(player, provider, key).map(|v| v * scale) else { continue };
        // No real player has a zero K/D-sized stat; zero means none. The Leetify rating is centred on zero.
        if value <= 0.0 && form.key != "leetifyRating" {
            continue;
        }
        let matches = Some(sample).filter(|s| !s.is_empty()).and_then(|s| ok_number(player, provider, s)).unwrap_or(DEFAULT_SAMPLE);
        found.push((value, matches / (matches + form.half_reliable)));
    }
    if found.is_empty() {
        return None;
    }
    let value = found.iter().map(|(v, _)| v).sum::<f64>() / found.len() as f64;
    let reliability = 1.0 - found.iter().map(|(_, r)| 1.0 - r).product::<f64>();
    Some((value, reliability))
}

/// A stat's value for the factor list, the way it is shown.
fn shown_value(player: &Player, key: &str, live: &LiveInput) -> Option<f64> {
    match FORMS.iter().find(|f| f.key == key) {
        Some(form) => form_value(player, form).map(|(value, _)| value),
        None => level_value(player, key, live),
    }
}

/// A player's level (spreads from a typical player, and the weight of evidence) and form (spreads, already
/// discounted for small samples), each when anything is known.
struct Strength {
    level: Option<(f64, f64)>,
    form: Option<f64>,
}

fn strength(player: &Player, live: &LiveInput) -> Strength {
    let mode = live.mode.to_ascii_lowercase();
    let (mut sum, mut weight) = (0.0, 0.0);
    for level in &LEVELS {
        if let Some(value) = level_value(player, level.key, live) {
            let w = level_weight(level.key, &mode);
            sum += w * ((value - level.mean) / level.spread).clamp(-3.0, 3.0);
            weight += w;
        }
    }
    let (mut form_sum, mut form_weight) = (0.0, 0.0);
    for form in &FORMS {
        if let Some((value, reliability)) = form_value(player, form) {
            form_sum += form.weight * reliability * ((value - form.mean) / form.spread).clamp(-3.0, 3.0);
            form_weight += form.weight;
        }
    }
    Strength { level: (weight > 0.0).then(|| (sum / weight, weight)), form: (form_weight > 0.0).then(|| form_sum / form_weight) }
}

/// How much of a player to count: 1 when they are certainly in this match on this side, less when their
/// side was inferred or they come only from Steam's recently played list.
fn certainty(player: &Player, inferred: bool) -> f64 {
    if player.is_self {
        return 1.0;
    }
    let present = player.side_source.is_certain() || matches!(player.confidence.as_str(), "confirmed" | "user-selected");
    let side = if player.side_source.is_certain() {
        1.0
    } else if inferred {
        0.85
    } else if player.side_source == SideSource::Likely {
        0.9
    } else {
        1.0
    };
    side * if present { 1.0 } else { 0.8 }
}

fn logistic(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// How a mode is won: rounds needed, rounds per half, and whether a tie at one short each goes to overtime
/// (Premier). Overtime is played in halves of three rounds, first to four of six, repeated after 3:3.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rules {
    target: i64,
    half: i64,
    overtime: bool,
}

const OVERTIME_HALF: i64 = 3;

fn rules(mode: &str) -> Option<Rules> {
    match mode.to_ascii_lowercase().as_str() {
        "competitive" => Some(Rules { target: 13, half: 12, overtime: false }),
        "premier" => Some(Rules { target: 13, half: 12, overtime: true }),
        "scrimcomp2v2" | "wingman" => Some(Rules { target: 9, half: 8, overtime: false }),
        _ => None,
    }
}

/// Rounds per half in this mode, for working out which side you started on.
pub fn half_length(mode: &str) -> Option<i64> {
    rules(mode).map(|r| r.half)
}

impl Rules {
    fn regulation(&self) -> i64 {
        2 * self.half
    }

    /// Which half round `i` (counted from 0) is in; overtime halves follow the two regulation halves.
    fn half_of(&self, i: i64) -> i64 {
        if i < self.regulation() {
            i / self.half
        } else {
            2 + (i - self.regulation()) / OVERTIME_HALF
        }
    }

    fn starts_half(&self, i: i64) -> bool {
        i == 0 || self.half_of(i) != self.half_of(i - 1)
    }

    /// The first round of a regulation half; overtime halves start with money for a full buy instead.
    fn is_pistol(&self, i: i64) -> bool {
        i < self.regulation() && i % self.half == 0
    }
}

/// What the economy says about the next round, from your side's view (`true` when you won).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Economy {
    /// The first round of a half: both teams start equal.
    Reset,
    /// The last round's winner is not known.
    Unknown,
    /// The round after a pistol round: the winner has rifles against pistols.
    Pistol(bool),
    /// The pistol winner also won the next round: the losers are often still short of money.
    Converted(bool),
    /// Any other round: the last round's winner keeps their weapons.
    Last(bool),
}

impl Economy {
    fn index(self) -> usize {
        match self {
            Self::Reset => 0,
            Self::Unknown => 1,
            Self::Pistol(won) => 2 + won as usize,
            Self::Converted(won) => 4 + won as usize,
            Self::Last(won) => 6 + won as usize,
        }
    }

    /// Added to your log-odds of winning the round.
    fn edge(self) -> f64 {
        let signed = |won: bool, size: f64| if won { size } else { -size };
        match self {
            Self::Pistol(won) => signed(won, PISTOL_EDGE),
            Self::Converted(won) => signed(won, CONVERTED_EDGE),
            Self::Last(won) => signed(won, LAST_ROUND_EDGE),
            Self::Reset | Self::Unknown => 0.0,
        }
    }

    /// The economy after a round you `won` (or lost) inside a half; `pistol` when it was a pistol round.
    fn after(self, won: bool, pistol: bool) -> Self {
        match self {
            Self::Reset if pistol => Self::Pistol(won),
            Self::Pistol(winner) if winner == won => Self::Converted(winner),
            _ => Self::Last(won),
        }
    }

    /// For the UI: a pistol round and its follow-up are worth pointing out.
    fn label(self) -> Option<&'static str> {
        match self {
            Self::Pistol(true) => Some("pistol-won"),
            Self::Pistol(false) => Some("pistol-lost"),
            Self::Converted(true) => Some("converted-won"),
            Self::Converted(false) => Some("converted-lost"),
            _ => None,
        }
    }
}

/// A race to `target` (a tie at one short each) from 0:0, where `chance(round, economy)` is your chance
/// in a round counted from the race's start and `next(round, economy, won)` the economy for the round after.
struct Race<C, N> {
    target: i64,
    chance: C,
    next: N,
}

impl<C: Fn(i64, Economy) -> f64, N: Fn(i64, Economy, bool) -> Economy> Race<C, N> {
    /// Chances to win and to tie from `ours`:`theirs` with `economy` for the next round.
    fn run(&self, ours: i64, theirs: i64, economy: Economy) -> (f64, f64) {
        let size = self.target as usize + 1;
        let mut memo = vec![None; size * size * 8];
        self.from(&mut memo, ours.clamp(0, self.target), theirs.clamp(0, self.target), economy)
    }

    fn from(&self, memo: &mut [Option<(f64, f64)>], a: i64, b: i64, economy: Economy) -> (f64, f64) {
        if a == self.target {
            return (1.0, 0.0);
        }
        if b == self.target {
            return (0.0, 0.0);
        }
        if a == self.target - 1 && b == self.target - 1 {
            return (0.0, 1.0);
        }
        let key = (a * (self.target + 1) + b) as usize * 8 + economy.index();
        if let Some(known) = memo[key] {
            return known;
        }
        let round = a + b;
        let p = (self.chance)(round, economy);
        let won = self.from(memo, a + 1, b, (self.next)(round, economy, true));
        let lost = self.from(memo, a, b + 1, (self.next)(round, economy, false));
        let result = (p * won.0 + (1.0 - p) * lost.0, p * won.1 + (1.0 - p) * lost.1);
        memo[key] = Some(result);
        result
    }
}

/// A round placed from your side's view: whether you won it, its number (from 0), and the economy before it.
type Placed = (bool, i64, Economy);

/// The match being forecast round by round: its rules, how much the map favours CT, and which side you
/// play in each round as far as it is known.
struct Game {
    rules: Rules,
    /// Log-odds edge of the CT side in a round between equal teams on this map.
    ct_edge: f64,
    /// Your side now (true for CT) and the round it belongs to.
    current: Option<bool>,
    reference: i64,
    /// Your side in the first half.
    start: Option<bool>,
}

impl Game {
    fn new(rules: Rules, live: &LiveInput, ct_rate: f64) -> Self {
        let side = |s: &str| match s {
            "CT" => Some(true),
            "T" => Some(false),
            _ => None,
        };
        let played = live.ours.unwrap_or(0).max(0) + live.theirs.unwrap_or(0).max(0);
        // Between rounds (and at half-time) CS2 still shows the side of the round just played.
        let reference = if live.round_over { (played - 1).max(0) } else { played };
        let ct_rate = ct_rate.clamp(0.3, 0.7);
        Self { rules, ct_edge: (ct_rate / (1.0 - ct_rate)).ln(), current: side(&live.side), reference, start: side(&live.start_side) }
    }

    /// Your side in round `i` (true for CT): the side you are on now within this half, the other one in
    /// the other regulation half. Overtime sides are only known for the half being played.
    fn side_of(&self, i: i64) -> Option<bool> {
        let r = self.rules;
        if let Some(now) = self.current.filter(|_| r.half_of(i) == r.half_of(self.reference)) {
            return Some(now);
        }
        if i >= r.regulation() {
            return None;
        }
        if let Some(start) = self.start {
            return Some(if i < r.half { start } else { !start });
        }
        self.current.filter(|_| self.reference < r.regulation()).map(|now| !now)
    }

    /// The map's side edge for you in round `i`; overtime is treated as even.
    fn side_edge(&self, i: i64) -> f64 {
        match self.side_of(i).filter(|_| i < self.rules.regulation()) {
            Some(true) => self.ct_edge,
            Some(false) => -self.ct_edge,
            None => 0.0,
        }
    }

    /// Your chance in round `i` with base `strength` (log-odds per round against these opponents).
    fn round(&self, strength: f64, i: i64, economy: Economy) -> f64 {
        logistic(strength + self.side_edge(i) + economy.edge())
    }

    fn next(&self, i: i64, economy: Economy, won: bool) -> Economy {
        if self.rules.starts_half(i + 1) {
            Economy::Reset
        } else {
            economy.after(won, self.rules.is_pistol(i))
        }
    }

    /// One overtime (six rounds from `base`:`base`).
    fn overtime(&self, strength: f64, base: i64) -> Race<impl Fn(i64, Economy) -> f64 + '_, impl Fn(i64, Economy, bool) -> Economy + '_> {
        Race { target: OVERTIME_HALF + 1, chance: move |i, e| self.round(strength, 2 * base + i, e), next: move |i, e, won| self.next(2 * base + i, e, won) }
    }

    /// Chances to win and to tie from `ours`:`theirs` with `economy` for the next round.
    fn finish(&self, strength: f64, ours: i64, theirs: i64, economy: Economy) -> (f64, f64) {
        let r = self.rules;
        let regulation = Race { target: r.target, chance: |i, e| self.round(strength, i, e), next: |i, e, won| self.next(i, e, won) };
        if !r.overtime {
            return regulation.run(ours, theirs, economy);
        }
        let last = r.target - 1;
        let (once_won, once_tied) = self.overtime(strength, last).run(0, 0, Economy::Reset);
        let overtime_win = if once_tied < 1.0 { once_won / (1.0 - once_tied) } else { 0.5 };
        let (won, tied) = if ours.min(theirs) < last {
            regulation.run(ours, theirs, economy)
        } else {
            let base = last + OVERTIME_HALF * ((ours.min(theirs) - last) / OVERTIME_HALF);
            self.overtime(strength, base).run(ours - base, theirs - base, economy)
        };
        (won + tied * overtime_win, 0.0)
    }

    /// The base strength that makes the match from 0:0 worth `match_chance` (a tie counting half).
    fn strength_for(&self, match_chance: f64) -> f64 {
        let (mut low, mut high) = (-4.0, 4.0);
        for _ in 0..40 {
            let middle = (low + high) / 2.0;
            let (won, tied) = self.finish(middle, 0, 0, Economy::Reset);
            if won + tied / 2.0 < match_chance {
                low = middle;
            } else {
                high = middle;
            }
        }
        (low + high) / 2.0
    }

    /// The rounds so far from your side's view: each placed round (won, index, economy before it), the wins
    /// and losses that could not be placed, and the economy for the next round.
    fn history(&self, live: &LiveInput, ours: i64, theirs: i64) -> (Vec<Placed>, (i64, i64), Economy) {
        let played = ours + theirs;
        let placed_rounds = live.round_wins.len() == played as usize;
        let mut placed = Vec::new();
        let mut economy = Economy::Reset;
        for i in 0..played {
            let won = placed_rounds
                .then(|| match (live.round_wins[i as usize].as_str(), self.side_of(i)) {
                    ("CT", Some(ct)) => Some(ct),
                    ("T", Some(ct)) => Some(!ct),
                    _ => None,
                })
                .flatten();
            if let Some(won) = won {
                placed.push((won, i, economy));
            }
            economy = match won {
                _ if self.rules.starts_half(i + 1) => Economy::Reset,
                Some(won) => economy.after(won, self.rules.is_pistol(i)),
                None => Economy::Unknown,
            };
        }
        if !placed_rounds && !self.rules.starts_half(played) {
            // Without round-by-round results, only the first rounds can be read from the score.
            economy = match (ours, theirs) {
                (1, 0) => Economy::Pistol(true),
                (0, 1) => Economy::Pistol(false),
                (2, 0) => Economy::Converted(true),
                (0, 2) => Economy::Converted(false),
                _ => Economy::Unknown,
            };
        }
        let wins = placed.iter().filter(|(won, _, _)| *won).count() as i64;
        let unplaced = ((ours - wins).max(0), (theirs - (placed.len() as i64 - wins)).max(0));
        (placed, unplaced, economy)
    }

    /// Your base strength after the rounds so far, starting from `prior` with uncertainty `spread`. A round
    /// counts by how surprising it was: winning the round after your pistol win says little, winning on the
    /// map's weak side against a full buy says much. Rounds that could not be placed count partly.
    fn updated_strength(&self, prior: f64, spread: f64, placed: &[Placed], unplaced: (i64, i64)) -> f64 {
        if placed.is_empty() && unplaced == (0, 0) {
            return prior;
        }
        let log_posterior = |strength: f64| {
            let mut sum = -0.5 * ((strength - prior) / spread).powi(2);
            for &(won, i, economy) in placed {
                let p = self.round(strength, i, economy);
                sum += if won { p.ln() } else { (1.0 - p).ln() };
            }
            let p = logistic(strength);
            sum + UNPLACED_ROUND_WEIGHT * (unplaced.0 as f64 * p.ln() + unplaced.1 as f64 * (1.0 - p).ln())
        };
        let grid: Vec<(f64, f64)> = (-40..=40).map(|k| prior + spread * k as f64 / 10.0).map(|s| (s, log_posterior(s))).collect();
        let top = grid.iter().map(|(_, l)| *l).fold(f64::NEG_INFINITY, f64::max);
        let (sum, weight) = grid.iter().fold((0.0, 0.0), |(sum, weight), (s, l)| {
            let w = (l - top).exp();
            (sum + s * w, weight + w)
        });
        sum / weight
    }
}

/// How sure the stats are about the per-round strength (log-odds): the less they cover, the sooner the
/// rounds played take over.
fn strength_spread(confidence: &str) -> f64 {
    match confidence {
        "high" => 0.22,
        "medium" => 0.26,
        _ => 0.30,
    }
}

/// Fits the slope to recorded results (most likely slope, starting from the rule of thumb, which a few
/// matches barely move) and scores the estimates that were shown.
pub fn calibrate(outcomes: &[Outcome]) -> Calibration {
    if outcomes.is_empty() {
        return Calibration::default();
    }
    let fit = |slope: f64| {
        let prior = -0.5 * ((slope - SLOPE) / SLOPE_PRIOR_SPREAD).powi(2);
        prior
            + outcomes
                .iter()
                .map(|o| {
                    let p = logistic(slope * o.gap).clamp(1e-6, 1.0 - 1e-6);
                    o.points * p.ln() + (1.0 - o.points) * (1.0 - p).ln()
                })
                .sum::<f64>()
    };
    let slope = (0..=200).map(|i| i as f64 * 0.05).max_by(|a, b| fit(*a).total_cmp(&fit(*b))).unwrap_or(SLOPE);
    let decided: Vec<&Outcome> = outcomes.iter().filter(|o| (o.estimate - 0.5).abs() >= 0.01 && o.points != 0.5).collect();
    let favourite_won = decided.iter().filter(|o| (o.estimate > 0.5) == (o.points > 0.5)).count();
    let brier = outcomes.iter().map(|o| (o.estimate - o.points).powi(2)).sum::<f64>() / outcomes.len() as f64;
    Calibration { matches: outcomes.len(), decided: decided.len(), favourite_won, brier: Some(brier), slope, map_rounds: BTreeMap::new() }
}

pub fn predict(players: &[Player], mode: &ModeInfo, live: &LiveInput, calibration: &Calibration) -> Prediction {
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
    let strengths: HashMap<&str, Strength> = team.iter().chain(&enemy).map(|p| (p.steam_id.as_str(), strength(p, live))).collect();
    let is_known = |p: &&&Player| strengths.get(p.steam_id.as_str()).is_some_and(|s| s.level.is_some() || s.form.is_some());
    let (team_known, enemy_known) = (team.iter().filter(is_known).count(), enemy.iter().filter(is_known).count());
    result.team_rated = Some(team_known);
    result.enemy_rated = Some(enemy_known);
    let min_known = MIN_KNOWN_PER_TEAM.min(if size > 0 { size } else { MIN_KNOWN_PER_TEAM });
    if team_known < min_known || enemy_known < min_known {
        result.status = "not-enough-data";
        return result;
    }

    // The lobby's level: what matchmaking says an unknown player is, and an empty slot.
    let levels: Vec<f64> = strengths.values().filter_map(|s| s.level.map(|(value, _)| value)).collect();
    let lobby = if levels.is_empty() { 0.0 } else { levels.iter().sum::<f64>() / levels.len() as f64 };
    let rules = rules(&live.mode);
    let played = live.ours.unwrap_or(0) + live.theirs.unwrap_or(0);
    let is_inferred = |p: &Player| inferred.iter().any(|i| i.steam_id == p.steam_id);
    let player_value = |p: &Player, with_live: bool| {
        let s = &strengths[p.steam_id.as_str()];
        let level = s.level.map(|(value, weight)| lobby + (value - lobby) * weight / (weight + LEVEL_SHRINK)).unwrap_or(lobby);
        let mut value = level + FORM_WEIGHT * s.form.unwrap_or(0.0);
        if with_live && p.is_self && rules.is_some() && played >= 3 {
            if let (Some(kills), Some(deaths)) = (live.kills, live.deaths) {
                let now = level + FORM_WEIGHT * ((kills as f64 / deaths.max(1) as f64 - 1.0) / 0.25).clamp(-3.0, 3.0);
                let share = played as f64 / (played as f64 + LIVE_HALF_ROUNDS);
                value = value * (1.0 - share) + now * share;
            }
        }
        lobby + (value - lobby) * certainty(p, is_inferred(p))
    };
    // A team is its full size: places nobody is known for count as the lobby average.
    let slots = |side: &[&Player]| side.len().max(size);
    let average = |side: &[&Player], with_live: bool| {
        let open = slots(side) - side.len();
        (side.iter().map(|p| player_value(p, with_live)).sum::<f64>() + open as f64 * lobby) / slots(side) as f64
    };
    let gap = average(&team, false) - average(&enemy, false);
    let chance = |gap: f64| logistic(calibration.slope * gap).clamp(PRE_MATCH_BOUNDS.0, PRE_MATCH_BOUNDS.1);
    let pre_match = chance(gap);

    let all: Vec<&Player> = team.iter().chain(&enemy).copied().collect();
    let total_slots = (slots(&team) + slots(&enemy)) as f64;
    let covered = |test: &dyn Fn(&Strength) -> bool| {
        all.iter().filter(|p| test(&strengths[p.steam_id.as_str()])).map(|p| certainty(p, is_inferred(p))).sum::<f64>() / total_slots
    };
    let (coverage, level_coverage) = (covered(&|s| s.level.is_some() || s.form.is_some()), covered(&|s| s.level.is_some()));
    let confidence = if coverage >= 0.8 && level_coverage >= 0.6 {
        "high"
    } else if coverage >= 0.5 {
        "medium"
    } else {
        "low"
    };

    result.status = "ok";
    result.pre_match = Some(pre_match);
    result.gap = Some(gap);
    result.probability = Some(pre_match);
    result.confidence = Some(confidence);
    result.unconfirmed = Some(all.iter().filter(|p| certainty(p, is_inferred(p)) < 1.0).count());
    result.open_slots = Some(total_slots as usize - all.len());
    if let Some(rules) = rules {
        let (ours, theirs) = (live.ours.unwrap_or(0).max(0), live.theirs.unwrap_or(0).max(0));
        let (ct_rate, recorded_rounds) = calibration.ct_rate(&live.mode, &live.map);
        let game = Game::new(rules, live, ct_rate);
        // The stats (with your K/D this match) set the strength per round; the rounds so far move it.
        let prior = game.strength_for(chance(average(&team, true) - average(&enemy, true)));
        let (placed, unplaced, economy) = game.history(live, ours, theirs);
        let strength = game.updated_strength(prior, strength_spread(confidence), &placed, unplaced);
        let (won, tied) = game.finish(strength, ours, theirs, economy);
        result.probability = Some(won);
        result.tie = Some(tied).filter(|_| !rules.overtime);
        result.economy = economy.label();
        if live.ours.is_some() && live.theirs.is_some() {
            let rounds_known = ours + theirs == 0 || unplaced == (0, 0);
            result.live = Some(LiveView { ours, theirs, target: rules.target, rounds_known });
        }
        if !live.map.is_empty() {
            let name = |ct: bool| if ct { "CT" } else { "T" };
            let next = ours + theirs;
            let after_half = (game.reference < rules.half).then(|| game.side_of(rules.half)).flatten().map(name);
            result.side = Some(SideView { map: live.map.clone(), ct_rate, recorded_rounds, current: game.side_of(next).map(name), after_half });
        }
    }
    if calibration.matches > 0 {
        result.calibration = Some(calibration.clone());
    }

    let keys = LEVELS.iter().map(|l| l.key).chain(FORMS.iter().map(|f| f.key));
    result.factors = keys
        .filter_map(|key| {
            let values = |side: &[&Player]| side.iter().filter_map(|p| shown_value(p, key, live)).collect::<Vec<f64>>();
            let (ours, theirs) = (values(&team), values(&enemy));
            let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;
            (!ours.is_empty() && !theirs.is_empty()).then(|| Factor {
                key,
                team: mean(&ours),
                enemy: mean(&theirs),
                team_count: ours.len(),
                enemy_count: theirs.len(),
            })
        })
        .collect();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderResult;
    use crate::modes::mode_info;
    use serde_json::{json, Value};

    fn player(id: &str, side: &str, is_self: bool, data: &[(&str, &str, Value)]) -> Player {
        let mut player = Player {
            steam_id: id.into(),
            side: side.into(),
            is_self,
            side_source: if side.is_empty() { SideSource::None } else { SideSource::Manual },
            ..Player::default()
        };
        for (provider, key, value) in data {
            let entry = player.providers.entry(provider.to_string()).or_insert_with(|| ProviderResult::ok(serde_json::Map::new()));
            entry.data.insert(key.to_string(), value.clone());
        }
        player
    }

    pub fn rated(id: &str, premier: f64, side: &str, is_self: bool) -> Player {
        player(id, side, is_self, &[("leetify", "premier", json!(premier))])
    }

    fn lobby(team: f64, enemy: f64) -> Vec<Player> {
        let mut players: Vec<Player> = (0..5).map(|i| rated(&i.to_string(), team, "team", i == 0)).collect();
        players.extend((5..10).map(|i| rated(&i.to_string(), enemy, "enemy", false)));
        players
    }

    fn no_score(mode: &str) -> LiveInput {
        LiveInput { mode: mode.into(), ..LiveInput::default() }
    }

    fn run(players: &[Player], mode: &str) -> Prediction {
        predict(players, &mode_info(mode), &no_score(mode), &Calibration::default())
    }

    #[test]
    fn needs_teams_and_stats() {
        assert_eq!(run(&[rated("0", 15000.0, "team", true), rated("1", 15000.0, "", false)], "competitive").status, "needs-teams");
        let sparse = [rated("0", 15000.0, "team", true), rated("1", 15000.0, "team", false), rated("2", 0.0, "enemy", false)];
        assert_eq!(run(&sparse, "competitive").status, "not-enough-data", "a zero Premier is no rating");
    }

    #[test]
    fn stronger_teams_are_favoured_by_any_stat() {
        let even = run(&lobby(15000.0, 15000.0), "premier");
        assert!((even.probability.unwrap() - 0.5).abs() < 0.01, "even teams in Premier (no ties) are 50%");
        let ahead = run(&lobby(16000.0, 15000.0), "premier").pre_match.unwrap();
        assert!((0.6..0.68).contains(&ahead), "1,000 Premier ahead is a modest favourite: {ahead}");
        // Without Premier, FACEIT and K/D still say who is stronger.
        let stat = |id: &str, side: &str, elo: f64, kd: f64| player(id, side, id == "0", &[("faceit", "elo", json!(elo)), ("csstats", "kd", json!(kd))]);
        let mut players: Vec<Player> = (0..5).map(|i| stat(&i.to_string(), "team", 1200.0, 0.9)).collect();
        players.extend((5..10).map(|i| stat(&i.to_string(), "enemy", 1800.0, 1.2)));
        let behind = run(&players, "premier");
        assert!(behind.probability.unwrap() < 0.3, "{behind:?}");
        let keys: Vec<&str> = behind.factors.iter().map(|f| f.key).collect();
        assert_eq!(keys, ["faceitElo", "kd"]);
        assert_eq!((behind.factors[0].team, behind.factors[0].enemy), (1200.0, 1800.0));
    }

    #[test]
    fn form_shifts_a_player_from_their_level_rather_than_replacing_it() {
        // A 20,000 player with a modest K/D against an unrated player with a great one, in a 15,000 lobby.
        let mut players = lobby(15000.0, 15000.0);
        players[1] = player("1", "team", false, &[("leetify", "premier", json!(20000)), ("csstats", "kd", json!(0.9))]);
        players[6] = player("6", "enemy", false, &[("csstats", "kd", json!(1.3))]);
        let result = run(&players, "premier");
        assert!(result.pre_match.unwrap() > 0.5, "a known high level outweighs a good K/D at an unknown level: {result:?}");
        // The same K/D over 3 matches says less than over 300.
        let kd = |matches: f64| player("x", "enemy", false, &[("csstats", "kd", json!(1.5)), ("csstats", "matches", json!(matches))]);
        let (few, many) = (strength(&kd(3.0), &no_score("premier")).form.unwrap(), strength(&kd(300.0), &no_score("premier")).form.unwrap());
        assert!(few < many * 0.5, "{few} {many}");
    }

    #[test]
    fn competitive_uses_the_skill_group_on_this_map() {
        let ranked = |id: &str, side: &str, rank: f64| {
            player(id, side, id == "0", &[("leetify", "mapRanks", json!([{ "map": "de_nuke", "rank": rank }, { "map": "de_inferno", "rank": 3 }]))])
        };
        let mut players: Vec<Player> = (0..5).map(|i| ranked(&i.to_string(), "team", 15.0)).collect();
        players.extend((5..10).map(|i| ranked(&i.to_string(), "enemy", 11.0)));
        let live = LiveInput { mode: "competitive".into(), map: "de_nuke".into(), ..LiveInput::default() };
        let result = predict(&players, &mode_info("competitive"), &live, &Calibration::default());
        assert!(result.pre_match.unwrap() > 0.75, "{result:?}");
        assert_eq!(result.factors[0].key, "mapRank");
        let elsewhere = LiveInput { map: "de_dust2".into(), ..live };
        assert_eq!(predict(&players, &mode_info("competitive"), &elsewhere, &Calibration::default()).status, "not-enough-data", "no rank on this map");
    }

    #[test]
    fn missing_stats_are_left_out_not_zero() {
        let mut players = lobby(15000.0, 15000.0);
        // Two opponents have no data at all; they count as the lobby average, not as weak players.
        for p in players.iter_mut().filter(|p| ["8", "9"].contains(&p.steam_id.as_str())) {
            p.providers.clear();
        }
        let result = run(&players, "premier");
        assert!((result.probability.unwrap() - 0.5).abs() < 0.01, "{result:?}");
        assert_eq!((result.team_rated, result.enemy_rated), (Some(5), Some(3)));
        // A FACEIT level stands in for a missing Elo.
        let level = player("x", "team", false, &[("faceit", "level", json!(10))]);
        assert_eq!(level_value(&level, "faceitElo", &no_score("premier")), Some(2200.0));
    }

    #[test]
    fn missing_players_and_unconfirmed_sides_count_as_the_lobby_average() {
        // Two strong teammates known against a full, average enemy team: three of your places are unknown.
        let mut players = vec![rated("0", 17000.0, "team", true), rated("1", 17000.0, "team", false)];
        players.extend((5..10).map(|i| rated(&i.to_string(), 15000.0, "enemy", false)));
        let partial = run(&players, "premier");
        let whole = run(&lobby(17000.0, 15000.0), "premier");
        assert!(partial.pre_match.unwrap() < whole.pre_match.unwrap(), "two known players are not a whole team: {partial:?}");
        assert_eq!(partial.open_slots, Some(3));
        // Players only seen in Steam's recently played list, on an inferred side, count for less.
        let mut unsure = lobby(17000.0, 15000.0);
        for p in unsure.iter_mut().skip(1).take(4) {
            p.side_source = SideSource::Likely;
            p.confidence = "candidate".into();
        }
        let result = run(&unsure, "premier");
        assert!(result.pre_match.unwrap() < whole.pre_match.unwrap() && result.pre_match.unwrap() > 0.5, "{result:?}");
        assert_eq!(result.unconfirmed, Some(4));
    }

    fn score(mode: &str, map: &str, ours: i64, theirs: i64) -> LiveInput {
        LiveInput { mode: mode.into(), map: map.into(), ours: Some(ours), theirs: Some(theirs), ..LiveInput::default() }
    }

    /// The score with every round placed: `rounds` lists your wins ('W') and losses ('L') in order, and you
    /// started on `start` ("CT" or "T").
    fn rounds(mode: &str, map: &str, start: &str, rounds: &str) -> LiveInput {
        let half = half_length(mode).unwrap() as usize;
        let other = if start == "CT" { "T" } else { "CT" };
        let side_in = |i: usize| if i < half { start } else { other };
        let round_wins: Vec<String> = rounds
            .chars()
            .enumerate()
            .map(|(i, r)| {
                if r == 'W' {
                    side_in(i).to_string()
                } else if side_in(i) == "CT" {
                    "T".into()
                } else {
                    "CT".into()
                }
            })
            .collect();
        let ours = rounds.chars().filter(|r| *r == 'W').count() as i64;
        LiveInput { side: side_in(rounds.len()).into(), start_side: start.into(), round_wins, ..score(mode, map, ours, rounds.len() as i64 - ours) }
    }

    fn chance(players: &[Player], live: &LiveInput) -> f64 {
        predict(players, &mode_info(&live.mode), live, &Calibration::default()).probability.unwrap()
    }

    #[test]
    fn the_live_score_moves_the_estimate() {
        let players = lobby(15000.0, 15000.0);
        let at = |ours, theirs| predict(&players, &mode_info("competitive"), &score("competitive", "", ours, theirs), &Calibration::default());
        let start = at(0, 0);
        assert!(start.tie.unwrap() > 0.05 && start.tie.unwrap() < 0.25, "Competitive can tie at 12:12: {start:?}");
        let (leading, trailing) = (at(10, 3).probability.unwrap(), at(3, 10).probability.unwrap());
        assert!(leading > 0.9 && trailing < 0.1, "{leading} {trailing}");
        assert_eq!(at(12, 12).tie, Some(1.0));
        assert_eq!(at(13, 7).probability, Some(1.0));
        assert_eq!(start.live, Some(LiveView { ours: 0, theirs: 0, target: 13, rounds_known: true }));
        assert!(!at(5, 3).live.unwrap().rounds_known, "without CS2's round results the rounds cannot be placed");
        assert_eq!(at(9, 3).pre_match, start.pre_match, "the stats-only estimate ignores the score");
    }

    #[test]
    fn winning_rounds_is_evidence_of_strength() {
        // An underdog leading 8:2 is taken to be stronger than its stats said, so its chance is higher than
        // the lead alone gives with the pre-match strength.
        let players = lobby(14000.0, 16000.0);
        let live = score("competitive", "", 8, 2);
        let result = predict(&players, &mode_info("competitive"), &live, &Calibration::default());
        let game = Game::new(rules("competitive").unwrap(), &live, 0.5);
        let (lead_only, _) = game.finish(game.strength_for(result.pre_match.unwrap()), 8, 2, Economy::Unknown);
        assert!(result.probability.unwrap() > lead_only + 0.02, "{result:?} {lead_only}");
    }

    #[test]
    fn a_pistol_round_and_its_follow_ups_say_little() {
        // A 23% underdog that wins the pistol round and the two rounds after it: those two were expected
        // (rifles against pistols), so the lead is worth less than three independent rounds would be (58%).
        let players = lobby(14000.0, 16000.0);
        let cascade = chance(&players, &rounds("premier", "de_cache", "CT", "WWW"));
        assert!(cascade < 0.52, "{cascade}");
        // Three rounds won against full buys say more than three won on the back of a pistol round.
        let late = chance(&players, &rounds("premier", "de_cache", "CT", "LLLWWW"));
        let early = chance(&players, &rounds("premier", "de_cache", "CT", "WWWLLL"));
        assert!(late > early + 0.03, "{late} {early}");
        // The economy for the next round is reported, from the round results or, early on, the score.
        let economy = |live: &LiveInput| predict(&players, &mode_info("premier"), live, &Calibration::default()).economy;
        assert_eq!(economy(&rounds("premier", "de_cache", "CT", "W")), Some("pistol-won"));
        assert_eq!(economy(&rounds("premier", "de_cache", "CT", "LL")), Some("converted-lost"));
        assert_eq!(economy(&rounds("premier", "de_cache", "CT", "WLW")), None);
        assert_eq!(economy(&score("premier", "", 0, 1)), Some("pistol-lost"));
        assert_eq!(economy(&score("premier", "", 12, 0)), None, "the second half starts with a pistol round again");
        // Winning the pistol round makes the next round likely, so the match chance jumps more than a round.
        let (won_pistol, neutral) = (chance(&players, &rounds("premier", "de_cache", "CT", "W")), chance(&players, &score("premier", "", 0, 0)));
        assert!(won_pistol > neutral + 0.08, "{won_pistol} {neutral}");
    }

    #[test]
    fn the_map_side_still_to_play_counts() {
        let players = lobby(15000.0, 15000.0);
        // 4:8 at half-time. On T-sided Anubis, starting CT means the strong T half is still to come (and the
        // CT rounds lost said less about strength); starting T means you lost on your best side.
        let at_half = |start: &str| {
            let history = if start == "CT" { "WLLWLLWLLWLL" } else { "LLWLLWLLWLLW" };
            // Between rounds CS2 still shows the side of the round just played.
            LiveInput { round_over: true, side: start.into(), ..rounds("premier", "de_anubis", start, history) }
        };
        let (weak_side_first, strong_side_first) = (chance(&players, &at_half("CT")), chance(&players, &at_half("T")));
        assert!(weak_side_first > strong_side_first + 0.03, "{weak_side_first} {strong_side_first}");
        // The same 4:8 on CT-sided Nuke after starting CT: the weak T half is still to come.
        let nuke = chance(&players, &LiveInput { map: "de_nuke".into(), ..at_half("CT") });
        assert!(weak_side_first > nuke + 0.05, "{weak_side_first} {nuke}");
        // Before a round is played the sides even out: each team plays both.
        let start = |side: &str| chance(&players, &LiveInput { side: side.into(), start_side: side.into(), ..score("premier", "de_nuke", 0, 0) });
        assert!((start("CT") - start("T")).abs() < 0.02 && (start("CT") - 0.5).abs() < 0.01);
        let side = predict(&players, &mode_info("premier"), &rounds("premier", "de_nuke", "T", "WL"), &Calibration::default()).side.unwrap();
        assert_eq!((side.current, side.after_half, side.recorded_rounds), (Some("T"), Some("CT"), 0));
        assert!((side.ct_rate - 0.55).abs() < 1e-9);
    }

    #[test]
    fn recorded_rounds_move_a_maps_side_figure() {
        let mut calibration = Calibration::default();
        calibration.map_rounds.insert(map_key("premier", "de_nuke"), MapRounds { ct: 300, rounds: 1000 });
        let (rate, recorded) = calibration.ct_rate("competitive", "de_nuke");
        assert!(rate < 0.4 && recorded == 1000, "1,000 recorded rounds outweigh the rough figure: {rate}");
        assert_eq!(calibration.ct_rate("scrimcomp2v2", "de_nuke"), (0.5, 0), "Wingman is counted on its own");
        assert_eq!(calibration.ct_rate("premier", "de_unknown"), (0.5, 0));
    }

    #[test]
    fn premier_goes_to_overtime() {
        let players = lobby(15000.0, 15000.0);
        let at = |ours, theirs| predict(&players, &mode_info("premier"), &score("premier", "", ours, theirs), &Calibration::default());
        let tied = at(12, 12);
        assert!((tied.probability.unwrap() - 0.5).abs() < 0.01 && tied.tie.is_none(), "{tied:?}");
        assert!(at(15, 14).probability.unwrap() > 0.5, "overtime counts on from 12:12");
        assert!((at(15, 15).probability.unwrap() - 0.5).abs() < 0.01, "a second overtime starts even");
        let live = LiveInput { side: "CT".into(), start_side: "CT".into(), ..score("premier", "de_nuke", 0, 0) };
        let game = Game::new(rules("premier").unwrap(), &live, 0.55);
        let (won, _) = game.finish(game.strength_for(0.7), 0, 0, Economy::Reset);
        assert!((won - 0.7).abs() < 0.001, "the per-round strength reproduces the pre-match estimate: {won}");
    }

    #[test]
    fn your_own_kills_and_deaths_count_as_rounds_pass() {
        let players = lobby(15000.0, 15000.0);
        let live = |kills, deaths| LiveInput {
            mode: "competitive".into(),
            ours: Some(5),
            theirs: Some(5),
            kills: Some(kills),
            deaths: Some(deaths),
            ..LiveInput::default()
        };
        let at = |kills, deaths| predict(&players, &mode_info("competitive"), &live(kills, deaths), &Calibration::default());
        let (hot, cold) = (at(20, 4), at(2, 9));
        assert!(hot.probability.unwrap() > cold.probability.unwrap() + 0.05, "{hot:?} {cold:?}");
        assert_eq!(hot.pre_match, cold.pre_match, "the stats-only estimate leaves this match out");
    }

    #[test]
    fn calibration_refits_the_slope_from_results() {
        assert_eq!(calibrate(&[]), Calibration::default());
        // Favourites that win far more often than the estimate said: the slope rises.
        let strong: Vec<Outcome> = (0..200).map(|i| Outcome { estimate: 0.6, gap: 0.12, points: if i % 10 < 9 { 1.0 } else { 0.0 } }).collect();
        let fitted = calibrate(&strong);
        assert!(fitted.slope > SLOPE + 2.0, "{fitted:?}");
        assert_eq!((fitted.decided, fitted.favourite_won), (200, 180));
        // A coin flip: the slope falls, and the Brier score is worse than 0.25.
        let random: Vec<Outcome> = (0..200).map(|i| Outcome { estimate: 0.7, gap: 0.25, points: (i % 2) as f64 }).collect();
        let flat = calibrate(&random);
        assert!(flat.slope < SLOPE - 1.0 && flat.brier.unwrap() > 0.25, "{flat:?}");
        // A handful of matches barely moves it.
        assert!((calibrate(&strong[..5]).slope - SLOPE).abs() < 0.5);
        // The fitted slope is used.
        let steeper = Calibration { slope: SLOPE * 2.0, matches: 50, ..Calibration::default() };
        let base = run(&lobby(16000.0, 15000.0), "premier");
        let refit = predict(&lobby(16000.0, 15000.0), &mode_info("premier"), &no_score("premier"), &steeper);
        assert!(refit.pre_match.unwrap() > base.pre_match.unwrap() && refit.calibration.is_some() && base.calibration.is_none());
    }

    #[test]
    fn infers_opponents_only_when_exactly_the_missing_players_remain() {
        let mut players: Vec<Player> = (0..5).map(|i| rated(&i.to_string(), 16000.0, "team", i == 0)).collect();
        players.extend((5..10).map(|i| rated(&i.to_string(), 15000.0, "", false)));
        let result = run(&players, "competitive");
        assert_eq!(result.status, "ok");
        assert_eq!(result.inferred_enemies, ["5", "6", "7", "8", "9"]);
        players.push(rated("10", 15000.0, "", false));
        assert!(run(&players, "competitive").inferred_enemies.is_empty(), "six unassigned players cannot all be opponents");
    }

    #[test]
    fn rush_infers_the_three_opponents() {
        let mut players: Vec<Player> = (0..3).map(|i| rated(&i.to_string(), 12000.0, "team", i == 0)).collect();
        players.extend((3..6).map(|i| rated(&i.to_string(), 11000.0, "", false)));
        let result = run(&players, "rush");
        assert_eq!(result.inferred_enemies, ["3", "4", "5"]);
        assert_eq!(result.status, "ok");
        assert!(result.live.is_none() && result.tie.is_none(), "rush has no known round rules");
    }

    #[test]
    fn free_for_all_and_wingman() {
        assert_eq!(run(&[rated("0", 1.0, "team", true)], "deathmatch").status, "ffa");
        let wingman = [rated("0", 9000.0, "", true), rated("a", 9000.0, "team", false), rated("b", 8000.0, "", false), rated("c", 8000.0, "", false)];
        let result = run(&wingman, "scrimcomp2v2");
        assert_eq!(result.inferred_enemies, ["b", "c"]);
        assert_eq!(result.status, "ok");
        assert!(result.tie.unwrap() > 0.0, "Wingman can tie at 8:8");
        // The Wingman rank is Wingman's own matchmaking rating.
        let duo = |id: &str, side: &str, rank: f64| player(id, side, id == "0", &[("leetify", "wingman", json!(rank))]);
        let ranked = [duo("0", "team", 16.0), duo("a", "team", 16.0), duo("b", "enemy", 8.0), duo("c", "enemy", 8.0)];
        assert!(run(&ranked, "scrimcomp2v2").pre_match.unwrap() > 0.8);
    }
}
