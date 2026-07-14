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

impl CapturedLogs {
    pub fn snapshot(&self) -> Vec<LogLine> {
        self.lines.lock().unwrap().clone()
    }
    fn push(&self, line: LogLine) {
        self.lines.lock().unwrap().push(line);
    }
}

/// Attach capture threads to a child's stdout/stderr. Lines go to memory + evidence file.
pub fn attach(child: &mut Child, evidence_file: PathBuf, started: Instant) -> (CapturedLogs, Vec<JoinHandle<()>>) {
    let logs = CapturedLogs::default();
    let file = Arc::new(Mutex::new(
        std::fs::File::create(&evidence_file).expect("create evidence log file"),
    ));
    let mut handles = Vec::new();

    if let Some(out) = child.stdout.take() {
        handles.push(spawn_reader(out, "stdout", logs.clone(), file.clone(), started));
    }
    if let Some(err) = child.stderr.take() {
        handles.push(spawn_reader(err, "stderr", logs.clone(), file.clone(), started));
    }
    (logs, handles)
}

fn spawn_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    source: &'static str,
    logs: CapturedLogs,
    file: Arc<Mutex<std::fs::File>>,
    started: Instant,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let buf = BufReader::new(reader);
        for line in buf.lines() {
            let Ok(text) = line else { break };
            let at_ms = started.elapsed().as_millis();
            if let Ok(mut f) = file.lock() {
                let _ = writeln!(f, "[{at_ms:>8}ms {source}] {text}");
            }
            logs.push(LogLine { at_ms, source, text });
        }
    })
}
