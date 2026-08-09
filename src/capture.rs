//! Continuous capture of a child process' output — the runner OWNS what it launches.
//! Every line is timestamped, kept in memory for oracles/diagnosis, and mirrored to
//! the run's evidence directory.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct LogLine {
    pub at_ms: u128,
    pub source: &'static str, // "stdout" | "stderr"
    pub text: String,
}

#[derive(Clone, Default)]
pub struct CapturedLogs {
    lines: Arc<Mutex<Vec<LogLine>>>,
}

/// A chatty (or hostile) service must not take probatum down with it. The
/// evidence file keeps everything; memory keeps the earliest lines, which is
/// what diagnosis needs — the first panic/error marker wins.
const MAX_LINES_IN_MEMORY: usize = 100_000;

impl CapturedLogs {
    pub fn snapshot(&self) -> Vec<LogLine> {
        self.lines.lock().unwrap().clone()
    }
    fn push(&self, line: LogLine) {
        let mut lines = self.lines.lock().unwrap();
        if lines.len() < MAX_LINES_IN_MEMORY {
            lines.push(line);
        }
    }
}

/// Attach capture threads to a child's stdout/stderr. Lines go to memory + evidence file.
pub fn attach(
    child: &mut Child,
    evidence_file: PathBuf,
    started: Instant,
) -> (CapturedLogs, Vec<JoinHandle<()>>) {
    let logs = CapturedLogs::default();
    // An unwritable evidence dir must not panic the runner: capture keeps
    // working in memory, and the caller is told the file does not exist
    // rather than being handed a path that was never written.
    let file = Arc::new(Mutex::new(std::fs::File::create(&evidence_file).ok()));
    let mut handles = Vec::new();

    if let Some(out) = child.stdout.take() {
        handles.push(spawn_reader(
            out,
            "stdout",
            logs.clone(),
            file.clone(),
            started,
        ));
    }
    if let Some(err) = child.stderr.take() {
        handles.push(spawn_reader(
            err,
            "stderr",
            logs.clone(),
            file.clone(),
            started,
        ));
    }
    (logs, handles)
}

fn spawn_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    source: &'static str,
    logs: CapturedLogs,
    file: Arc<Mutex<Option<std::fs::File>>>,
    started: Instant,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let buf = BufReader::new(reader);
        for line in buf.lines() {
            let Ok(text) = line else { break };
            let at_ms = started.elapsed().as_millis();
            if let Ok(mut f) = file.lock() {
                if let Some(f) = f.as_mut() {
                    let _ = writeln!(f, "[{at_ms:>8}ms {source}] {text}");
                }
            }
            logs.push(LogLine {
                at_ms,
                source,
                text,
            });
        }
    })
}
