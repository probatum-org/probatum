//! The runner: executes steps sequentially, owns every process it spawns,
//! captures everything, evaluates oracles, and produces the evidence dir.

use crate::capture::{self, CapturedLogs, LogLine};
use crate::diagnose::{self, Cause};
use crate::manifest::{Manifest, Oracle, Step};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub enum Status {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StepReport {
    pub index: usize,
    pub label: String,
    pub status: Status,
    pub duration_ms: u128,
    pub detail: Option<String>,
    pub cause: Option<Cause>,
    pub log_file: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OracleReport {
    pub label: String,
    pub held: bool,
    pub detail: Option<String>,
    pub violations: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct RunReport {
    pub proof: String,
    pub promise_id: String,
    pub promise_statement: String,
    pub verdict: String, // "held" | "violated"
    pub seed: u32,
    pub run_dir: String,
    pub steps: Vec<StepReport>,
    pub oracles: Vec<OracleReport>,
    pub context_notes: Vec<String>,
    pub replay: String,
}

struct Service {
    child: Child,
    logs: CapturedLogs,
    label: String,
}

pub fn run(manifest: &Manifest, manifest_path: &std::path::Path, seed: u32) -> Result<RunReport> {
    let run_dir = next_run_dir()?;
    std::fs::create_dir_all(&run_dir)?;
    std::fs::copy(manifest_path, run_dir.join("manifest.yaml")).ok();

    let started = Instant::now();
    let mut services: Vec<Service> = Vec::new();
    let mut steps_out: Vec<StepReport> = Vec::new();
    let mut failed = false;

    for (i, step) in manifest.steps.iter().enumerate() {
        let log_file = run_dir.join(format!("step-{}-{}.log", i + 1, step.name().replace('.', "-")));
        if failed {
            steps_out.push(StepReport {
                index: i + 1,
                label: step.label(),
                status: Status::Skipped,
                duration_ms: 0,
                detail: None,
                cause: None,
                log_file: log_file.display().to_string(),
            });
            continue;
        }
        let t0 = Instant::now();
        let report = match step {
            Step::SuiteRun { cmd } => run_suite(cmd, &log_file, i, step),
            Step::ServiceStart { cmd, ready, timeout_secs } => {
                run_service(cmd, ready.as_deref(), *timeout_secs, &log_file, i, step, &mut services)
            }
            Step::ApiCheck { get, expect } => run_api_check(get, *expect, &log_file, i, step),
        };
        let mut report = report;
        report.duration_ms = t0.elapsed().as_millis();
        if report.status == Status::Failed {
            failed = true;
        }
        steps_out.push(report);
    }

    // Oracles evaluate over everything the services logged, even after a failure.
    let mut oracles_out = Vec::new();
    for oracle in &manifest.oracles {
        oracles_out.push(eval_oracle(oracle, &services));
    }

    // Teardown: the runner never leaves an orphan environment behind.
    // Services run in their own process group — kill the whole group natively.
    for mut svc in services {
        let pid = svc.child.id() as i32;
        unsafe {
            libc::kill(-pid, libc::SIGKILL); // negative pid = the entire process group
        }
        let _ = svc.child.kill();
        let _ = svc.child.wait();
    }

    let context_notes = build_context_notes(&steps_out);
    let violated = failed || oracles_out.iter().any(|o| !o.held);

    let report = RunReport {
        proof: manifest.proof.clone(),
        promise_id: manifest.promise.id.clone(),
        promise_statement: manifest.promise.statement.clone(),
        verdict: if violated { "violated".into() } else { "held".into() },
        seed,
        run_dir: run_dir.display().to_string(),
        steps: steps_out,
        oracles: oracles_out,
        context_notes,
        replay: format!("probatum run {} --seed {}", manifest_path.display(), seed),
    };

    std::fs::write(run_dir.join("run.json"), serde_json::to_string_pretty(&report)?)?;
    let _ = started; // reserved for global timing in later versions
    Ok(report)
}

fn run_suite(cmd: &str, log_file: &PathBuf, i: usize, step: &Step) -> StepReport {
    let started = Instant::now();
    let spawned = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return failed_step(i, step, log_file, format!("cannot spawn: {e}"), None),
    };
    let (logs, handles) = capture::attach(&mut child, log_file.clone(), started);
    let status = child.wait();
    for h in handles {
        let _ = h.join();
    }
    let lines = logs.snapshot();

    match status {
        Ok(s) if s.success() => StepReport {
            index: i + 1,
            label: step.label(),
            status: Status::Passed,
            duration_ms: 0,
            detail: summarize_suite(&lines),
            cause: None,
            log_file: log_file.display().to_string(),
        },
        Ok(s) => {
            let cause = diagnose::from_logs(&lines).or_else(|| {
                Some(Cause {
                    headline: format!("suite exited with {s}"),
                    correlated: diagnose::tail(&lines, 5),
                })
            });
            failed_step(i, step, log_file, format!("exit {}", fmt_status(&s)), cause)
        }
        Err(e) => failed_step(i, step, log_file, format!("wait failed: {e}"), None),
    }
}

fn run_service(
    cmd: &str,
    ready: Option<&str>,
    timeout_secs: u64,
    log_file: &PathBuf,
    i: usize,
    step: &Step,
    services: &mut Vec<Service>,
) -> StepReport {
    let started = Instant::now();
    let spawned = {
        use std::os::unix::process::CommandExt;
        Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .process_group(0) // own group: teardown kills the whole tree, never just the sh
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    };

    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return failed_step(i, step, log_file, format!("cannot spawn: {e}"), None),
    };
    let (logs, _handles) = capture::attach(&mut child, log_file.clone(), started);

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        // Did the process die before becoming ready? That's the interesting failure.
        if let Ok(Some(status)) = child.try_wait() {
            std::thread::sleep(Duration::from_millis(120)); // let capture threads drain
            let lines = logs.snapshot();
            let cause = diagnose::from_logs(&lines).or_else(|| {
                Some(Cause {
                    headline: format!("process exited with {status} before becoming ready"),
                    correlated: diagnose::tail(&lines, 5),
                })
            });
            return failed_step(
                i,
                step,
                log_file,
                format!("exit {} après {:.1}s", fmt_status(&status), started.elapsed().as_secs_f32()),
                cause,
            );
        }
        if let Some(url) = ready {
            if let Ok(resp) = crate::http::get(url, Duration::from_millis(500)) {
                if resp.status == 200 {
                    services.push(Service {
                        child,
                        logs,
                        label: step.label(),
                    });
                    return StepReport {
                        index: i + 1,
                        label: step.label(),
                        status: Status::Passed,
                        duration_ms: 0,
                        detail: Some(format!("ready en {:.1}s", started.elapsed().as_secs_f32())),
                        cause: None,
                        log_file: log_file.display().to_string(),
                    };
                }
            }
        } else {
            // No readiness probe: consider started after a grace period.
            if started.elapsed() > Duration::from_millis(500) {
                services.push(Service { child, logs, label: step.label() });
                return StepReport {
                    index: i + 1,
                    label: step.label(),
                    status: Status::Passed,
                    duration_ms: 0,
                    detail: None,
                    cause: None,
                    log_file: log_file.display().to_string(),
                };
            }
        }
        if Instant::now() > deadline {
            let lines = logs.snapshot();
            let cause = diagnose::from_logs(&lines);
            let _ = child.kill();
            return failed_step(
                i,
                step,
                log_file,
                format!("pas prêt en {timeout_secs}s (readiness: {})", ready.unwrap_or("-")),
                cause,
            );
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn run_api_check(url: &str, expect: u16, log_file: &PathBuf, i: usize, step: &Step) -> StepReport {
    match crate::http::get(url, Duration::from_secs(5)) {
        Ok(resp) if resp.status == expect => StepReport {
            index: i + 1,
            label: step.label(),
            status: Status::Passed,
            duration_ms: 0,
            detail: Some(format!("HTTP {}", resp.status)),
            cause: None,
            log_file: log_file.display().to_string(),
        },
        Ok(resp) => failed_step(
            i,
            step,
            log_file,
            format!("HTTP {} (attendu {})", resp.status, expect),
            Some(Cause {
                headline: format!("réponse inattendue: HTTP {}", resp.status),
                correlated: resp.body.lines().take(3).map(String::from).collect(),
            }),
        ),
        Err(e) => failed_step(i, step, log_file, format!("requête échouée: {e}"), None),
    }
}

fn eval_oracle(oracle: &Oracle, services: &[Service]) -> OracleReport {
    match oracle {
        Oracle::LogsClean { allow } => {
            let mut violations = Vec::new();
            for svc in services {
                for line in svc.logs.snapshot() {
                    let is_error = ["ERROR", "error:", "FATAL", "panicked at"]
                        .iter()
                        .any(|m| line.text.contains(m));
                    let allowed = allow.iter().any(|a| line.text.contains(a));
                    if is_error && !allowed {
                        violations.push(format!(
                            "{} → [{:>6}ms] {}",
                            svc.label,
                            line.at_ms,
                            line.text.trim_end()
                        ));
                    }
                }
            }
            OracleReport {
                label: oracle.label(),
                held: violations.is_empty(),
                detail: (!violations.is_empty())
                    .then(|| format!("{} ligne(s) d'erreur non autorisées", violations.len())),
                violations,
            }
        }
    }
}

fn build_context_notes(steps: &[StepReport]) -> Vec<String> {
    let mut notes = Vec::new();
    let suite_passed = steps
        .iter()
        .any(|s| s.label.starts_with("suite.run") && s.status == Status::Passed);
    let boot_failed = steps
        .iter()
        .find(|s| s.label.starts_with("service.start") && s.status == Status::Failed);
    if let (true, Some(fs)) = (suite_passed, boot_failed) {
        if let Some(suite) = steps.iter().find(|s| s.label.starts_with("suite.run")) {
            let suite_info = suite.detail.clone().unwrap_or_else(|| "OK".into());
            notes.push(format!(
                "suite.run OK ({suite_info}) — l'échec n'apparaît qu'au démarrage réel ({})",
                fs.detail.clone().unwrap_or_default()
            ));
        }
    }
    notes
}

fn summarize_suite(lines: &[LogLine]) -> Option<String> {
    // Recognize common test-summary shapes (cargo test, pytest, generic).
    for l in lines.iter().rev() {
        let t = l.text.trim();
        if t.contains("passed") || t.contains("test result:") || t.contains("ok.") {
            return Some(t.chars().take(90).collect());
        }
    }
    None
}

fn failed_step(
    i: usize,
    step: &Step,
    log_file: &PathBuf,
    detail: String,
    cause: Option<Cause>,
) -> StepReport {
    StepReport {
        index: i + 1,
        label: step.label(),
        status: Status::Failed,
        duration_ms: 0,
        detail: Some(detail),
        cause,
        log_file: log_file.display().to_string(),
    }
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
