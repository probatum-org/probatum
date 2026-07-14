//! Runs the checks in order, owns every process it starts, captures output live,
//! and surfaces only what matters: the extracted cause of each failure.
//!
//! Semantics frozen with the owner (see DISCUSSION.md):
//! - failed ≠ couldn't-run: a bad result is not the same as "couldn't observe";
//! - stop at the first failed/errored check, the rest is skipped (no cascade noise);
//! - external logs are read from run start (offset noted before any check runs);
//!   replacement/truncation during the window is ambiguous → couldn't-run;
//! - a port that already answers before we start our service = dirty environment.

use crate::capture::{self, CapturedLogs, LogLine};
use crate::diagnose::{self, Cause};
use crate::manifest::Check;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Status {
    Passed,
    Failed,  // it ran and gave a bad result
    Errored, // it couldn't run or couldn't observe (missing binary, dirty env, rotated log)
    Skipped, // not executed: an earlier check already failed
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CheckReport {
    pub label: String,
    pub status: Status,
    pub detail: Option<String>,
    pub cause: Option<Cause>,
    pub log_file: String,
}

#[derive(Debug, serde::Serialize)]
pub struct RunReport {
    pub schema: u32,
    pub verdict: String, // "pass" | "fail" | "couldn't-run"
    pub failed: usize,
    pub errored: usize,
    pub skipped: usize,
    pub source: String,
    pub run_dir: String,
    pub seed: u32,
    pub checks: Vec<CheckReport>,
    pub replay: String,
}

/// Crash markers that always count as a failure in a running service's logs.
/// They do NOT apply to a plain `run:` — its exit code is the authority
/// (a passing `cargo test` may legitimately print "panicked at").
const CRITICAL: [&str; 4] = [
    "panicked at",
    "FATAL",
    "Traceback (most recent call last)",
    "fatal:",
];

struct Service {
    child: Child,
    logs: CapturedLogs,
    report_index: usize,
    contains: Vec<String>,
    absent: Vec<String>,
    allow: Vec<String>,
}

/// (inode, size) of a log file at run start; None = the file did not exist yet.
type LogBaseline = Option<(u64, u64)>;

pub fn run(checks: &[Check], config_text: &str, source: &str, seed: u32) -> Result<RunReport> {
    // Safety net: if this function exits ANY way (return, ?, panic unwinding),
    // every registered process group gets killed. The explicit teardown below
    // stays as the controlled path; this guard is the last line of defense.
    let _own_guard = crate::own::Guard;

    let run_dir = next_run_dir()?;
    std::fs::create_dir_all(&run_dir)?;
    let frozen = run_dir.join("config.yaml");
    std::fs::write(&frozen, config_text).ok();

    // Note every log file's offset BEFORE any check runs: only lines written
    // during this run count. Pre-existing content is normal, not dirty.
    let mut baselines: HashMap<String, LogBaseline> = HashMap::new();
    for check in checks {
        if let Check::Log { path, .. } = check {
            let b = std::fs::metadata(path).ok().map(|m| (m.ino(), m.size()));
            baselines.insert(path.clone(), b);
        }
    }

    let mut services: Vec<Service> = Vec::new();
    let mut out: Vec<CheckReport> = Vec::new();
    let mut halted = false;

    for (i, check) in checks.iter().enumerate() {
        let log_file = run_dir.join(format!("check-{}.log", i + 1));
        if halted {
            out.push(CheckReport {
                label: check.label(),
                status: Status::Skipped,
                detail: None,
                cause: None,
                log_file: log_file.display().to_string(),
            });
            continue;
        }
        let report = match check {
            Check::Run { cmd, contains, absent, .. } => {
                run_cmd(cmd, contains, absent, &log_file, check)
            }
            Check::Service { cmd, ready, timeout_secs, contains, absent, allow, .. } => run_service(
                cmd,
                ready.as_deref(),
                *timeout_secs,
                contains,
                absent,
                allow,
                &log_file,
                check,
                out.len(),
                &mut services,
            ),
            Check::Get { url, expect, contains, .. } => {
                run_get(url, *expect, contains, &log_file, check)
            }
            Check::Log { path, contains, absent, .. } => {
                run_log(path, contains, absent, baselines.get(path).copied().flatten(), &log_file, check)
            }
        };
        if report.status == Status::Failed || report.status == Status::Errored {
            halted = true;
        }
        out.push(report);
    }

    // ponytail: test hook — proves the ownership guard on probatum's own crash
    // path (services are alive right here). Not a user feature.
    if std::env::var_os("PROBATUM_TEST_PANIC").is_some() {
        panic!("PROBATUM_TEST_PANIC");
    }

    // A service can misbehave after it became ready — scan every tracked
    // service's full output before the verdict.
    for svc in &services {
        if out[svc.report_index].status != Status::Passed {
            continue;
        }
        let lines = svc.logs.snapshot();
        if let Some(cause) = scan_lines(&lines, &svc.absent, &svc.allow, true) {
            out[svc.report_index].status = Status::Failed;
            out[svc.report_index].detail = Some("error in logs".into());
            out[svc.report_index].cause = Some(cause);
        } else if let Some(missing) = find_missing(&lines, &svc.contains) {
            out[svc.report_index].status = Status::Failed;
            out[svc.report_index].detail = Some(format!("output missing \"{missing}\""));
        }
    }

    // Teardown: kill every process group we started. The runner owns what it launches.
    for mut svc in services {
        let pid = svc.child.id() as i32;
        unsafe {
            libc::kill(-pid, libc::SIGKILL); // negative pid = the whole process group
        }
        let _ = svc.child.kill();
        let _ = svc.child.wait();
    }

    let failed = out.iter().filter(|c| c.status == Status::Failed).count();
    let errored = out.iter().filter(|c| c.status == Status::Errored).count();
    let skipped = out.iter().filter(|c| c.status == Status::Skipped).count();
    let verdict = if failed > 0 {
        "fail"
    } else if errored > 0 {
        "couldn't-run"
    } else {
        "pass"
    };

    let report = RunReport {
        schema: 1,
        verdict: verdict.into(),
        failed,
        errored,
        skipped,
        source: source.to_string(),
        run_dir: run_dir.display().to_string(),
        seed,
        checks: out,
        replay: format!("probatum run {} --seed {}", frozen.display(), seed),
    };
    std::fs::write(run_dir.join("run.json"), serde_json::to_string_pretty(&report)?).ok();
    Ok(report)
}

fn run_cmd(cmd: &str, contains: &[String], absent: &[String], log_file: &Path, check: &Check) -> CheckReport {
    let started = Instant::now();
    let spawned = {
        use std::os::unix::process::CommandExt;
        Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .process_group(0) // own group: a run that leaks background children gets swept too
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    };
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return errored(check, log_file, format!("couldn't run: {e}")),
    };
    crate::own::register(child.id());
    let (logs, handles) = capture::attach(&mut child, log_file.to_path_buf(), started);
    let status = child.wait();
    for h in handles {
        let _ = h.join();
    }
    let lines = logs.snapshot();

    match status {
        Ok(s) if s.success() => {
            // Exit 0 — but explicit rules still apply to the output.
            if let Some(cause) = scan_lines(&lines, absent, &[], false) {
                return report(check, log_file, Status::Failed, Some("error in output".into()), Some(cause));
            }
            if let Some(missing) = find_missing(&lines, contains) {
                return report(check, log_file, Status::Failed, Some(format!("output missing \"{missing}\"")), None);
            }
            report(check, log_file, Status::Passed, summarize(&lines), None)
        }
        Ok(s) => {
            let cause = diagnose::from_logs(&lines).or_else(|| {
                Some(Cause {
                    headline: format!("exited {}", fmt_status(&s)),
                    correlated: diagnose::tail(&lines, 5),
                })
            });
            report(check, log_file, Status::Failed, Some(format!("exited {}", fmt_status(&s))), cause)
        }
        Err(e) => errored(check, log_file, format!("couldn't run: {e}")),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_service(
    cmd: &str,
    ready: Option<&str>,
    timeout_secs: u64,
    contains: &[String],
    absent: &[String],
    allow: &[String],
    log_file: &Path,
    check: &Check,
    report_index: usize,
    services: &mut Vec<Service>,
) -> CheckReport {
    // Dirty environment: if the readiness URL already answers before we start,
    // something else is on that port — running against it would test the wrong thing.
    if let Some(url) = ready {
        if crate::http::get(url, Duration::from_millis(300)).is_ok() {
            return errored(
                check,
                log_file,
                format!("environment not clean: {url} already answers before start"),
            );
        }
    }

    let started = Instant::now();
    let spawned = {
        use std::os::unix::process::CommandExt;
        Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .process_group(0) // own group so teardown kills the whole tree, not just sh
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    };
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return errored(check, log_file, format!("couldn't start: {e}")),
    };
    crate::own::register(child.id());
    let (logs, _handles) = capture::attach(&mut child, log_file.to_path_buf(), started);

    let track = |services: &mut Vec<Service>, child, logs: &CapturedLogs| {
        services.push(Service {
            child,
            logs: logs.clone(),
            report_index,
            contains: contains.to_vec(),
            absent: absent.to_vec(),
            allow: allow.to_vec(),
        });
    };

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        // Died before becoming ready — that's the interesting failure.
        if let Ok(Some(status)) = child.try_wait() {
            std::thread::sleep(Duration::from_millis(120)); // let capture drain
            let lines = logs.snapshot();
            let cause = diagnose::from_logs(&lines).or_else(|| {
                Some(Cause {
                    headline: format!("crashed at startup (exit {})", fmt_status(&status)),
                    correlated: diagnose::tail(&lines, 5),
                })
            });
            track(services, child, &logs); // keep it: teardown reaps any group children
            return report(
                check,
                log_file,
                Status::Failed,
                Some(format!("crashed at startup after {:.1}s", started.elapsed().as_secs_f32())),
                cause,
            );
        }
        if let Some(url) = ready {
            if let Ok(resp) = crate::http::get(url, Duration::from_millis(500)) {
                if (200..300).contains(&resp.status) {
                    track(services, child, &logs);
                    return report(
                        check,
                        log_file,
                        Status::Passed,
                        Some(format!("ready in {:.1}s", started.elapsed().as_secs_f32())),
                        None,
                    );
                }
            }
        } else if started.elapsed() > Duration::from_millis(500) {
            // No readiness probe: consider started after a short grace period.
            track(services, child, &logs);
            return report(check, log_file, Status::Passed, Some("started".into()), None);
        }
        if Instant::now() > deadline {
            let lines = logs.snapshot();
            let cause = diagnose::from_logs(&lines);
            track(services, child, &logs); // teardown will kill the group — no orphan
            return report(check, log_file, Status::Failed, Some(format!("not ready in {timeout_secs}s")), cause);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn run_get(url: &str, expect: Option<u16>, contains: &[String], log_file: &Path, check: &Check) -> CheckReport {
    match crate::http::get(url, Duration::from_secs(5)) {
        Ok(resp) => {
            // Evidence: what we actually observed.
            let head: String = resp.body.lines().take(20).collect::<Vec<_>>().join("\n");
            let _ = std::fs::write(log_file, format!("GET {url}\nHTTP {}\n\n{head}\n", resp.status));

            let status_ok = match expect {
                Some(code) => resp.status == code,
                None => (200..300).contains(&resp.status), // default: any 2xx
            };
            if !status_ok {
                let expected = expect.map(|c| c.to_string()).unwrap_or_else(|| "2xx".into());
                return report(
                    check,
                    log_file,
                    Status::Failed,
                    Some(format!("HTTP {} (expected {expected})", resp.status)),
                    Some(Cause {
                        headline: format!("unexpected HTTP {}", resp.status),
                        correlated: resp.body.lines().take(3).map(String::from).collect(),
                    }),
                );
            }
            if let Some(missing) = contains.iter().find(|p| !resp.body.contains(p.as_str())) {
                return report(
                    check,
                    log_file,
                    Status::Failed,
                    Some(format!("body missing \"{missing}\"")),
                    Some(Cause {
                        headline: format!("HTTP {} but body doesn't contain \"{missing}\"", resp.status),
                        correlated: resp.body.lines().take(3).map(String::from).collect(),
                    }),
                );
            }
            report(check, log_file, Status::Passed, Some(format!("HTTP {}", resp.status)), None)
        }
        Err(e) => errored(check, log_file, format!("couldn't reach: {e}")),
    }
}

fn run_log(
    path: &str,
    contains: &[String],
    absent: &[String],
    baseline: LogBaseline,
    log_file: &Path,
    check: &Check,
) -> CheckReport {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return errored(check, log_file, format!("log file not found: {path}")),
    };
    // The window is [offset at run start .. now]. A replaced or truncated file
    // makes the window ambiguous: we can no longer say what happened during the run.
    let offset = match baseline {
        Some((ino, size)) => {
            if meta.ino() != ino {
                return errored(check, log_file, "log file was replaced during the run — window is ambiguous".into());
            }
            if meta.size() < size {
                return errored(check, log_file, "log file was truncated during the run — window is ambiguous".into());
            }
            size
        }
        None => 0, // didn't exist at run start: everything in it was written during the run
    };

    use std::io::{Read, Seek, SeekFrom};
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return errored(check, log_file, format!("couldn't read log: {e}")),
    };
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(offset)).is_err() || f.read_to_end(&mut buf).is_err() {
        return errored(check, log_file, format!("couldn't read log: {path}"));
    }
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().collect();
    let _ = std::fs::write(log_file, text.as_bytes()); // evidence: the observed window

    if let Some(idx) = lines.iter().position(|l| absent.iter().any(|p| l.contains(p.as_str()))) {
        let hit = absent.iter().find(|p| lines[idx].contains(p.as_str())).unwrap();
        let lo = idx.saturating_sub(1);
        let hi = (idx + 3).min(lines.len());
        return report(
            check,
            log_file,
            Status::Failed,
            Some(format!("found \"{hit}\"")),
            Some(Cause {
                headline: lines[idx].trim().to_string(),
                correlated: lines[lo..hi].iter().map(|s| s.trim_end().to_string()).collect(),
            }),
        );
    }
    if let Some(missing) = contains.iter().find(|p| !lines.iter().any(|l| l.contains(p.as_str()))) {
        return report(
            check,
            log_file,
            Status::Failed,
            Some(format!("\"{missing}\" not found ({} new line(s))", lines.len())),
            None,
        );
    }
    report(check, log_file, Status::Passed, Some(format!("{} new line(s) checked", lines.len())), None)
}

