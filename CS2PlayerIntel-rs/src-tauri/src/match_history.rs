//! The app's own record of finished matches, in a local SQLite database (history.sqlite in the app data
//! folder): who was in each match, the sides you marked, your result, values measured from Steam, CSRep,
//! CSStats and FACEIT at that time (each with its source time), and your notes on players: each note
//! keeps when it was written, the map and mode, whether the player was with or against you, and the
//! match it belongs to.
//!
//! Leetify asks apps not to store its data, so no Leetify value is ever written here; Leetify history is
//! shown live from Leetify instead. A snapshot is never updated: it records what was measured when.

use crate::gsi::GsiSummary;
use crate::model::{is_steam_id, now_ms};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS matches (
    id INTEGER PRIMARY KEY,
    ended_at INTEGER NOT NULL,
    map TEXT NOT NULL DEFAULT '',
    mode TEXT NOT NULL DEFAULT '',
    result TEXT NOT NULL DEFAULT 'unknown',
    self_id TEXT NOT NULL,
    started_at INTEGER,
    kills INTEGER,
    deaths INTEGER,
    assists INTEGER,
    mvps INTEGER,
    score INTEGER
);
CREATE TABLE IF NOT EXISTS encounters (
    match_id INTEGER NOT NULL REFERENCES matches(id) ON DELETE CASCADE,
    steam_id TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    side TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (match_id, steam_id)
);
CREATE INDEX IF NOT EXISTS encounters_by_player ON encounters(steam_id);
CREATE TABLE IF NOT EXISTS snapshots (
    match_id INTEGER NOT NULL REFERENCES matches(id) ON DELETE CASCADE,
    steam_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    metric TEXT NOT NULL,
    value REAL NOT NULL,
    measured_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS snapshots_by_player ON snapshots(steam_id, metric, measured_at);
CREATE TABLE IF NOT EXISTS player_notes (
    id INTEGER PRIMARY KEY,
    steam_id TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    map TEXT NOT NULL DEFAULT '',
    mode TEXT NOT NULL DEFAULT '',
    side TEXT NOT NULL DEFAULT '',
    match_started_at INTEGER,
    match_id INTEGER REFERENCES matches(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS player_notes_by_player ON player_notes(steam_id, created_at);
";

/// Longest note accepted.
const MAX_NOTE_CHARS: usize = 2000;

/// Your own scoreboard line for a match, as CS2 reported it at the end (GSI). Your own game data, so it
/// is kept; it shows how you do on each map over time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OwnMatchStats {
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    pub mvps: Option<i64>,
    pub score: Option<i64>,
}

/// Where a note is written: the match in progress (or the one just finished), or a recorded match.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NoteContext {
    pub map: String,
    pub mode: String,
    /// The player's side relative to you: "team", "enemy" or "".
    pub side: String,
    /// Start time of the live match, which links the note once the match is recorded.
    pub match_started_at: Option<i64>,
    /// A recorded match, when the note is written from the History page.
    pub match_id: Option<i64>,
}
/// One of your matches for the per-map view: map, result, end time, kills, deaths, assists, MVPs.
type OwnRow = (String, String, i64, Option<i64>, Option<i64>, Option<i64>, Option<i64>);
/// Per-map totals while counting: map, matches, results, [kills, deaths, assists, MVPs], matches with stats.
type MapTotals = (String, Vec<Value>, Tally, [i64; 4], i64);
/// One metric's distinct measurements: provider, metric, (value, measured at) oldest first.
type Series = (String, String, Vec<(f64, i64)>);

/// Matches kept at most; the oldest go first.
const MAX_MATCHES: i64 = 5000;

/// One value as a provider reported it when the match ended.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub provider: String,
    pub metric: String,
    pub value: f64,
    /// When the provider's data was fetched (not when the match ended).
    pub measured_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPlayer {
    pub steam_id: String,
    #[serde(default)]
    pub side: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub snapshots: Vec<Snapshot>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Tally {
    pub played: u32,
    pub won: u32,
    pub lost: u32,
    pub tied: u32,
    pub unknown: u32,
}

impl Tally {
    fn add(&mut self, result: &str) {
        self.played += 1;
        match result {
            "win" => self.won += 1,
            "loss" => self.lost += 1,
            "tie" => self.tied += 1,
            _ => self.unknown += 1,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistorySummary {
    pub all: Tally,
    pub together: Tally,
    pub against: Tally,
    pub first_played_at: Option<i64>,
    pub last_played_at: Option<i64>,
}

pub struct MatchHistoryStore {
    file: PathBuf,
    conn: Connection,
    /// Set when the database could not be opened; history then lives in memory for this session.
    pub load_error: String,
    /// Encounter summaries per account (your SteamID), rebuilt after every write.
    summaries: HashMap<String, HashMap<String, HistorySummary>>,
}

fn open_connection(file: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(file)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA)?;
    upgrade(&conn)?;
    Ok(conn)
}

/// Brings a database made with an older layout up to date: a match start time and your own stats on each
/// match, and the
/// single note per player (`notes` table) turned into dated notes.
fn upgrade(conn: &Connection) -> rusqlite::Result<()> {
    let has_started_at: bool = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('matches') WHERE name = 'started_at'", [], |row| row.get(0))?;
    if !has_started_at {
        conn.execute_batch("ALTER TABLE matches ADD COLUMN started_at INTEGER;")?;
    }
    for column in ["kills", "deaths", "assists", "mvps", "score"] {
        let present: bool = conn.query_row(&format!("SELECT COUNT(*) FROM pragma_table_info('matches') WHERE name = '{column}'"), [], |row| row.get(0))?;
        if !present {
            conn.execute_batch(&format!("ALTER TABLE matches ADD COLUMN {column} INTEGER;"))?;
        }
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS matches_by_start ON matches(started_at);")?;
    let has_old_notes: bool = conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'notes'", [], |row| row.get(0))?;
    if has_old_notes {
        conn.execute_batch(
            "BEGIN; INSERT INTO player_notes (steam_id, text, created_at) SELECT steam_id, text, updated_at FROM notes; DROP TABLE notes; COMMIT;",
        )?;
    }
    Ok(())
}

impl MatchHistoryStore {
    /// Opens history.sqlite in `directory`.
    pub fn open(directory: &Path) -> Self {
        let file = directory.join("history.sqlite");
        let _ = std::fs::create_dir_all(directory);
        let (conn, load_error) = match open_connection(&file) {
            Ok(conn) => (conn, String::new()),
            Err(error) => {
                let conn = Connection::open_in_memory().and_then(|c| c.execute_batch(SCHEMA).map(|_| c)).expect("an in-memory SQLite database opens");
                (conn, format!("Match history could not be opened ({error}); this session's matches are kept in memory only."))
            }
        };
        Self { file, conn, load_error, summaries: HashMap::new() }
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(
        &mut self,
        started_at: Option<i64>,
        own: &OwnMatchStats,
        ended_at: i64,
        map: &str,
        mode: &str,
        result: &str,
        self_id: &str,
        players: &[HistoryPlayer],
    ) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO matches (ended_at, map, mode, result, self_id, started_at, kills, deaths, assists, mvps, score) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![ended_at, map, mode, result, self_id, started_at, own.kills, own.deaths, own.assists, own.mvps, own.score],
        )?;
        let match_id = tx.last_insert_rowid();
        if let Some(started_at) = started_at {
            // Notes written while this match was running now belong to it.
            tx.execute("UPDATE player_notes SET match_id = ?1 WHERE match_id IS NULL AND match_started_at = ?2", params![match_id, started_at])?;
        }
        for player in players {
            tx.execute(
                "INSERT OR IGNORE INTO encounters (match_id, steam_id, name, side) VALUES (?1, ?2, ?3, ?4)",
                params![match_id, player.steam_id, player.name, player.side],
            )?;
            for s in &player.snapshots {
                tx.execute(
                    "INSERT INTO snapshots (match_id, steam_id, provider, metric, value, measured_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![match_id, player.steam_id, s.provider, s.metric, s.value, s.measured_at],
                )?;
            }
        }
        tx.execute("DELETE FROM matches WHERE id NOT IN (SELECT id FROM matches ORDER BY ended_at DESC LIMIT ?1)", params![MAX_MATCHES])?;
        tx.commit()
    }

    /// Records a finished match; false when there is nothing worth keeping.
    #[cfg(test)]
    pub fn record(&mut self, map: &str, mode: &str, result: &str, self_id: &str, players: Vec<HistoryPlayer>) -> bool {
        self.record_match(None, &OwnMatchStats::default(), map, mode, result, self_id, players)
    }

    /// Records a finished match that started at `started_at`, linking the notes written during it.
    #[allow(clippy::too_many_arguments)]
    pub fn record_match(
        &mut self,
        started_at: Option<i64>,
        own: &OwnMatchStats,
        map: &str,
        mode: &str,
        result: &str,
        self_id: &str,
        players: Vec<HistoryPlayer>,
    ) -> bool {
        let others: Vec<HistoryPlayer> = players.into_iter().filter(|p| is_steam_id(&p.steam_id) && p.steam_id != self_id).collect();
        if self_id.is_empty() || others.is_empty() {
            return false;
        }
        // History is a convenience; the match itself is unaffected when it cannot be saved.
        let saved = self.insert(started_at, own, now_ms(), map, mode, result, self_id, &others).is_ok();
        self.summaries.clear();
        saved
    }

    fn summaries(&mut self, self_id: &str) -> &HashMap<String, HistorySummary> {
        if !self.summaries.contains_key(self_id) {
            let mut map: HashMap<String, HistorySummary> = HashMap::new();
            if let Ok(mut statement) =
                self.conn.prepare("SELECT e.steam_id, e.side, m.result, m.ended_at FROM encounters e JOIN matches m ON m.id = e.match_id WHERE m.self_id = ?1")
            {
                let rows = statement.query_map(params![self_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?))
                });
                for (steam_id, side, result, ended_at) in rows.into_iter().flatten().flatten() {
                    let summary = map.entry(steam_id).or_default();
                    summary.all.add(&result);
                    match side.as_str() {
                        "team" => summary.together.add(&result),
                        "enemy" => summary.against.add(&result),
                        _ => {}
                    }
                    summary.first_played_at = Some(summary.first_played_at.map_or(ended_at, |first| first.min(ended_at)));
                    summary.last_played_at = Some(summary.last_played_at.map_or(ended_at, |last| last.max(ended_at)));
                }
            }
            self.summaries.insert(self_id.to_string(), map);
        }
        &self.summaries[self_id]
    }

    pub fn summary_for(&mut self, self_id: &str, steam_id: &str) -> HistorySummary {
        self.summaries(self_id).get(steam_id).cloned().unwrap_or_default()
    }

    /// Recorded matches of one account (all accounts when `self_id` is empty), newest first, a page at a time.
    pub fn matches(&self, self_id: &str, limit: usize, offset: usize) -> Value {
        let total: i64 = self.conn.query_row("SELECT COUNT(*) FROM matches WHERE ?1 = '' OR self_id = ?1", params![self_id], |row| row.get(0)).unwrap_or(0);
        let matches: Vec<Value> = self
            .conn
            .prepare(
                "SELECT m.id, m.ended_at, m.map, m.mode, m.result, \
                        COALESCE(SUM(e.side = 'team'), 0), COALESCE(SUM(e.side = 'enemy'), 0), COUNT(e.steam_id) \
                 FROM matches m LEFT JOIN encounters e ON e.match_id = m.id \
                 WHERE ?1 = '' OR m.self_id = ?1 GROUP BY m.id ORDER BY m.ended_at DESC, m.id DESC LIMIT ?2 OFFSET ?3",
            )
            .and_then(|mut statement| {
                statement
                    .query_map(params![self_id, limit as i64, offset as i64], |row| {
                        Ok(json!({
                            "id": row.get::<_, i64>(0)?,
                            "endedAt": row.get::<_, i64>(1)?,
                            "map": row.get::<_, String>(2)?,
                            "mode": row.get::<_, String>(3)?,
                            "result": row.get::<_, String>(4)?,
                            "team": row.get::<_, i64>(5)?,
                            "enemy": row.get::<_, i64>(6)?,
                            "players": row.get::<_, i64>(7)?,
                        }))
                    })?
                    .collect()
            })
            .unwrap_or_default();
        json!({ "total": total, "matches": matches })
    }

    /// Your own results per map from the stats recorded with each match: matches, wins, losses, ties, totals,
    /// and each match oldest first (for the trend). Matches recorded without your stats count for results only.
    pub fn own_performance(&self, self_id: &str) -> Value {
        let rows: Vec<OwnRow> = self
            .conn
            .prepare("SELECT map, result, ended_at, kills, deaths, assists, mvps FROM matches WHERE self_id = ?1 AND map != '' ORDER BY ended_at, id")
            .and_then(|mut statement| {
                statement
                    .query_map(params![self_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)))?
                    .collect()
            })
            .unwrap_or_default();
        let mut maps: Vec<MapTotals> = Vec::new();
        for (map, result, ended_at, kills, deaths, assists, mvps) in rows {
            let index = match maps.iter().position(|m| m.0 == map) {
                Some(i) => i,
                None => {
                    maps.push((map.clone(), Vec::new(), Tally::default(), [0; 4], 0));
                    maps.len() - 1
                }
            };
            let entry = &mut maps[index];
            entry.2.add(&result);
            if let (Some(k), Some(d)) = (kills, deaths) {
                entry.3[0] += k;
                entry.3[1] += d;
                entry.3[2] += assists.unwrap_or(0);
                entry.3[3] += mvps.unwrap_or(0);
                entry.4 += 1;
            }
            entry.1.push(json!({ "endedAt": ended_at, "result": result, "kills": kills, "deaths": deaths, "assists": assists }));
        }
        maps.sort_by(|a, b| b.2.played.cmp(&a.2.played).then(a.0.cmp(&b.0)));
        let maps: Vec<Value> = maps
            .into_iter()
            .map(|(map, matches, tally, [kills, deaths, assists, mvps], with_stats)| {
                json!({
                    "map": map,
                    "played": tally.played, "won": tally.won, "lost": tally.lost, "tied": tally.tied,
                    "withStats": with_stats, "kills": kills, "deaths": deaths, "assists": assists, "mvps": mvps,
                    "matches": matches,
                })
            })
            .collect();
        json!({ "maps": maps })
    }

    /// One recorded match: its players with their side, the values measured for them at the time, how often
    /// you have met them in all, and whether you wrote a note about them.
    pub fn match_details(&self, id: i64) -> Option<Value> {
        let (ended_at, map, mode, result, self_id): (i64, String, String, String, String) = self
            .conn
            .query_row("SELECT ended_at, map, mode, result, self_id FROM matches WHERE id = ?1", params![id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })
            .ok()?;
        let mut values: HashMap<String, serde_json::Map<String, Value>> = HashMap::new();
        if let Ok(mut statement) = self.conn.prepare("SELECT steam_id, provider, metric, value FROM snapshots WHERE match_id = ?1") {
            let rows = statement
                .query_map(params![id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, f64>(3)?)));
            for (steam_id, provider, metric, value) in rows.into_iter().flatten().flatten() {
                values.entry(steam_id).or_default().insert(format!("{provider}.{metric}"), json!(value));
            }
        }
        let players: Vec<Value> = self
            .conn
            .prepare(
                "SELECT e.steam_id, e.name, e.side, \
                        (SELECT COUNT(*) FROM encounters x JOIN matches o ON o.id = x.match_id WHERE x.steam_id = e.steam_id AND o.self_id = ?2), \
                        EXISTS(SELECT 1 FROM player_notes n WHERE n.steam_id = e.steam_id) \
                 FROM encounters e WHERE e.match_id = ?1 ORDER BY e.side = 'team' DESC, e.side = 'enemy' DESC, e.name COLLATE NOCASE",
            )
            .and_then(|mut statement| {
                statement
                    .query_map(params![id, self_id], |row| {
                        let steam_id: String = row.get(0)?;
                        Ok(json!({
                            "steamId": steam_id,
                            "name": row.get::<_, String>(1)?,
                            "side": row.get::<_, String>(2)?,
                            "met": row.get::<_, i64>(3)?,
                            "hasNote": row.get::<_, bool>(4)?,
                            "values": values.get(&steam_id).cloned().unwrap_or_default(),
                            "notes": [],
                        }))
                    })?
                    .collect()
            })
            .unwrap_or_default();
        let mut players = players;
        for player in players.iter_mut() {
            let steam_id = player["steamId"].as_str().unwrap_or_default().to_string();
            player["notes"] = Value::Array(self.notes_for(&steam_id).into_iter().filter(|n| n["matchId"] == id).collect());
        }
        Some(json!({ "id": id, "endedAt": ended_at, "map": map, "mode": mode, "result": result, "selfId": self_id, "players": players }))
    }

    /// Everything recorded about one player: encounters, measured values over time, and your note.
    pub fn player_history(&mut self, self_id: &str, steam_id: &str) -> Value {
        let summary = self.summary_for(self_id, steam_id);
        let encounters: Vec<Value> = self
            .conn
            .prepare("SELECT m.ended_at, m.map, m.mode, m.result, e.side, e.name FROM encounters e JOIN matches m ON m.id = e.match_id WHERE m.self_id = ?1 AND e.steam_id = ?2 ORDER BY m.ended_at DESC, m.id DESC LIMIT 200")
            .and_then(|mut statement| {
                statement
                    .query_map(params![self_id, steam_id], |row| {
                        Ok(json!({ "endedAt": row.get::<_, i64>(0)?, "map": row.get::<_, String>(1)?, "mode": row.get::<_, String>(2)?, "result": row.get::<_, String>(3)?, "side": row.get::<_, String>(4)?, "name": row.get::<_, String>(5)? }))
                    })?
                    .collect()
            })
            .unwrap_or_default();
        // First, previous and latest measurement of each metric.
        let mut series: Vec<Series> = Vec::new();
        if let Ok(mut statement) =
            self.conn.prepare("SELECT provider, metric, value, measured_at FROM snapshots WHERE steam_id = ?1 ORDER BY provider, metric, measured_at")
        {
            let rows = statement
                .query_map(params![steam_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, f64>(2)?, row.get::<_, i64>(3)?)));
            for (provider, metric, value, at) in rows.into_iter().flatten().flatten() {
                match series.last_mut() {
                    Some((p, m, points)) if *p == provider && *m == metric => {
                        // The same fetched value is recorded again at every match; keep distinct measurements.
                        if points.last().is_none_or(|(_, last_at)| *last_at != at) {
                            points.push((value, at));
                        }
                    }
                    _ => series.push((provider, metric, vec![(value, at)])),
                }
            }
        }
        let point = |p: Option<&(f64, i64)>| p.map(|(value, at)| json!({ "value": value, "measuredAt": at })).unwrap_or(Value::Null);
        let progression: Vec<Value> = series
            .iter()
            .map(|(provider, metric, points)| {
                json!({
                    "provider": provider,
                    "metric": metric,
                    "first": point(points.first()),
                    "previous": point(points.len().checked_sub(2).and_then(|i| points.get(i))),
                    "current": point(points.last()),
                    "measurements": points.len(),
                })
            })
            .collect();
        json!({ "summary": summary, "encounters": encounters, "progression": progression, "notes": self.notes_for(steam_id) })
    }

    /// Adds a note on a player, stamped with the time and where it was written.
    pub fn add_note(&mut self, steam_id: &str, text: &str, context: &NoteContext) -> Result<Value, String> {
        let text = text.trim();
        if !is_steam_id(steam_id) {
            return Err("Unknown player.".into());
        }
        if text.is_empty() {
            return Err("The note is empty.".into());
        }
        if text.chars().count() > MAX_NOTE_CHARS {
            return Err("Notes are limited to 2,000 characters.".into());
        }
        // A note written after its match was recorded joins that match straight away.
        let match_id = context.match_id.or_else(|| {
            let started_at = context.match_started_at?;
            self.conn.query_row("SELECT id FROM matches WHERE started_at = ?1 ORDER BY id DESC LIMIT 1", params![started_at], |row| row.get(0)).ok()
        });
        self.conn
            .execute(
                "INSERT INTO player_notes (steam_id, text, created_at, map, mode, side, match_started_at, match_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![steam_id, text, now_ms(), context.map, context.mode, context.side, context.match_started_at, match_id],
            )
            .map_err(|e| e.to_string())?;
        let id = self.conn.last_insert_rowid();
        Ok(self.notes_for(steam_id).into_iter().find(|n| n["id"] == id).unwrap_or(Value::Null))
    }

    /// Where a note about `steam_id` written from a recorded match belongs.
    pub fn note_context_for_match(&self, match_id: i64, steam_id: &str) -> Option<NoteContext> {
        self.conn
            .query_row(
                "SELECT m.map, m.mode, COALESCE(e.side, '') FROM matches m LEFT JOIN encounters e ON e.match_id = m.id AND e.steam_id = ?2 WHERE m.id = ?1",
                params![match_id, steam_id],
                |row| Ok(NoteContext { map: row.get(0)?, mode: row.get(1)?, side: row.get(2)?, match_started_at: None, match_id: Some(match_id) }),
            )
            .ok()
    }

    pub fn delete_note(&mut self, id: i64) -> Result<(), String> {
        self.conn.execute("DELETE FROM player_notes WHERE id = ?1", params![id]).map(|_| ()).map_err(|e| e.to_string())
    }

    /// A player's notes, newest first, each with the match it was written in (and that match's result).
    pub fn notes_for(&self, steam_id: &str) -> Vec<Value> {
        self.conn
            .prepare(
                "SELECT n.id, n.text, n.created_at, n.map, n.mode, n.side, n.match_id, m.result \
                 FROM player_notes n LEFT JOIN matches m ON m.id = n.match_id \
                 WHERE n.steam_id = ?1 ORDER BY n.created_at DESC, n.id DESC",
            )
            .and_then(|mut statement| {
                statement
                    .query_map(params![steam_id], |row| {
                        Ok(json!({
                            "id": row.get::<_, i64>(0)?,
                            "text": row.get::<_, String>(1)?,
                            "createdAt": row.get::<_, i64>(2)?,
                            "map": row.get::<_, String>(3)?,
                            "mode": row.get::<_, String>(4)?,
                            "side": row.get::<_, String>(5)?,
                            "matchId": row.get::<_, Option<i64>>(6)?,
                            "result": row.get::<_, Option<String>>(7)?,
                        }))
                    })?
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The whole database as JSON, for export.
    pub fn export(&self) -> Value {
        let table = |sql: &str, columns: &[&str]| -> Vec<Value> {
            self.conn
                .prepare(sql)
                .and_then(|mut statement| {
                    statement
                        .query_map([], |row| {
                            let mut object = serde_json::Map::new();
                            for (i, column) in columns.iter().enumerate() {
                                let value: rusqlite::types::Value = row.get(i)?;
                                object.insert(
                                    column.to_string(),
                                    match value {
                                        rusqlite::types::Value::Integer(n) => n.into(),
                                        rusqlite::types::Value::Real(n) => n.into(),
                                        rusqlite::types::Value::Text(t) => t.into(),
                                        _ => Value::Null,
                                    },
                                );
                            }
                            Ok(Value::Object(object))
                        })?
                        .collect()
                })
                .unwrap_or_default()
        };
        json!({
            "format": "cs2-player-intel-history",
            "version": 2,
            "exportedAt": now_ms(),
            "matches": table(
                "SELECT id, ended_at, map, mode, result, self_id, started_at, kills, deaths, assists, mvps, score FROM matches ORDER BY ended_at",
                &["id", "endedAt", "map", "mode", "result", "selfId", "startedAt", "kills", "deaths", "assists", "mvps", "score"]
            ),
            "encounters": table("SELECT match_id, steam_id, name, side FROM encounters", &["matchId", "steamId", "name", "side"]),
            "snapshots": table("SELECT match_id, steam_id, provider, metric, value, measured_at FROM snapshots", &["matchId", "steamId", "provider", "metric", "value", "measuredAt"]),
            "notes": table(
                "SELECT steam_id, text, created_at, map, mode, side, match_started_at, match_id FROM player_notes ORDER BY created_at",
                &["steamId", "text", "createdAt", "map", "mode", "side", "matchStartedAt", "matchId"]
            ),
        })
    }

    /// Merges a file written by [`Self::export`]. A match already recorded (same account, end time and map)
    /// is skipped, and so is a note already present (same player, time and text), so importing the same file
    /// twice changes nothing. Exports with one note per player (format 1) are read too.
    pub fn import(&mut self, data: &Value) -> Result<Value, String> {
        if data["format"] != "cs2-player-intel-history" {
            return Err("This is not a CS2 Player Intel history export.".into());
        }
        if !matches!(data["version"].as_i64(), Some(1 | 2)) {
            return Err("This export comes from a newer version of the app; update first.".into());
        }
        let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let list = |key: &str| data[key].as_array().cloned().unwrap_or_default();
        let (mut added, mut skipped, mut notes) = (0, 0, 0);
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        let mut ids: HashMap<i64, i64> = HashMap::new();
        for m in list("matches") {
            let (Some(old_id), Some(ended_at)) = (m["id"].as_i64(), m["endedAt"].as_i64()) else { continue };
            let (self_id, map) = (text(&m["selfId"]), text(&m["map"]));
            if !is_steam_id(&self_id) {
                continue;
            }
            let existing: Option<i64> = tx
                .query_row("SELECT id FROM matches WHERE self_id = ?1 AND ended_at = ?2 AND map = ?3", params![self_id, ended_at, map], |row| row.get(0))
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some(existing) = existing {
                // Notes in the file can still point at it.
                ids.insert(old_id, existing);
                skipped += 1;
                continue;
            }
            tx.execute(
                "INSERT INTO matches (ended_at, map, mode, result, self_id, started_at, kills, deaths, assists, mvps, score) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    ended_at,
                    map,
                    text(&m["mode"]),
                    text(&m["result"]),
                    self_id,
                    m["startedAt"].as_i64(),
                    m["kills"].as_i64(),
                    m["deaths"].as_i64(),
                    m["assists"].as_i64(),
                    m["mvps"].as_i64(),
                    m["score"].as_i64()
                ],
            )
            .map_err(|e| e.to_string())?;
            ids.insert(old_id, tx.last_insert_rowid());
            added += 1;
        }
        for e in list("encounters") {
            let Some(&match_id) = e["matchId"].as_i64().and_then(|id| ids.get(&id)) else { continue };
            let steam_id = text(&e["steamId"]);
            if is_steam_id(&steam_id) {
                tx.execute(
                    "INSERT OR IGNORE INTO encounters (match_id, steam_id, name, side) VALUES (?1, ?2, ?3, ?4)",
                    params![match_id, steam_id, text(&e["name"]), text(&e["side"])],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        for snap in list("snapshots") {
            let Some(&match_id) = snap["matchId"].as_i64().and_then(|id| ids.get(&id)) else { continue };
            let (Some(value), Some(measured_at)) = (snap["value"].as_f64(), snap["measuredAt"].as_i64()) else { continue };
            let steam_id = text(&snap["steamId"]);
            if is_steam_id(&steam_id) && value.is_finite() {
                tx.execute(
                    "INSERT INTO snapshots (match_id, steam_id, provider, metric, value, measured_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![match_id, steam_id, text(&snap["provider"]), text(&snap["metric"]), value, measured_at],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        for n in list("notes") {
            let (steam_id, note) = (text(&n["steamId"]), text(&n["text"]));
            // Format 1 had one note per player with the time it was last changed.
            let Some(created_at) = n["createdAt"].as_i64().or_else(|| n["updatedAt"].as_i64()) else { continue };
            if !is_steam_id(&steam_id) || note.trim().is_empty() || note.chars().count() > MAX_NOTE_CHARS {
                continue;
            }
            let present: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM player_notes WHERE steam_id = ?1 AND created_at = ?2 AND text = ?3)",
                    params![steam_id, created_at, note],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            if present {
                continue;
            }
            let match_id = n["matchId"].as_i64().and_then(|id| ids.get(&id).copied());
            notes += tx
                .execute(
                    "INSERT INTO player_notes (steam_id, text, created_at, map, mode, side, match_started_at, match_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![steam_id, note, created_at, text(&n["map"]), text(&n["mode"]), text(&n["side"]), n["matchStartedAt"].as_i64(), match_id],
                )
                .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        self.summaries.clear();
        Ok(json!({ "matches": added, "skipped": skipped, "notes": notes }))
    }

    /// Deletes recorded matches (and with `notes`, your notes too).
    pub fn clear(&mut self, notes: bool) -> Result<(), String> {
        let sql = if notes { "DELETE FROM matches; DELETE FROM player_notes;" } else { "DELETE FROM matches;" };
        self.conn.execute_batch(sql).map_err(|e| e.to_string())?;
        let _ = self.conn.execute_batch("VACUUM;");
        self.summaries.clear();
        Ok(())
    }

    /// Deletes matches older than `days` (0 keeps everything).
    pub fn purge_older_than(&mut self, days: f64) {
        if days <= 0.0 {
            return;
        }
        let cutoff = now_ms() - (days * 86_400_000.0) as i64;
        if self.conn.execute("DELETE FROM matches WHERE ended_at < ?1", params![cutoff]).unwrap_or(0) > 0 {
            self.summaries.clear();
        }
    }

    /// Your SteamID from the most recent recorded match, for checks run while CS2 is closed.
    pub fn last_self_id(&self) -> Option<String> {
        self.conn.query_row("SELECT self_id FROM matches ORDER BY ended_at DESC LIMIT 1", [], |row| row.get(0)).ok()
    }

    pub fn stats(&self) -> Value {
        let count = |table: &str| -> i64 { self.conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0)).unwrap_or(0) };
        let players: i64 = self.conn.query_row("SELECT COUNT(DISTINCT steam_id) FROM encounters", [], |row| row.get(0)).unwrap_or(0);
        let bytes = std::fs::metadata(&self.file).map(|m| m.len()).unwrap_or(0);
        json!({ "matches": count("matches"), "players": players, "snapshots": count("snapshots"), "notes": count("player_notes"), "file": self.file.to_string_lossy(), "bytes": bytes, "error": self.load_error })
    }
}

/// Your result from the final GSI state: scores belong to whichever side each team ends on.
pub fn match_result(summary: &GsiSummary, own_team: &str) -> &'static str {
    let (Some(ct), Some(t)) = (summary.score_ct, summary.score_t) else { return "unknown" };
    if summary.phase != "gameover" || !matches!(own_team, "CT" | "T") {
        return "unknown";
    }
    let (mine, theirs) = if own_team == "CT" { (ct, t) } else { (t, ct) };
    match mine.cmp(&theirs) {
        std::cmp::Ordering::Greater => "win",
        std::cmp::Ordering::Less => "loss",
        std::cmp::Ordering::Equal => "tie",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "76561198000000000";
    const A: &str = "76561198000000001";

    fn player(id: &str, side: &str) -> HistoryPlayer {
        HistoryPlayer { steam_id: id.into(), side: side.into(), name: "Alpha".into(), snapshots: Vec::new() }
    }

    fn with_hours(mut p: HistoryPlayer, hours: f64, at: i64) -> HistoryPlayer {
        p.snapshots.push(Snapshot { provider: "steam".into(), metric: "hoursCs2".into(), value: hours, measured_at: at });
        p
    }

    #[test]
    fn lists_matches_and_shows_one_with_its_players() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MatchHistoryStore::open(dir.path());
        store.record("de_dust2", "premier", "win", ME, vec![player(ME, "team"), with_hours(player(A, "enemy"), 300.0, 1_000)]);
        store.record("de_mirage", "competitive", "loss", ME, vec![player(ME, "team"), player(A, "team")]);
        store.add_note(A, "Plays AWP", &NoteContext::default()).unwrap();

        let page = store.matches(ME, 1, 0);
        assert_eq!(page["total"], 2);
        let newest = &page["matches"][0];
        // You aren't stored as an encounter, so counts are of the other players.
        assert_eq!(
            (newest["map"].as_str(), newest["result"].as_str(), newest["team"].as_i64(), newest["players"].as_i64()),
            (Some("de_mirage"), Some("loss"), Some(1), Some(1))
        );
        let older = &store.matches(ME, 1, 1)["matches"][0];
        assert_eq!((older["map"].as_str(), older["enemy"].as_i64()), (Some("de_dust2"), Some(1)));
        assert_eq!(store.matches("76561198000000009", 10, 0)["total"], 0, "other accounts' matches are separate");
        assert_eq!(store.matches("", 10, 0)["total"], 2);

        let details = store.match_details(older["id"].as_i64().unwrap()).unwrap();
        assert_eq!(details["map"], "de_dust2");
        let players = details["players"].as_array().unwrap();
        assert_eq!(players.len(), 1);
        let a = &players[0];
        assert_eq!((a["side"].as_str(), a["met"].as_i64(), a["hasNote"].as_bool()), (Some("enemy"), Some(2), Some(true)));
        assert_eq!(a["values"]["steam.hoursCs2"], 300.0, "values as they were at that match");
        assert!(store.match_details(9999).is_none());

        // Matches ending in the same millisecond keep their recorded order (newest first).
        store.insert(None, &OwnMatchStats::default(), 5_000, "de_nuke", "premier", "win", ME, &[]).unwrap();
        store.insert(None, &OwnMatchStats::default(), 5_000, "de_train", "premier", "loss", ME, &[]).unwrap();
        let page = store.matches(ME, 10, 0);
        let tied: Vec<&str> = page["matches"].as_array().unwrap().iter().filter(|m| m["endedAt"] == 5_000).map(|m| m["map"].as_str().unwrap()).collect();
        assert_eq!(tied, ["de_train", "de_nuke"]);
    }

    #[test]
    fn own_stats_per_map_over_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MatchHistoryStore::open(dir.path());
        let line = |k, d| OwnMatchStats { kills: Some(k), deaths: Some(d), assists: Some(2), mvps: Some(1), score: Some(30) };
        store.record_match(None, &line(20, 10), "de_mirage", "premier", "win", ME, vec![player(A, "enemy")]);
        store.record_match(None, &line(10, 20), "de_mirage", "premier", "loss", ME, vec![player(A, "enemy")]);
        store.record_match(None, &OwnMatchStats::default(), "de_mirage", "premier", "win", ME, vec![player(A, "enemy")]);
        store.record_match(None, &line(15, 15), "de_nuke", "premier", "tie", ME, vec![player(A, "team")]);
        let perf = store.own_performance(ME);
        let mirage = &perf["maps"][0];
        assert_eq!(mirage["map"], "de_mirage", "most played first");
        assert_eq!((mirage["played"].as_u64(), mirage["won"].as_u64(), mirage["lost"].as_u64()), (Some(3), Some(2), Some(1)));
        assert_eq!((mirage["withStats"].as_i64(), mirage["kills"].as_i64(), mirage["deaths"].as_i64()), (Some(2), Some(30), Some(30)));
        let series = mirage["matches"].as_array().unwrap();
        assert_eq!((series[0]["kills"].as_i64(), series[1]["kills"].as_i64()), (Some(20), Some(10)), "oldest first");
        assert!(series[2]["kills"].is_null(), "a match without your stats still counts for results");
        assert_eq!(perf["maps"][1]["tied"], 1);
        assert_eq!(store.own_performance("76561198000000009")["maps"].as_array().unwrap().len(), 0, "per account");
        // Export and import keep your stats.
        let export = store.export();
        let other = tempfile::tempdir().unwrap();
        let mut copy = MatchHistoryStore::open(other.path());
        copy.import(&export).unwrap();
        assert_eq!(copy.own_performance(ME)["maps"][0]["kills"], 30);
    }

    #[test]
    fn records_and_summarizes_with_sides() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MatchHistoryStore::open(dir.path());
        assert!(!store.record("de_dust2", "competitive", "win", ME, vec![player(ME, "team")]), "only yourself is not worth recording");
        assert!(store.record("de_dust2", "competitive", "win", ME, vec![player(A, "team"), player("bad", "")]));
        assert!(store.record("de_nuke", "competitive", "loss", ME, vec![player(A, "enemy")]));
        assert!(store.record("de_nuke", "competitive", "unknown", ME, vec![player(A, "")]));
        drop(store);
        let mut reopened = MatchHistoryStore::open(dir.path());
        let summary = reopened.summary_for(ME, A);
        assert_eq!(summary.all, Tally { played: 3, won: 1, lost: 1, tied: 0, unknown: 1 });
        assert_eq!(summary.together.won, 1);
        assert_eq!(summary.against.lost, 1);
        assert!(summary.first_played_at.is_some() && summary.last_played_at >= summary.first_played_at);
        assert_eq!(reopened.summary_for("76561198000000009", A).all.played, 0, "history is per account");
        let history = reopened.player_history(ME, A);
        assert_eq!(history["encounters"].as_array().unwrap().len(), 3);
        assert_eq!(history["encounters"][0]["name"], "Alpha");
    }

    #[test]
    fn snapshots_keep_what_was_measured_when() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MatchHistoryStore::open(dir.path());
        store.record("de_inferno", "premier", "win", ME, vec![with_hours(player(A, "enemy"), 400.0, 1_000)]);
        store.record("de_inferno", "premier", "loss", ME, vec![with_hours(player(A, "enemy"), 400.0, 1_000)]);
        store.record("de_mirage", "premier", "win", ME, vec![with_hours(player(A, "enemy"), 450.0, 2_000)]);
        store.record("de_anubis", "premier", "win", ME, vec![with_hours(player(A, "enemy"), 520.0, 3_000)]);
        let history = store.player_history(ME, A);
        let hours = &history["progression"][0];
        assert_eq!((hours["provider"].as_str(), hours["metric"].as_str()), (Some("steam"), Some("hoursCs2")));
        assert_eq!(hours["measurements"], 3, "the same fetched value at two matches is one measurement");
        assert_eq!(
            (hours["first"]["value"].as_f64(), hours["previous"]["value"].as_f64(), hours["current"]["value"].as_f64()),
            (Some(400.0), Some(450.0), Some(520.0))
        );
        assert_eq!(hours["current"]["measuredAt"], 3_000, "the source time is kept, not the match time");
    }

    #[test]
    fn import_merges_without_duplicates() {
        let source = tempfile::tempdir().unwrap();
        let mut a = MatchHistoryStore::open(source.path());
        a.record("de_dust2", "premier", "win", ME, vec![with_hours(player(A, "enemy"), 300.0, 1_000)]);
        a.add_note(A, "old note", &NoteContext::default()).unwrap();
        let export = a.export();
        let target = tempfile::tempdir().unwrap();
        let mut b = MatchHistoryStore::open(target.path());
        b.add_note(A, "newer note", &NoteContext::default()).unwrap();
        let first = b.import(&export).unwrap();
        assert_eq!((first["matches"].as_i64(), first["skipped"].as_i64()), (Some(1), Some(0)));
        assert_eq!(first["notes"], 1, "notes from the file are added beside yours");
        assert_eq!(b.notes_for(A).len(), 2);
        assert_eq!(b.summary_for(ME, A).against.won, 1);
        assert_eq!(b.player_history(ME, A)["progression"][0]["current"]["value"], 300.0);
        let again = b.import(&export).unwrap();
        assert_eq!(
            (again["matches"].as_i64(), again["skipped"].as_i64(), again["notes"].as_i64()),
            (Some(0), Some(1), Some(0)),
            "importing twice changes nothing"
        );
        // A format 1 file: one note per player, stamped with when it was last changed.
        let old = json!({ "format": "cs2-player-intel-history", "version": 1, "matches": [], "notes": [{ "steamId": A, "text": "From an old export", "updatedAt": 42 }] });
        assert_eq!(b.import(&old).unwrap()["notes"], 1);
        assert_eq!(b.notes_for(A).last().unwrap()["createdAt"], 42);
        assert!(b.import(&json!({ "format": "something else" })).is_err());
        assert!(b.import(&json!({ "format": "cs2-player-intel-history", "version": 9 })).unwrap_err().contains("newer version"));
    }

    #[test]
    fn notes_export_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MatchHistoryStore::open(dir.path());
        store.record("de_dust2", "competitive", "win", ME, vec![player(A, "team")]);
        assert_eq!(store.summary_for(ME, A).together.won, 1);
        let note = store.add_note(A, "  Plays AWP on B  ", &NoteContext::default()).unwrap();
        assert_eq!(note["text"], "Plays AWP on B");
        assert!(store.add_note(A, &"x".repeat(2001), &NoteContext::default()).is_err());
        assert!(store.add_note(A, "   ", &NoteContext::default()).is_err());
        assert!(store.add_note("bad", "text", &NoteContext::default()).is_err());
        let export = store.export();
        assert_eq!(export["matches"].as_array().unwrap().len(), 1);
        assert_eq!(export["notes"][0]["text"], "Plays AWP on B");
        store.clear(false).unwrap();
        assert_eq!(store.summary_for(ME, A).all.played, 0);
        assert_eq!(store.notes_for(A).len(), 1, "notes survive clearing matches");
        store.clear(true).unwrap();
        assert!(store.notes_for(A).is_empty());
        assert_eq!(store.stats()["matches"], 0);
    }

    #[test]
    fn notes_belong_to_the_match_they_were_written_in() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MatchHistoryStore::open(dir.path());
        let during = NoteContext { map: "de_mirage".into(), mode: "premier".into(), side: "enemy".into(), match_started_at: Some(1_000), match_id: None };
        let first = store.add_note(A, "Peeks banana every round", &during).unwrap();
        assert!(first["matchId"].is_null(), "the match isn't recorded yet");
        assert!(store.record_match(Some(1_000), &OwnMatchStats::default(), "de_mirage", "premier", "loss", ME, vec![player(A, "enemy")]));
        let after = store.add_note(A, "Reported for griefing", &during).unwrap();
        let notes = store.notes_for(A);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0]["text"], "Reported for griefing", "newest first");
        assert_eq!(notes[0]["matchId"], notes[1]["matchId"], "both belong to the match");
        assert_eq!((notes[1]["map"].as_str(), notes[1]["side"].as_str(), notes[1]["result"].as_str()), (Some("de_mirage"), Some("enemy"), Some("loss")));

        let match_id = notes[0]["matchId"].as_i64().unwrap();
        let details = store.match_details(match_id).unwrap();
        assert_eq!(details["players"][0]["notes"].as_array().unwrap().len(), 2);
        // A note for another match, written from the History page, stays with that match.
        assert!(store.record_match(Some(2_000), &OwnMatchStats::default(), "de_nuke", "premier", "win", ME, vec![player(A, "team")]));
        let later = store.match_details(match_id + 1).unwrap();
        assert!(later["players"][0]["notes"].as_array().unwrap().is_empty());
        let context = NoteContext { match_id: Some(match_id + 1), side: "team".into(), ..NoteContext::default() };
        store.add_note(A, "Good teammate this time", &context).unwrap();
        assert_eq!(store.match_details(match_id + 1).unwrap()["players"][0]["notes"][0]["text"], "Good teammate this time");

        store.delete_note(after["id"].as_i64().unwrap()).unwrap();
        assert_eq!(store.notes_for(A).len(), 2);
        // Deleting a match keeps its notes (without the link).
        store.clear(false).unwrap();
        assert!(store.notes_for(A).iter().all(|n| n["matchId"].is_null() && n["result"].is_null()));
        assert_eq!(first["map"], "de_mirage");
    }

    #[test]
    fn databases_with_one_note_per_player_are_upgraded() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("history.sqlite")).unwrap();
            conn.execute_batch(
                "CREATE TABLE matches (id INTEGER PRIMARY KEY, ended_at INTEGER NOT NULL, map TEXT NOT NULL DEFAULT '', mode TEXT NOT NULL DEFAULT '', result TEXT NOT NULL DEFAULT 'unknown', self_id TEXT NOT NULL);
                 CREATE TABLE notes (steam_id TEXT PRIMARY KEY, text TEXT NOT NULL, updated_at INTEGER NOT NULL);
                 INSERT INTO matches (ended_at, map, self_id) VALUES (5, 'de_dust2', '76561198000000000');
                 INSERT INTO notes VALUES ('76561198000000001', 'Kept', 7);",
            )
            .unwrap();
        }
        let mut store = MatchHistoryStore::open(dir.path());
        assert!(store.load_error.is_empty());
        assert_eq!(store.notes_for(A)[0]["text"], "Kept");
        assert_eq!(store.notes_for(A)[0]["createdAt"], 7);
        assert_eq!(store.matches(ME, 10, 0)["total"], 1);
        assert!(
            store.record_match(Some(9), &OwnMatchStats::default(), "de_nuke", "premier", "win", ME, vec![player(A, "team")]),
            "new matches keep their start time"
        );
    }

    #[test]
    fn retention_removes_old_matches() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MatchHistoryStore::open(dir.path());
        store.insert(None, &OwnMatchStats::default(), now_ms() - 40 * 86_400_000, "de_nuke", "competitive", "win", ME, &[player(A, "")]).unwrap();
        store.record("de_nuke", "competitive", "win", ME, vec![player(A, "")]);
        store.purge_older_than(30.0);
        assert_eq!(store.summary_for(ME, A).all.played, 1);
    }

    #[test]
    fn results_follow_the_final_score() {
        let mut summary = GsiSummary { phase: "gameover".into(), score_ct: Some(13), score_t: Some(9), ..GsiSummary::default() };
        assert_eq!(match_result(&summary, "CT"), "win");
        assert_eq!(match_result(&summary, "T"), "loss");
        assert_eq!(match_result(&summary, ""), "unknown");
        summary.score_t = Some(13);
        assert_eq!(match_result(&summary, "T"), "tie");
        summary.phase = "live".into();
        assert_eq!(match_result(&summary, "T"), "unknown");
    }
}
