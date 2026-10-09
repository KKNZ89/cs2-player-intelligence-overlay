//! The app's own record of finished matches, in a local SQLite database (history.sqlite in the app data
//! folder): who was in each match, the sides you marked, your result, values measured from Steam, CSRep,
//! CSStats and FACEIT at that time (each with its source time), and your notes on players.
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
    self_id TEXT NOT NULL
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
CREATE TABLE IF NOT EXISTS notes (
    steam_id TEXT PRIMARY KEY,
    text TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
";
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
    Ok(conn)
}

impl MatchHistoryStore {
    /// Opens history.sqlite in `directory`, importing the older match-history.json once.
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

    fn insert(&mut self, ended_at: i64, map: &str, mode: &str, result: &str, self_id: &str, players: &[HistoryPlayer]) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO matches (ended_at, map, mode, result, self_id) VALUES (?1, ?2, ?3, ?4, ?5)", params![ended_at, map, mode, result, self_id])?;
        let match_id = tx.last_insert_rowid();
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
    pub fn record(&mut self, map: &str, mode: &str, result: &str, self_id: &str, players: Vec<HistoryPlayer>) -> bool {
        let others: Vec<HistoryPlayer> = players.into_iter().filter(|p| is_steam_id(&p.steam_id) && p.steam_id != self_id).collect();
        if self_id.is_empty() || others.is_empty() {
            return false;
        }
        // History is a convenience; the match itself is unaffected when it cannot be saved.
        let saved = self.insert(now_ms(), map, mode, result, self_id, &others).is_ok();
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
                 WHERE ?1 = '' OR m.self_id = ?1 GROUP BY m.id ORDER BY m.ended_at DESC LIMIT ?2 OFFSET ?3",
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
                        EXISTS(SELECT 1 FROM notes n WHERE n.steam_id = e.steam_id) \
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
                        }))
                    })?
                    .collect()
            })
            .unwrap_or_default();
        Some(json!({ "id": id, "endedAt": ended_at, "map": map, "mode": mode, "result": result, "selfId": self_id, "players": players }))
    }

    /// Everything recorded about one player: encounters, measured values over time, and your note.
    pub fn player_history(&mut self, self_id: &str, steam_id: &str) -> Value {
        let summary = self.summary_for(self_id, steam_id);
        let encounters: Vec<Value> = self
            .conn
            .prepare("SELECT m.ended_at, m.map, m.mode, m.result, e.side, e.name FROM encounters e JOIN matches m ON m.id = e.match_id WHERE m.self_id = ?1 AND e.steam_id = ?2 ORDER BY m.ended_at DESC LIMIT 200")
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
        let note = self.note(steam_id);
        json!({ "summary": summary, "encounters": encounters, "progression": progression, "note": note })
    }

    pub fn note(&self, steam_id: &str) -> Value {
        self.conn
            .query_row("SELECT text, updated_at FROM notes WHERE steam_id = ?1", params![steam_id], |row| {
                Ok(json!({ "text": row.get::<_, String>(0)?, "updatedAt": row.get::<_, i64>(1)? }))
            })
            .optional()
            .ok()
            .flatten()
            .unwrap_or(Value::Null)
    }

    /// Saves (or with empty text, deletes) your note on a player.
    pub fn set_note(&mut self, steam_id: &str, text: &str) -> Result<(), String> {
        let text = text.trim();
        if text.chars().count() > 2000 {
            return Err("Notes are limited to 2,000 characters.".into());
        }
        let result = if text.is_empty() {
            self.conn.execute("DELETE FROM notes WHERE steam_id = ?1", params![steam_id])
        } else {
            self.conn.execute(
                "INSERT INTO notes (steam_id, text, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(steam_id) DO UPDATE SET text = excluded.text, updated_at = excluded.updated_at",
                params![steam_id, text, now_ms()],
            )
        };
        result.map(|_| ()).map_err(|e| e.to_string())
    }

    /// SteamIDs with a note, for the note marker in tables.
    pub fn noted(&self) -> Vec<String> {
        self.conn
            .prepare("SELECT steam_id FROM notes")
            .and_then(|mut statement| statement.query_map([], |row| row.get::<_, String>(0))?.collect())
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
            "version": 1,
            "exportedAt": now_ms(),
            "matches": table("SELECT id, ended_at, map, mode, result, self_id FROM matches ORDER BY ended_at", &["id", "endedAt", "map", "mode", "result", "selfId"]),
            "encounters": table("SELECT match_id, steam_id, name, side FROM encounters", &["matchId", "steamId", "name", "side"]),
            "snapshots": table("SELECT match_id, steam_id, provider, metric, value, measured_at FROM snapshots", &["matchId", "steamId", "provider", "metric", "value", "measuredAt"]),
            "notes": table("SELECT steam_id, text, updated_at FROM notes", &["steamId", "text", "updatedAt"]),
        })
    }

    /// Merges a file written by [`Self::export`]. A match already recorded (same account, end time and map)
    /// is skipped, so importing the same file twice changes nothing; for notes, the newer one wins.
    pub fn import(&mut self, data: &Value) -> Result<Value, String> {
        if data["format"] != "cs2-player-intel-history" {
            return Err("This is not a CS2 Player Intel history export.".into());
        }
        if data["version"].as_i64() != Some(1) {
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
            let exists: bool = tx
                .query_row("SELECT EXISTS(SELECT 1 FROM matches WHERE self_id = ?1 AND ended_at = ?2 AND map = ?3)", params![self_id, ended_at, map], |row| {
                    row.get(0)
                })
                .map_err(|e| e.to_string())?;
            if exists {
                skipped += 1;
                continue;
            }
            tx.execute(
                "INSERT INTO matches (ended_at, map, mode, result, self_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![ended_at, map, text(&m["mode"]), text(&m["result"]), self_id],
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
            let Some(updated_at) = n["updatedAt"].as_i64() else { continue };
            if !is_steam_id(&steam_id) || note.trim().is_empty() || note.chars().count() > 2000 {
                continue;
            }
            notes += tx
                .execute(
                    "INSERT INTO notes (steam_id, text, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(steam_id) DO UPDATE SET text = excluded.text, updated_at = excluded.updated_at WHERE excluded.updated_at > notes.updated_at",
                    params![steam_id, note, updated_at],
                )
                .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        self.summaries.clear();
        Ok(json!({ "matches": added, "skipped": skipped, "notes": notes }))
    }

    /// Deletes recorded matches (and with `notes`, your notes too).
    pub fn clear(&mut self, notes: bool) -> Result<(), String> {
        let sql = if notes { "DELETE FROM matches; DELETE FROM notes;" } else { "DELETE FROM matches;" };
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
        json!({ "matches": count("matches"), "players": players, "snapshots": count("snapshots"), "notes": count("notes"), "file": self.file.to_string_lossy(), "bytes": bytes, "error": self.load_error })
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
        store.set_note(A, "Plays AWP").unwrap();

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
        a.set_note(A, "old note").unwrap();
        let export = a.export();
        let target = tempfile::tempdir().unwrap();
        let mut b = MatchHistoryStore::open(target.path());
        b.set_note(A, "newer note").unwrap();
        let first = b.import(&export).unwrap();
        assert_eq!((first["matches"].as_i64(), first["skipped"].as_i64()), (Some(1), Some(0)));
        assert_eq!(first["notes"], 0, "the newer note is kept");
        assert_eq!(b.note(A)["text"], "newer note");
        assert_eq!(b.summary_for(ME, A).against.won, 1);
        assert_eq!(b.player_history(ME, A)["progression"][0]["current"]["value"], 300.0);
        let again = b.import(&export).unwrap();
        assert_eq!((again["matches"].as_i64(), again["skipped"].as_i64()), (Some(0), Some(1)), "importing twice changes nothing");
        assert!(b.import(&json!({ "format": "something else" })).is_err());
        assert!(b.import(&json!({ "format": "cs2-player-intel-history", "version": 9 })).unwrap_err().contains("newer version"));
    }

    #[test]
    fn notes_export_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MatchHistoryStore::open(dir.path());
        store.record("de_dust2", "competitive", "win", ME, vec![player(A, "team")]);
        assert_eq!(store.summary_for(ME, A).together.won, 1);
        store.set_note(A, "  Plays AWP on B  ").unwrap();
        assert_eq!(store.note(A)["text"], "Plays AWP on B");
        assert_eq!(store.noted(), [A]);
        assert!(store.set_note(A, &"x".repeat(2001)).is_err());
        let export = store.export();
        assert_eq!(export["matches"].as_array().unwrap().len(), 1);
        assert_eq!(export["notes"][0]["text"], "Plays AWP on B");
        store.clear(false).unwrap();
        assert_eq!(store.summary_for(ME, A).all.played, 0);
        assert!(!store.note(A).is_null(), "notes survive clearing matches");
        store.clear(true).unwrap();
        assert!(store.note(A).is_null());
        assert_eq!(store.stats()["matches"], 0);
    }

    #[test]
    fn retention_removes_old_matches() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MatchHistoryStore::open(dir.path());
        store.insert(now_ms() - 40 * 86_400_000, "de_nuke", "competitive", "win", ME, &[player(A, "")]).unwrap();
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
