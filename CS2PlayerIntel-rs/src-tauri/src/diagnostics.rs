//! diagnostics.log in the app data folder: one line per event with level and operation, secrets
//! redacted, rotated at 1 MB. Lines are short and rare, so they are appended synchronously.

use regex::Regex;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

const MAX_BYTES: u64 = 1024 * 1024;

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

    /// The last lines, newest last, for the Diagnostics view.
    pub fn recent(&self, count: usize) -> Vec<String> {
        let _guard = self.lock.lock().unwrap();
        let read = || -> std::io::Result<String> {
            let mut file = std::fs::File::open(&self.file)?;
            let size = file.metadata()?.len();
            let length = size.min(64 * 1024);
            file.seek(SeekFrom::Start(size - length))?;
            let mut buffer = Vec::with_capacity(length as usize);
            file.read_to_end(&mut buffer)?;
            Ok(String::from_utf8_lossy(&buffer).into_owned())
        };
        let text = read().unwrap_or_default();
        let lines: Vec<String> = text.lines().filter(|l| !l.is_empty()).map(String::from).collect();
        lines[lines.len().saturating_sub(count)..].to_vec()
    }
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