/// First line matching a forbidden pattern (optionally including the default
/// crash markers), unless an `allow` pattern exempts it.
fn scan_lines(lines: &[LogLine], absent: &[String], allow: &[String], with_defaults: bool) -> Option<Cause> {
    let hit = |l: &LogLine| {
        let matched = (with_defaults && CRITICAL.iter().any(|m| l.text.contains(m)))
            || absent.iter().any(|p| l.text.contains(p.as_str()));
        matched && !allow.iter().any(|a| l.text.contains(a.as_str()))
    };
    let idx = lines.iter().position(hit)?;
    let lo = idx.saturating_sub(1);
    let hi = (idx + 3).min(lines.len());
    Some(Cause {
        headline: lines[idx].text.trim().to_string(),
        correlated: lines[lo..hi]
            .iter()
            .map(|l| format!("[{:>6}ms {}] {}", l.at_ms, l.source, l.text.trim_end()))
            .collect(),
    })
}

/// First `contains` pattern that appears nowhere in the output.
fn find_missing<'a>(lines: &[LogLine], contains: &'a [String]) -> Option<&'a String> {
    contains.iter().find(|p| !lines.iter().any(|l| l.text.contains(p.as_str())))
}

fn summarize(lines: &[LogLine]) -> Option<String> {
    for l in lines.iter().rev() {
        let t = l.text.trim();
        if t.contains("passed") || t.contains("test result:") || t.contains("ok.") {
            return Some(t.chars().take(90).collect());
        }
    }
    None
}

fn report(check: &Check, log_file: &Path, status: Status, detail: Option<String>, cause: Option<Cause>) -> CheckReport {
    CheckReport {
        label: check.label(),
        status,
        detail,
        cause,
        log_file: log_file.display().to_string(),
    }
}

fn errored(check: &Check, log_file: &Path, msg: String) -> CheckReport {
    let _ = std::fs::write(log_file, format!("{msg}\n")); // evidence file always exists
    report(check, log_file, Status::Errored, Some(msg), None)
}

fn fmt_status(s: &std::process::ExitStatus) -> String {
    match s.code() {
        Some(c) => c.to_string(),
        None => s.to_string(), // killed by signal
    }
}

fn next_run_dir() -> Result<PathBuf> {
    let base = PathBuf::from(".probatum/runs");
    std::fs::create_dir_all(&base).context("create .probatum/runs")?;
    let mut max = 0u32;
    for entry in std::fs::read_dir(&base)? {
        if let Ok(name) = entry.map(|e| e.file_name().to_string_lossy().into_owned()) {
            if let Ok(n) = name.parse::<u32>() {
                max = max.max(n);
            }
        }
    }
    Ok(base.join(format!("{:04}", max + 1)))
}
