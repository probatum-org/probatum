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
    state: Arc<Mutex<State>>,
    stdout: Option<Arc<Mutex<Stdout>>>,
}

/// The rules a check applies to its output. They are evaluated on every line
/// as it arrives, never on the bounded in-memory copy: a pattern on line
/// 150,001 must count exactly like one on line 1.
#[derive(Default)]
pub struct Rules {
    pub contains: Vec<String>,
    /// `absent`, plus the default crash markers for a service.
    pub forbid: Vec<String>,
    pub allow: Vec<String>,
}

#[derive(Default)]
struct State {
    lines: Vec<LogLine>,
    rules: Rules,
    found: Vec<bool>,
    /// First forbidden line with its context: one line before, two after.
    hit: Vec<LogLine>,
    hit_at: usize,
    after: usize,
    prev: Option<LogLine>,
    summary: Option<String>,
}

#[derive(Default)]
struct Stdout {
    bytes: Vec<u8>,
    incomplete: bool,
}

pub const WITHHELD: &str = "[output withheld: scenario captures or consumes values]\n";

/// A chatty (or hostile) service must not take probatum down with it. Public
/// evidence keeps everything; sensitive scenarios withhold payloads. Memory
/// keeps the earliest lines, which is what diagnosis needs — the first
/// panic/error marker wins. Rules are evaluated per line as output arrives,
/// so this bound never changes a verdict. Named stdout capture has a separate
/// bounded buffer.
const MAX_LINES_IN_MEMORY: usize = 100_000;

impl CapturedLogs {
    pub fn stdout(&self) -> anyhow::Result<String> {
        let raw = self
            .stdout
            .as_ref()
            .expect("stdout capture enabled")
            .lock()
            .unwrap();
        if raw.incomplete {
            anyhow::bail!("stdout capture incomplete (read error or more than 1 MiB)");
        }
        String::from_utf8(raw.bytes.clone())
            .map_err(|_| anyhow::anyhow!("stdout capture is not UTF-8"))
    }
    /// The earliest lines, for diagnosis only — rules never read this.
    pub fn snapshot(&self) -> Vec<LogLine> {
        self.state.lock().unwrap().lines.clone()
    }
    /// First `contains` pattern seen on no line of the whole output.
    pub fn missing(&self) -> Option<String> {
        let st = self.state.lock().unwrap();
        st.rules
            .contains
            .iter()
            .zip(&st.found)
            .find(|(_, found)| !**found)
            .map(|(pattern, _)| pattern.clone())
    }
    /// The first forbidden line (not exempted by `allow`) and its context,
    /// with the index of the offending line within that context.
    pub fn forbidden(&self) -> Option<(Vec<LogLine>, usize)> {
        let st = self.state.lock().unwrap();
        (!st.hit.is_empty()).then(|| (st.hit.clone(), st.hit_at))
    }
    /// The last line that looks like a test summary ("test result: ok. …").
    pub fn summary(&self) -> Option<String> {
        self.state.lock().unwrap().summary.clone()
    }
    fn push(&self, line: LogLine) {
        let mut st = self.state.lock().unwrap();
        let st = &mut *st;
        for (pattern, found) in st.rules.contains.iter().zip(st.found.iter_mut()) {
            *found |= line.text.contains(pattern.as_str());
        }
        if st.after > 0 {
            st.hit.push(line.clone());
            st.after -= 1;
        } else if st.hit.is_empty()
            && st
                .rules
                .forbid
                .iter()
                .any(|p| line.text.contains(p.as_str()))
            && !st
                .rules
                .allow
                .iter()
                .any(|a| line.text.contains(a.as_str()))
        {
            st.hit.extend(st.prev.take());
            st.hit_at = st.hit.len();
            st.hit.push(line.clone());
            st.after = 2;
        }
        let t = line.text.trim();
        if t.contains("passed") || t.contains("test result:") || t.contains("ok.") {
            st.summary = Some(t.chars().take(90).collect());
        }
        st.prev = Some(line.clone());
        if st.lines.len() < MAX_LINES_IN_MEMORY {
            st.lines.push(line);
        }
    }
}

/// Attach capture threads to a child's stdout/stderr. Lines go to memory + evidence file.
pub fn attach(
    child: &mut Child,
    evidence_file: PathBuf,
    started: Instant,
    private: bool,
    collect_stdout: bool,
    rules: Rules,
) -> (CapturedLogs, Vec<JoinHandle<()>>) {
    let logs = CapturedLogs {
        stdout: collect_stdout.then(|| Arc::new(Mutex::new(Stdout::default()))),
        state: Arc::new(Mutex::new(State {
            found: vec![false; rules.contains.len()],
            rules,
            ..State::default()
        })),
    };
    // An unwritable evidence dir must not panic the runner: capture keeps
    // working in memory, and the caller is told the file does not exist
    // rather than being handed a path that was never written.
    let mut evidence = std::fs::File::create(&evidence_file).ok();
    if private {
        if let Some(file) = &mut evidence {
            let _ = file.write_all(WITHHELD.as_bytes());
        }
        evidence = None; // Never persist raw output, including during panic/signal exits.
    }
    let file = Arc::new(Mutex::new(evidence));
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
        let mut buf = BufReader::new(reader);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match buf.read_until(b'\n', &mut bytes) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => {
                    if source == "stdout" {
                        if let Some(raw) = &logs.stdout {
                            raw.lock().unwrap().incomplete = true;
                        }
                    }
                    break;
                }
            }
            if source == "stdout" {
                if let Some(raw) = &logs.stdout {
                    let mut raw = raw.lock().unwrap();
                    let remaining =
                        crate::values::MAX_CAPTURE_BYTES.saturating_sub(raw.bytes.len());
                    raw.incomplete |= bytes.len() > remaining;
                    raw.bytes
                        .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
                }
            }
            let text = String::from_utf8_lossy(&bytes)
                .trim_end_matches(['\r', '\n'])
                .to_string();
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
