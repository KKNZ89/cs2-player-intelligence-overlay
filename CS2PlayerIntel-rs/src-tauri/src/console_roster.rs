//! Player rows from CS2's `status` command in console.log (only written when CS2 runs with -condebug).
//! The file is tailed from its current end, so old sessions are never read.

use crate::model::{now_ms, Candidate};
use regex::Regex;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::LazyLock;

static ROW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*#\s*\d+\s+").unwrap());
static STEAM_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(7656119\d{10})\b").unwrap());
static QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([^"]+)""#).unwrap());
static ROW_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*#?\s*\d*\s*").unwrap());

const MAX_READ: u64 = 1024 * 1024;
const FORGET_AFTER_MS: i64 = 120_000;

/// Only player rows from status; arbitrary logged profile IDs are not roster evidence.
pub fn parse_status_line(line: &str) -> Option<(String, String)> {
    if !ROW.is_match(line) {
        return None;
    }
    let id = STEAM_ID.captures(line)?.get(1)?;
    let steam_id = id.as_str().to_string();
    let name = match QUOTED.captures(line) {
        Some(quoted) => quoted[1].to_string(),
        None => {
            let before = ROW_PREFIX.replace(&line[..id.start()], "").trim().to_string();
            if before.is_empty() {
                steam_id.clone()
            } else {
                before
            }
        }
    };
    Some((steam_id, name))
}

#[derive(Default)]
pub struct ConsoleTailer {
    pub file: Option<PathBuf>,
    offset: u64,
    pending: Vec<u8>,
    /// Newest sightings last.
    pub players: Vec<Candidate>,
    last_error: String,
}

pub enum Poll {
    Unchanged,
    Changed,
    Failed(String),
}

impl ConsoleTailer {
    pub fn start(&mut self, file: Option<PathBuf>) {
        if file == self.file {
            return;
        }
        self.reset(false);
        self.offset = file.as_ref().and_then(|f| std::fs::metadata(f).ok()).map(|m| m.len()).unwrap_or(0);
        self.file = file;
    }

    /// discard_unread skips whatever was logged before now (a new match starts).
    pub fn reset(&mut self, discard_unread: bool) {
        self.players.clear();
        self.pending.clear();
        if discard_unread {
            self.offset = self.file.as_ref().and_then(|f| std::fs::metadata(f).ok()).map(|m| m.len()).unwrap_or(0);
        }
    }

    pub fn poll(&mut self) -> Poll {
        let Some(file) = self.file.clone() else { return Poll::Unchanged };
        let now = now_ms();
        let before = self.players.len();
        self.players.retain(|p| p.seen_at.unwrap_or(0) >= now - FORGET_AFTER_MS);
        let mut changed = before != self.players.len();
        match self.read(&file, now) {
            Ok(found) => {
                self.last_error.clear();
                changed |= found;
            }
            // A locked or unreadable log is retried every second; each distinct failure is reported once.
            Err(error) if error != self.last_error => {
                self.last_error = error.clone();
                return Poll::Failed(error);
            }
            Err(_) => {}
        }
        if changed {
            Poll::Changed
        } else {
            Poll::Unchanged
        }
    }

    fn read(&mut self, file: &PathBuf, now: i64) -> Result<bool, String> {
        let size = match std::fs::metadata(file) {
            Ok(meta) => meta.len(),
            // console.log exists only with -condebug; a file created later is a new CS2 session.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.offset = 0;
                return Ok(false);
            }
            Err(error) => return Err(error.to_string()),
        };
        let mut changed = false;
        if size < self.offset {
            self.offset = 0;
            changed = !self.players.is_empty();
            self.reset(false);
        }
        if size == self.offset {
            return Ok(changed);
        }
        let length = (size - self.offset).min(MAX_READ);
        let mut handle = std::fs::File::open(file).map_err(|e| e.to_string())?;
        handle.seek(SeekFrom::Start(self.offset)).map_err(|e| e.to_string())?;
        let mut buffer = Vec::with_capacity(length as usize);
        handle.take(length).read_to_end(&mut buffer).map_err(|e| e.to_string())?;
        self.offset += buffer.len() as u64;
        self.pending.extend_from_slice(&buffer);
        let Some(last_newline) = self.pending.iter().rposition(|&b| b == b'\n') else {
            if self.pending.len() > 65_536 {
                self.pending.clear();
            }
            return Ok(changed);
        };
        let complete: Vec<u8> = self.pending.drain(..=last_newline).collect();
        for line in String::from_utf8_lossy(&complete).lines() {
            let Some((steam_id, name)) = parse_status_line(line) else { continue };
            self.players.retain(|p| p.steam_id != steam_id);
            self.players.push(Candidate {
                steam_id,
                name,
                source: "console-status".into(),
                confidence: "candidate".into(),
                seen_at: Some(now),
                ..Candidate::default()
            });
            changed = true;
        }
        Ok(changed)
    }

    /// The most recent sightings (enough for the largest lobby), as the roster sees them.
    pub fn recent(&self) -> Vec<Candidate> {
        self.players[self.players.len().saturating_sub(20)..].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_status_rows_only() {
        assert_eq!(parse_status_line(r#"#  3 1 "Player One" 76561198000000001 05:00 50 0 active"#), Some(("76561198000000001".into(), "Player One".into())));
        assert_eq!(parse_status_line("# 4 Unquoted 76561198000000002 ok"), Some(("76561198000000002".into(), "Unquoted".into())));
        assert_eq!(parse_status_line("Visiting 76561198000000003 profile"), None);
    }

    #[test]
    fn tails_new_lines_and_handles_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("console.log");
        std::fs::write(&path, "# 1 \"Old\" 76561198000000009\n").unwrap();
        let mut tailer = ConsoleTailer::default();
        tailer.start(Some(path.clone()));
        assert!(matches!(tailer.poll(), Poll::Unchanged), "existing lines are skipped");
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "# 2 \"New\" 76561198000000001\n# 3 \"Half").unwrap();
        assert!(matches!(tailer.poll(), Poll::Changed));
        assert_eq!(tailer.recent().len(), 1);
        writeln!(file, "\" 76561198000000002").unwrap();
        tailer.poll();
        assert_eq!(tailer.recent()[1].name, "Half");
        std::fs::write(&path, "").unwrap();
        assert!(matches!(tailer.poll(), Poll::Changed), "a truncated log starts over");
        assert!(tailer.players.is_empty());
    }
}
