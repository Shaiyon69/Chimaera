//! Reads umu-run output: the port of src/main/services/umu-output-monitor.ts.
//! Progress lines become launcher status detail; error and fatal lines explain a failed launch.

use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UmuEvent {
    Progress(String),
    Error(String),
    Fatal(String),
}

static ANSI_ESCAPE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").unwrap());
static LOG_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(INFO|WARNING|ERROR|CRITICAL):\s*(\S.*)$").unwrap());
static EXCEPTION_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([A-Z]\w*):\s*(\S.*)$").unwrap());
static PRESSURE_VESSEL_ERROR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^pv-[\w-]+\[\d+\]:\s+E:\s*(\S.*)$").unwrap());

const PROGRESS_PREFIXES: [&str; 7] = ["Setting up Unified Launcher", "Downloading ", "Extracting ", "Verifying integrity of ", "Updating ", "Restoring Runtime Platform", "Found '"];

fn clean_progress(message: &str) -> String {
    let mut cleaned = message.trim_end_matches('.');
    if cleaned.to_lowercase().ends_with("please wait") {
        cleaned = cleaned[..cleaned.len() - "please wait".len()].trim_end();
        cleaned = cleaned.strip_suffix(',').unwrap_or(cleaned);
    }
    cleaned.trim_end().to_string()
}

pub fn parse_line(raw: &str) -> Option<UmuEvent> {
    let stripped = ANSI_ESCAPE.replace_all(raw, "");
    let line = stripped.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(c) = PRESSURE_VESSEL_ERROR.captures(line) {
        return Some(UmuEvent::Fatal(c[1].to_string()));
    }
    if let Some(c) = EXCEPTION_LINE.captures(line) {
        if c[1].ends_with("Error") || c[1].ends_with("Exception") {
            return Some(UmuEvent::Fatal(format!("{}: {}", &c[1], &c[2])));
        }
    }
    let c = LOG_LINE.captures(line)?;
    match &c[1] {
        "ERROR" | "CRITICAL" => Some(UmuEvent::Error(c[2].to_string())),
        "INFO" if PROGRESS_PREFIXES.iter().any(|p| c[2].starts_with(p)) => Some(UmuEvent::Progress(clean_progress(&c[2]))),
        _ => None,
    }
}

/// Joins lines split across reads and remembers the failure to report.
#[derive(Default)]
pub struct UmuOutputMonitor {
    buffer: String,
    last_error: Option<String>,
    fatal_error: Option<String>,
}

impl UmuOutputMonitor {
    pub fn feed(&mut self, chunk: &str) -> Vec<UmuEvent> {
        self.buffer.push_str(chunk);
        let Some(end) = self.buffer.rfind('\n') else { return Vec::new() };
        let complete: String = self.buffer.drain(..=end).collect();
        self.parse_lines(complete.lines())
    }

    pub fn flush(&mut self) -> Vec<UmuEvent> {
        let rest = std::mem::take(&mut self.buffer);
        self.parse_lines(std::iter::once(rest.as_str()))
    }

    pub fn failure_message(&self) -> Option<&str> {
        self.fatal_error.as_deref().or(self.last_error.as_deref())
    }

    pub fn has_fatal_error(&self) -> bool {
        self.fatal_error.is_some()
    }

    fn parse_lines<'a>(&mut self, lines: impl Iterator<Item = &'a str>) -> Vec<UmuEvent> {
        let events: Vec<UmuEvent> = lines.filter_map(parse_line).collect();
        for event in &events {
            match event {
                UmuEvent::Error(m) => self.last_error = Some(m.clone()),
                UmuEvent::Fatal(m) => self.fatal_error = Some(m.clone()),
                UmuEvent::Progress(_) => {}
            }
        }
        events
    }
}

