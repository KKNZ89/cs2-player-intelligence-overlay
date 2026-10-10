//! diagnostics.log in the app data folder: one line per event with level and operation, secrets
//! redacted, rotated at 1 MB. Lines are short and rare, so they are appended synchronously.

use regex::Regex;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

const MAX_BYTES: u64 = 1024 * 1024;
/// How much of the end of the log the last-match report reads.
const REPORT_BYTES: u64 = 512 * 1024;
/// Without a match start in the log, the report covers this many of the last lines.
const REPORT_FALLBACK_LINES: usize = 300;

static SECRET_FIELD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(api[-_ ]?key|authorization|cookie|token|session)\s*[:=]\s*[^\s,;]+").unwrap());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Info,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Info => "INFO",
        }
    }
}

pub fn redact(text: &str, secrets: &[String]) -> String {
    let mut value = text.to_string();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        value = value.replace(secret.as_str(), "[redacted]");
    }
    SECRET_FIELD.replace_all(&value, "$1=[redacted]").into_owned()
}

pub struct Diagnostics {
    pub file: PathBuf,
    secrets: Mutex<Vec<String>>,
    lock: Mutex<()>,
}

impl Diagnostics {
    pub fn new(directory: PathBuf) -> Self {
        Self { file: directory.join("diagnostics.log"), secrets: Mutex::new(Vec::new()), lock: Mutex::new(()) }
    }

    /// Values that must never reach the log (API keys, the GSI token).
    pub fn set_secrets(&self, secrets: Vec<String>) {
        *self.secrets.lock().unwrap() = secrets;
    }

    pub fn log(&self, level: Level, operation: &str, message: &str) {
        let secrets = self.secrets.lock().unwrap().clone();
        let line = format!(
            "{} {} {}: {}\n",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
            level.label(),
            redact(operation, &secrets),
            redact(message, &secrets).replace(['\r', '\n'], " ")
        );
        let _guard = self.lock.lock().unwrap();
        // Logging must never interrupt recovery, so failures are ignored.
        let _ = self.append(&line);
    }

    pub fn error(&self, operation: &str, message: &str) {
        self.log(Level::Error, operation, message);
    }

    fn append(&self, line: &str) -> std::io::Result<()> {
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::metadata(&self.file).map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
            let _ = std::fs::rename(&self.file, self.file.with_extension("log.previous"));
        }
        OpenOptions::new().create(true).append(true).open(&self.file)?.write_all(line.as_bytes())
    }

    /// Up to `bytes` from the end of the log.
    fn tail(&self, bytes: u64) -> String {
        let _guard = self.lock.lock().unwrap();
        let read = || -> std::io::Result<String> {
            let mut file = std::fs::File::open(&self.file)?;
            let size = file.metadata()?.len();
            let length = size.min(bytes);
            file.seek(SeekFrom::Start(size - length))?;
            let mut buffer = Vec::with_capacity(length as usize);
            file.read_to_end(&mut buffer)?;
            Ok(String::from_utf8_lossy(&buffer).into_owned())
        };
        read().unwrap_or_default()
    }

    /// The last lines, newest last, for the Diagnostics view.
    pub fn recent(&self, count: usize) -> Vec<String> {
        let text = self.tail(64 * 1024);
        let lines: Vec<String> = text.lines().filter(|l| !l.is_empty()).map(String::from).collect();
        lines[lines.len().saturating_sub(count)..].to_vec()
    }

    /// The log since the last match started, shortened for sharing (see [`match_report`]).
    pub fn last_match_report(&self) -> String {
        match_report(&self.tail(REPORT_BYTES))
    }
}

