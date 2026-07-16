//! Deterministic failure diagnosis — no AI, no guessing: correlate captured
//! evidence and surface the most probable cause. The verdict stays mechanical.

use crate::capture::LogLine;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Cause {
    /// One-line probable cause (a panic, a fatal, the last error).
    pub headline: String,
    /// The correlated log lines that support it.
    pub correlated: Vec<String>,
}

const PANIC_MARKERS: [&str; 4] = [
    "panicked at",
    "FATAL",
    "Traceback (most recent call last)",
    "fatal:",
];
const ERROR_MARKERS: [&str; 3] = ["ERROR", "error:", "Error:"];

/// Priority: first panic-class line, else last error-class line, with neighbours.
pub fn from_logs(lines: &[LogLine]) -> Option<Cause> {
    let panic_idx = lines
        .iter()
        .position(|l| PANIC_MARKERS.iter().any(|m| l.text.contains(m)));
    let error_idx = lines
        .iter()
        .rposition(|l| ERROR_MARKERS.iter().any(|m| l.text.contains(m)));

    let idx = panic_idx.or(error_idx)?;
    let headline = lines[idx].text.trim().to_string();

    // Bring 2 lines of context after a panic (backtrace hint), 1 before an error.
    let lo = idx.saturating_sub(1);
    let hi = (idx + 3).min(lines.len());
    let correlated = lines[lo..hi]
        .iter()
        .map(|l| format!("[{:>6}ms {}] {}", l.at_ms, l.source, l.text.trim_end()))
        .collect();

    Some(Cause {
        headline,
        correlated,
    })
}

/// Tail of raw output for suites (last meaningful lines).
pub fn tail(lines: &[LogLine], n: usize) -> Vec<String> {
    let meaningful: Vec<&LogLine> = lines.iter().filter(|l| !l.text.trim().is_empty()).collect();
    meaningful
        .iter()
        .rev()
        .take(n)
        .rev()
        .map(|l| l.text.trim_end().to_string())
        .collect()
}