/// Why a umu-run exit counts as a failed launch, or `None` when it does not.
pub fn setup_failure_message(exit_code: Option<i32>, signal: Option<i32>, failure: Option<&str>, has_fatal: bool, game_detected: bool) -> Option<String> {
    if game_detected {
        return if exit_code != Some(0) && has_fatal { failure.map(str::to_string) } else { None };
    }
    if has_fatal && failure.is_some() {
        return failure.map(str::to_string);
    }
    if exit_code == Some(0) {
        return None;
    }
    if let Some(f) = failure {
        return Some(f.to_string());
    }
    Some(match (signal, exit_code) {
        (Some(s), _) => format!("umu-run was terminated by signal {s}"),
        (None, Some(c)) => format!("umu-run exited with code {c}"),
        (None, None) => "umu-run exited with code null".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use UmuEvent::*;

    #[test]
    fn fatal_exception_before_detection_wins_even_on_exit_zero() {
        let mut m = UmuOutputMonitor::default();
        m.feed("INFO: Downloading steamrt3 (3.0.20260928.262393)...\n");
        m.feed("Traceback (most recent call last):\nRuntimeError: runtime setup failed\n");
        assert_eq!(setup_failure_message(Some(0), None, m.failure_message(), m.has_fatal_error(), false).as_deref(), Some("RuntimeError: runtime setup failed"));
    }

    #[test]
    fn non_zero_exit_before_detection() {
        assert_eq!(setup_failure_message(Some(1), None, Some("runtime setup failed"), false, false).as_deref(), Some("runtime setup failed"));
        assert_eq!(setup_failure_message(Some(1), None, None, false, false).as_deref(), Some("umu-run exited with code 1"));
        assert_eq!(setup_failure_message(None, Some(9), None, false, false).as_deref(), Some("umu-run was terminated by signal 9"));
    }

    #[test]
    fn permissive_once_game_detected() {
        assert_eq!(setup_failure_message(Some(1), None, Some("runtime setup failed"), false, true), None);
        assert_eq!(setup_failure_message(Some(0), None, Some("RuntimeError: boom"), true, true), None);
        assert_eq!(setup_failure_message(Some(1), None, Some("RuntimeError: boom"), true, true).as_deref(), Some("RuntimeError: boom"));
        assert_eq!(setup_failure_message(Some(0), None, None, false, false), None);
    }

    #[test]
    fn progress_without_wait_suffix() {
        assert_eq!(parse_line("INFO: Downloading steamrt3 (3.0.20260928.262393), please wait..."), Some(Progress("Downloading steamrt3 (3.0.20260928.262393)".into())));
        assert_eq!(parse_line("INFO: Extracting UMU-Proton-10.0-4.tar.gz..."), Some(Progress("Extracting UMU-Proton-10.0-4.tar.gz".into())));
        assert_eq!(parse_line("\u{1b}[34m\u{1b}[1mINFO\u{1b}[0m: Verifying integrity of sniper_platform_3.0..."), Some(Progress("Verifying integrity of sniper_platform_3.0".into())));
    }

    #[test]
    fn ignores_other_lines() {
        for line in ["INFO: Using UMU-Proton-10.0-4", "fsync: up and running.", "Note: shader cache is warm", "   ", "Proton: Error: unable to use parent for game drive, path /home"] {
            assert_eq!(parse_line(line), None, "{line}");
        }
    }

    #[test]
    fn errors_and_fatals() {
        assert_eq!(parse_line("ERROR: Digest mismatched: SteamLinuxRuntime_sniper.tar.xz"), Some(Error("Digest mismatched: SteamLinuxRuntime_sniper.tar.xz".into())));
        assert_eq!(parse_line("FileNotFoundError: _v2-entry-point (umu) cannot be found"), Some(Fatal("FileNotFoundError: _v2-entry-point (umu) cannot be found".into())));
        assert_eq!(parse_line("pv-adverb[106667]: E: Failed to execute child process"), Some(Fatal("Failed to execute child process".into())));
    }

    #[test]
    fn joins_split_lines_and_prefers_fatal() {
        let mut m = UmuOutputMonitor::default();
        assert_eq!(m.feed("INFO: Downloading UMU-"), []);
        assert_eq!(m.feed("Proton-10.0-4.tar.gz...\nINFO: Using"), [Progress("Downloading UMU-Proton-10.0-4.tar.gz".into())]);
        assert_eq!(m.flush(), []);
        m.feed("ERROR: Digest mismatched: x\nTraceback (most recent call last):\nValueError: Digest mismatched: x\n");
        assert!(m.has_fatal_error());
        assert_eq!(m.failure_message(), Some("ValueError: Digest mismatched: x"));
        let mut m = UmuOutputMonitor::default();
        m.feed("ERROR: Aborting steamrt install due to network error");
        m.flush();
        assert!(!m.has_fatal_error());
        assert_eq!(m.failure_message(), Some("Aborting steamrt install due to network error"));
    }
}