/// The log since the last match started, short enough to paste: each repeated line once with its count
/// and last time, and the latest lookup summary per player instead of one per lookup.
pub fn match_report(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    // Skip a partial first line from reading mid-file.
    let whole = |line: &&str| line.split(' ').next().is_some_and(|time| chrono::DateTime::parse_from_rfc3339(time).is_ok());
    let lines = if lines.first().is_none_or(whole) { &lines[..] } else { &lines[1..] };
    let start = lines.iter().rposition(|l| l.contains(" match: started")).unwrap_or(lines.len().saturating_sub(REPORT_FALLBACK_LINES));
    let clock = |time: &str| time.get(11..19).unwrap_or(time).to_string();
    // (message, first time, last time, count) in order of first appearance.
    let mut events: Vec<(&str, &str, &str, usize)> = Vec::new();
    let mut players: Vec<(&str, &str)> = Vec::new();
    for line in &lines[start..] {
        let (time, message) = line.split_once(' ').unwrap_or(("", line));
        if let Some(summary) = message.strip_prefix("INFO lookup-summary: ") {
            let player = summary.split(':').next().unwrap_or_default();
            match players.iter_mut().find(|(p, _)| *p == player) {
                Some(entry) => entry.1 = summary,
                None => players.push((player, summary)),
            }
            continue;
        }
        match events.iter_mut().find(|(m, ..)| *m == message) {
            Some(event) => {
                event.2 = time;
                event.3 += 1;
            }
            None => events.push((message, time, time, 1)),
        }
    }
    let since = lines.get(start).and_then(|l| l.split_once(' ')).map(|(time, _)| time).unwrap_or("the start of the log");
    let mut out = vec![format!("CS2 Player Intel {} · log since {since} (repeats folded)", env!("CARGO_PKG_VERSION"))];
    for (message, first, last, count) in events {
        out.push(if count == 1 { format!("{first} {message}") } else { format!("{first} {message} (×{count}, last {})", clock(last)) });
    }
    if !players.is_empty() {
        out.push("Latest lookup per player:".into());
        out.extend(players.into_iter().map(|(_, summary)| format!("  {summary}")));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secrets_and_key_value_pairs() {
        let text = redact("failed with abc123 and token=xyz; api_key: q1", &["abc123".into()]);
        assert_eq!(text, "failed with [redacted] and token=[redacted]; api_key=[redacted]");
    }

    #[test]
    fn match_report_starts_at_the_last_match_and_folds_repeats() {
        let log = "\
5:44.1Z INFO partial line
2026-10-09T09:00:00.000Z INFO match: started: de_dust2 competitive (from warmup)
2026-10-09T09:01:00.000Z INFO lookup-summary: …1111: leetify not-found
2026-10-09T10:00:00.000Z INFO match: started: de_mirage competitive (from warmup)
2026-10-09T10:01:32.000Z ERROR steam-lookup: rate-limited: Steam is limiting profile requests.
2026-10-09T10:01:32.100Z INFO lookup-summary: …2222: steam rate-limited
2026-10-09T10:01:33.000Z ERROR steam-lookup: rate-limited: Steam is limiting profile requests.
2026-10-09T10:02:00.000Z INFO lookup-summary: …3333: leetify not-found
2026-10-09T10:04:42.000Z ERROR steam-lookup: rate-limited: Steam is limiting profile requests.
2026-10-09T10:04:42.100Z INFO lookup-summary: …2222: leetify not-found, steam rate-limited
";
        let report = match_report(log);
        let lines: Vec<&str> = report.lines().collect();
        assert!(lines[0].contains("log since 2026-10-09T10:00:00.000Z"), "{report}");
        assert_eq!(lines[1], "2026-10-09T10:00:00.000Z INFO match: started: de_mirage competitive (from warmup)");
        assert_eq!(lines[2], "2026-10-09T10:01:32.000Z ERROR steam-lookup: rate-limited: Steam is limiting profile requests. (×3, last 10:04:42)");
        assert_eq!(lines[3], "Latest lookup per player:");
        assert_eq!(&lines[4..], ["  …2222: leetify not-found, steam rate-limited", "  …3333: leetify not-found"]);
        assert!(!report.contains("…1111"), "earlier matches are left out");
        assert!(match_report("").starts_with("CS2 Player Intel"), "an empty log still gives a header");
    }

    /// Prints the report for a real log: CS2INTEL_LOG=path cargo test real_log_report -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_log_report() {
        let text = std::fs::read_to_string(std::env::var("CS2INTEL_LOG").unwrap()).unwrap();
        println!("{}", match_report(&text));
    }

    #[test]
    fn logs_and_reads_recent_lines() {
        let dir = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::new(dir.path().to_path_buf());
        diagnostics.set_secrets(vec!["s3cret".into()]);
        diagnostics.log(Level::Info, "lookup", "first");
        diagnostics.error("lookup", "second s3cret\nmore");
        let lines = diagnostics.recent(1);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("ERROR lookup: second [redacted] more"));
        assert_eq!(diagnostics.recent(10).len(), 2);
    }
}
