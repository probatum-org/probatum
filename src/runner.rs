//! Runs the checks in order, owns every process it starts, captures output live,
//! and surfaces only what matters: the extracted cause of each failure.
//!
//! Semantics frozen with the owner (see DISCUSSION.md):
//! - failed ≠ couldn't-run: a bad result is not the same as "couldn't observe";
//! - stop at the first failed/errored check, the rest is skipped (no cascade noise);
//! - external logs are read from scenario start (before its checks run);
//!   replacement/truncation during the window is ambiguous → couldn't-run;
//! - a port that already answers before we start our service = dirty environment.

use crate::capture::{self, CapturedLogs, LogLine};
use crate::diagnose::{self, Cause};
use crate::manifest::{Check, Manifest, Scenario, ScopedCheck};
use crate::values::{self, Plan, Store};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Status {
    Passed,
    Failed,   // it ran and gave a bad result
    Errored,  // it couldn't run or couldn't observe (missing binary, dirty env, rotated log)
    Skipped,  // not executed: an earlier check already failed
    Excluded, // not selected or outside the current OS scope
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NonExecutionReason {
    NotSelected,
    OsMismatch,
    PreviousFailure,
    DependencyUnavailable,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CheckReport {
    pub scenario: String,
    pub step: Option<usize>,
    pub captures: Vec<String>,
    pub output_withheld: bool,
    pub label: String,
    pub status: Status,
    /// How long the check took. Recorded for every check so a reader (or an
    /// agent diffing two run.json files) can see something getting slower
    /// even when nothing fails. Not a metrics system — just the number.
    pub duration_ms: u128,
    pub detail: Option<String>,
    pub cause: Option<Cause>,
    pub log_file: Option<String>,
    pub reason: Option<NonExecutionReason>,
}

/// The run half of the outcome document. `schema` and `verdict` live on the
/// envelope (main.rs) so that an outcome with no run — an invalid config —
/// still carries them.
#[derive(Debug, serde::Serialize)]
pub struct RunReport {
    #[serde(skip)]
    pub verdict: String, // "pass" | "fail" | "couldn't-run"
    pub failed: usize,
    pub errored: usize,
    pub skipped: usize,
    pub executed: usize,
    pub excluded: usize,
    pub host_os: String,
    pub selected_scenario: Option<String>,
    pub prerequisites: Vec<String>,
    pub reason: Option<&'static str>,
    pub source: String,
    pub run_dir: String,
    pub seed: u32,
    pub checks: Vec<CheckReport>,
    pub replay: String,
}

/// Default markers that fail a running service's logs (crash class + error
/// class, per the frozen contract: "panic, traceback, FATAL, ERROR out of the
/// box"). They do NOT apply to a plain `run:` — its exit code is the authority
/// (a passing `cargo test` may legitimately print "panicked at").
const CRITICAL: [&str; 6] = [
    "panicked at",
    "FATAL",
    "Traceback (most recent call last)",
    "fatal:",
    "ERROR",
    "error:",
];

struct Service {
    child: Child,
    logs: CapturedLogs,
    report_index: usize,
    contains: Vec<String>,
    absent: Vec<String>,
    allow: Vec<String>,
    handles: Vec<std::thread::JoinHandle<()>>,
}

/// (inode, size) at scenario start; None = the file did not exist yet.
type LogBaseline = Option<(u64, u64)>;

struct Evidence {
    path: PathBuf,
    private: bool,
}

impl Evidence {
    fn write(&self, content: impl AsRef<[u8]>) {
        let bytes = if self.private {
            capture::WITHHELD.as_bytes()
        } else {
            content.as_ref()
        };
        let _ = std::fs::write(&self.path, bytes);
    }
}

#[derive(Default)]
struct RunState {
    out: Vec<CheckReport>,
    halted: bool,
    values: Store,
}

pub fn run(
    manifest: &Manifest,
    config_text: &str,
    source: &str,
    seed: u32,
    selected: Option<&str>,
    plan: &Plan,
) -> Result<RunReport> {
    let run_dir = next_run_dir()?;
    let frozen = run_dir.join("config.toml");
    std::fs::write(&frozen, config_text).context("write frozen config")?;
    let host_os = std::env::consts::OS;
    let (order, included) = plan.execution(manifest, selected, host_os);
    let prerequisites = order
        .iter()
        .filter(|&&i| {
            included[i] && selected.is_some_and(|name| name != manifest.scenarios[i].name)
        })
        .map(|&i| manifest.scenarios[i].name.clone())
        .collect();
    let mut state = RunState::default();
    for i in order {
        run_scenario(
            &manifest.scenarios[i],
            i,
            &run_dir,
            host_os,
            included[i],
            plan,
            &mut state,
        );
    }
    let out = state.out;

    let failed = out.iter().filter(|c| c.status == Status::Failed).count();
    let errored = out.iter().filter(|c| c.status == Status::Errored).count();
    let skipped = out.iter().filter(|c| c.status == Status::Skipped).count();
    let excluded = out.iter().filter(|c| c.status == Status::Excluded).count();
    let executed = out.len() - skipped - excluded;
    let blocked = out
        .iter()
        .any(|c| c.reason == Some(NonExecutionReason::DependencyUnavailable));
    let verdict = if failed > 0 {
        "fail"
    } else if errored > 0 || executed == 0 || blocked {
        "couldn't-run"
    } else {
        "pass"
    };
    let mut replay = format!(
        "probatum run {} --seed {seed}",
        shell_quote(&frozen.display().to_string())
    );
    if let Some(name) = selected {
        replay.push_str(&format!(" --scenario {}", shell_quote(name)));
    }
    Ok(RunReport {
        verdict: verdict.into(),
        failed,
        errored,
        skipped,
        executed,
        excluded,
        host_os: host_os.into(),
        selected_scenario: selected.map(String::from),
        prerequisites,
        reason: if blocked {
            Some("dependency_unavailable")
        } else {
            (executed == 0).then_some("no_applicable_checks")
        },
        source: source.to_string(),
        run_dir: run_dir.display().to_string(),
        seed,
        checks: out,
        replay,
    })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn exclusion(
    scenario: &Scenario,
    scoped: &ScopedCheck,
    included: bool,
    host_os: &str,
) -> Option<(NonExecutionReason, String)> {
    if !included {
        return Some((
            NonExecutionReason::NotSelected,
            "not selected or required as a prerequisite".into(),
        ));
    }
    if let Some(os) = scenario.os.or(scoped.os) {
        if !os.matches(host_os) {
            return Some((
                NonExecutionReason::OsMismatch,
                format!(
                    "out of scope: requires {}, running on {host_os}",
                    os.as_str()
                ),
            ));
        }
    }
    None
}

fn not_run(
    scenario: &Scenario,
    check: &Check,
    status: Status,
    reason: NonExecutionReason,
    detail: String,
) -> CheckReport {
    CheckReport {
        scenario: scenario.name.clone(),
        step: None,
        captures: Vec::new(),
        output_withheld: false,
        label: check.label(),
        status,
        duration_ms: 0,
        detail: Some(detail),
        cause: None,
        log_file: None,
        reason: Some(reason),
    }
}

fn run_scenario(
    scenario: &Scenario,
    scenario_index: usize,
    run_dir: &Path,
    host_os: &str,
    included: bool,
    plan: &Plan,
    state: &mut RunState,
) {
    let RunState {
        out,
        halted,
        values,
    } = state;
    let private = plan.sensitive[scenario_index];
    let _own_guard = crate::own::Guard;
    let exclusions: Vec<_> = scenario
        .checks
        .iter()
        .map(|c| exclusion(scenario, c, included, host_os))
        .collect();
    // A fresh observation window, before any applicable check in this scenario.
    // Excluded targets are never touched, even for baseline collection.
    let mut baselines: HashMap<String, LogBaseline> = HashMap::new();
    if !*halted {
        for (scoped, excluded) in scenario.checks.iter().zip(&exclusions) {
            if excluded.is_none() {
                if let Check::Log { path, .. } = &scoped.check {
                    let b = std::fs::metadata(path).ok().map(|m| (m.ino(), m.size()));
                    baselines.insert(path.clone(), b);
                }
            }
        }
    }
    let mut services = Vec::new();
    let mut jar = HashMap::new();
    let first_report = out.len();
    let mut executed = false;
    for (check_index, (scoped, excluded)) in scenario.checks.iter().zip(exclusions).enumerate() {
        let check = &scoped.check;
        if let Some((reason, detail)) = excluded {
            out.push(not_run(scenario, check, Status::Excluded, reason, detail));
            continue;
        }
        if *halted {
            out.push(not_run(
                scenario,
                check,
                Status::Skipped,
                NonExecutionReason::PreviousFailure,
                "skipped after an earlier failure or error".into(),
            ));
            continue;
        }
        if let Some(missing) = plan.references[scenario_index][check_index]
            .iter()
            .find(|key| !values.contains_key(*key))
        {
            out.push(not_run(
                scenario,
                check,
                Status::Skipped,
                NonExecutionReason::DependencyUnavailable,
                format!("required capture {missing:?} is unavailable"),
            ));
            continue;
        }
        executed = true;
        let log_file = Evidence {
            path: run_dir.join(format!("check-{}.log", out.len() + 1)),
            private,
        };
        let check_started = Instant::now();
        let (resolved, env) = match values::resolve(scoped, values) {
            Ok(resolved) => resolved,
            Err(error) => {
                let mut report = errored(check, &log_file, error.to_string());
                report.scenario = scenario.name.clone();
                report.step = scoped.step;
                report.output_withheld = private;
                out.push(report);
                *halted = true;
                continue;
            }
        };
        let check = &resolved;
        let mut captured = None;
        let output = (!scoped.captures.is_empty()).then_some(&mut captured);
        let report = match check {
            Check::Run {
                cmd,
                contains,
                absent,
                timeout_secs,
                expect,
                ..
            } => run_cmd(
                cmd,
                contains,
                absent,
                *timeout_secs,
                *expect,
                &log_file,
                check,
                &env,
                output,
            ),
            Check::Service {
                cmd,
                ready,
                timeout_secs,
                contains,
                absent,
                allow,
                ..
            } => run_service(
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
                &env,
            ),
            Check::Http {
                method,
                url,
                body,
                headers,
                expect,
                contains,
                absent,
                timeout_secs,
                max_ms,
                min_ms,
                ..
            } => run_http(
                method,
                url,
                body.as_deref(),
                headers,
                *expect,
                contains,
                absent,
                *timeout_secs,
                (*min_ms, *max_ms),
                &log_file,
                check,
                &mut jar,
                output,
            ),
            Check::Log {
                path,
                contains,
                absent,
                ..
            } => run_log(
                path,
                contains,
                absent,
                baselines.get(path).copied().flatten(),
                &log_file,
                check,
            ),
        };
        let mut report = report;
        report.label = scoped.check.label(); // Keep references, never expanded secrets.
        if report.status == Status::Passed && !scoped.captures.is_empty() {
            match captured.unwrap_or_else(|| Err(anyhow::anyhow!("capture output unavailable"))) {
                Err(error) => {
                    report.status = Status::Errored;
                    report.detail = Some(error.to_string());
                }
                Ok(output) => match values::extract(&scoped.captures, &output) {
                    Err(error) => {
                        report.status = Status::Failed;
                        report.detail = Some(error.to_string());
                    }
                    Ok(captures) => {
                        for (name, value) in captures {
                            values.insert(values::key(&scenario.name, scoped.step, &name), value);
                            report.captures.push(name);
                        }
                    }
                },
            }
        }
        if private {
            withhold_output(&mut report, check, values); // after extraction: its values are known
        }
        report.scenario = scenario.name.clone();
        report.step = scoped.step;
        report.duration_ms = check_started.elapsed().as_millis();
        if matches!(report.status, Status::Failed | Status::Errored) {
            *halted = true;
        }
        out.push(report);
    }
    if executed && std::env::var_os("PROBATUM_TEST_PANIC").is_some() {
        panic!("PROBATUM_TEST_PANIC");
    }
    finish_services(services, out);
    for (report, scoped) in out[first_report..].iter_mut().zip(&scenario.checks) {
        report.step = scoped.step;
        report.output_withheld =
            private && !matches!(report.status, Status::Skipped | Status::Excluded);
        // Service verdicts may change during teardown. Their causes remain private.
        if private && matches!(scoped.check, Check::Service { .. }) {
            withhold_output(report, &scoped.check, values);
        }
    }
    *halted |= out[first_report..]
        .iter()
        .any(|r| matches!(r.status, Status::Failed | Status::Errored));
}

/// In a scenario that captures or consumes values, what the system under test
/// said stays private: the cause (body/log excerpts) and a command's output
/// summary are dropped, and evidence files are withheld at the source. The
/// detail probatum wrote itself — status, network error, the rule that did not
/// hold — is kept, with every captured value replaced, so a failure still says
/// what went wrong.
fn withhold_output(report: &mut CheckReport, check: &Check, values: &Store) {
    if matches!(report.status, Status::Skipped | Status::Excluded) {
        return;
    }
    report.cause = None;
    report.output_withheld = true;
    if report.status == Status::Passed && matches!(check, Check::Run { .. }) {
        report.detail = None; // the summary is a line of the command's own output
    }
    if let Some(detail) = &mut report.detail {
        *detail = redact(detail, values);
    }
}

/// Replace every captured value's text in a probatum-authored message.
fn redact(text: &str, values: &Store) -> String {
    let mut secrets: Vec<String> = values
        .values()
        .map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        // ponytail: values under 4 chars (a count, a flag) are left alone —
        // redacting "2" would shred "HTTP 201 in 2ms"; a real credential is
        // never that short. Tighten if short secrets ever appear.
        .filter(|s| s.chars().count() >= 4)
        .collect();
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len())); // longest first: no partial overlap
    let mut out = text.to_string();
    for secret in &secrets {
        out = out.replace(secret.as_str(), "[redacted]");
    }
    out
}

fn kill_group(child: &mut Child) {
    signal_group(child);
    let _ = child.wait();
    crate::own::unregister(child.id());
}

fn signal_group(child: &mut Child) {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
}

fn finish_services(mut services: Vec<Service>, out: &mut [CheckReport]) {
    // Inspect all services before any teardown; stopping one service can cause
    // another to exit. An already-dead service is a failure even without logs.
    let statuses: Vec<_> = services.iter_mut().map(|s| s.child.try_wait()).collect();
    for svc in &mut services {
        signal_group(&mut svc.child);
    }
    for (mut svc, status) in services.into_iter().zip(statuses) {
        let _ = svc.child.wait();
        crate::own::unregister(svc.child.id());
        for handle in svc.handles {
            let _ = handle.join();
        }
        let r = &mut out[svc.report_index];
        if r.status != Status::Passed {
            continue;
        }
        let lines = svc.logs.snapshot();
        match status {
            Ok(Some(status)) => {
                r.status = Status::Failed;
                r.detail = Some(format!(
                    "service exited after startup ({})",
                    fmt_status(&status)
                ));
                r.cause = diagnose::from_logs(&lines);
            }
            Err(e) => {
                r.status = Status::Errored;
                r.detail = Some(format!("couldn't observe service: {e}"));
            }
            Ok(None) => {
                if let Some(cause) = scan_lines(&lines, &svc.absent, &svc.allow, true) {
                    r.status = Status::Failed;
                    r.detail = Some("error in logs".into());
                    r.cause = Some(cause);
                } else if let Some(missing) = find_missing(&lines, &svc.contains) {
                    r.status = Status::Failed;
                    r.detail = Some(format!("output missing \"{missing}\""));
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_cmd(
    cmd: &str,
    contains: &[String],
    absent: &[String],
    timeout_secs: Option<u64>,
    expect: i64,
    log_file: &Evidence,
    check: &Check,
    env: &[(String, String)],
    output: Option<&mut Option<Result<String>>>,
) -> CheckReport {
    let started = Instant::now();
    let spawned = {
        use std::os::unix::process::CommandExt;
        Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .envs(env.iter().map(|(name, value)| (name, value)))
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
    let (logs, handles) = capture::attach(
        &mut child,
        log_file.path.clone(),
        started,
        log_file.private,
        output.is_some(),
    );

    let status = match timeout_secs {
        None => child.wait(),
        Some(secs) => {
            let deadline = Instant::now() + Duration::from_secs(secs);
            loop {
                match child.try_wait() {
                    Ok(Some(s)) => break Ok(s),
                    Err(e) => break Err(e),
                    Ok(None) => {}
                }
                if Instant::now() > deadline {
                    kill_group(&mut child);
                    for h in handles {
                        let _ = h.join();
                    }
                    let lines = logs.snapshot();
                    return report(
                        check,
                        log_file,
                        Status::Failed,
                        Some(format!("timed out after {secs}s")),
                        Some(Cause {
                            headline: format!("still running after {secs}s — killed"),
                            correlated: diagnose::tail(&lines, 5),
                        }),
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    // The shell can exit while background descendants still own its pipes.
    // Sweep the group before draining capture, including on successful exit.
    kill_group(&mut child);
    for h in handles {
        let _ = h.join();
    }
    let lines = logs.snapshot();

    if let Some(output) = output {
        *output = Some(logs.stdout());
    }

    match status {
        // The expected code (0 unless declared) is the pass condition. A
        // command killed by a signal has no code and can never match.
        Ok(s) if s.code() == Some(expect as i32) => {
            // Expected exit — but explicit rules still apply to the output.
            if let Some(cause) = scan_lines(&lines, absent, &[], false) {
                return report(
                    check,
                    log_file,
                    Status::Failed,
                    Some("error in output".into()),
                    Some(cause),
                );
            }
            if let Some(missing) = find_missing(&lines, contains) {
                return report(
                    check,
                    log_file,
                    Status::Failed,
                    Some(format!("output missing \"{missing}\"")),
                    None,
                );
            }
            report(check, log_file, Status::Passed, summarize(&lines), None)
        }
        Ok(s) => {
            let detail = if expect == 0 {
                format!("exited {}", fmt_status(&s))
            } else {
                format!("exited {} (expected {expect})", fmt_status(&s))
            };
            let cause = diagnose::from_logs(&lines).or_else(|| {
                Some(Cause {
                    headline: detail.clone(),
                    correlated: diagnose::tail(&lines, 5),
                })
            });
            report(check, log_file, Status::Failed, Some(detail), cause)
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
    log_file: &Evidence,
    check: &Check,
    report_index: usize,
    services: &mut Vec<Service>,
    env: &[(String, String)],
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
            .envs(env.iter().map(|(name, value)| (name, value)))
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
    let (logs, handles) = capture::attach(
        &mut child,
        log_file.path.clone(),
        started,
        log_file.private,
        false,
    );

    let track = |services: &mut Vec<Service>, child, logs: &CapturedLogs| {
        services.push(Service {
            child,
            logs: logs.clone(),
            report_index,
            contains: contains.to_vec(),
            absent: absent.to_vec(),
            allow: allow.to_vec(),
            handles,
        });
    };

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    // Remember what the readiness probe last said: "not ready in Ns" alone
    // points at the service, which may be the one thing that is not wrong
    // (issue #2 — e.g. the URL's host resolves somewhere nothing listens).
    let mut last_probe: Option<String> = None;
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
                Some(format!(
                    "crashed at startup after {:.1}s",
                    started.elapsed().as_secs_f32()
                )),
                cause,
            );
        }
        if let Some(url) = ready {
            match crate::http::get(url, Duration::from_millis(500)) {
                Ok(resp) if (200..300).contains(&resp.status) => {
                    track(services, child, &logs);
                    return report(
                        check,
                        log_file,
                        Status::Passed,
                        Some(format!("ready in {:.1}s", started.elapsed().as_secs_f32())),
                        None,
                    );
                }
                Ok(resp) => last_probe = Some(format!("HTTP {}", resp.status)),
                Err(e) => last_probe = Some(format!("{e:#}")),
            }
        } else if started.elapsed() > Duration::from_millis(500) {
            // No readiness probe: consider started after a short grace period.
            track(services, child, &logs);
            return report(
                check,
                log_file,
                Status::Passed,
                Some("started".into()),
                None,
            );
        }
        if Instant::now() > deadline {
            let lines = logs.snapshot();
            let cause = diagnose::from_logs(&lines);
            track(services, child, &logs); // teardown will kill the group — no orphan
            let probe = last_probe.as_deref().unwrap_or("probe never answered");
            return report(
                check,
                log_file,
                Status::Failed,
                Some(format!(
                    "not ready in {timeout_secs}s (last probe: {probe})"
                )),
                cause,
            );
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[allow(clippy::too_many_arguments)]
fn run_http(
    method: &str,
    url: &str,
    body: Option<&str>,
    headers: &[(String, String)],
    expect: Option<u16>,
    contains: &[String],
    absent: &[String],
    timeout_secs: u64,
    (min_ms, max_ms): (Option<u128>, Option<u128>),
    log_file: &Evidence,
    check: &Check,
    jar: &mut HashMap<String, Vec<(String, String)>>,
    output: Option<&mut Option<Result<String>>>,
) -> CheckReport {
    // A body without an explicit content-type defaults to JSON — the 99% case
    // for smoke-testing an API (documented in --help).
    let mut hdrs = headers.to_vec();
    if body.is_some()
        && !hdrs
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        hdrs.push(("Content-Type".into(), "application/json".into()));
    }
    // Replay the jar for this host — an explicit Cookie header on the check wins.
    let host = crate::http::host_of(url);
    if let Some(cookies) = jar.get(&host) {
        if !cookies.is_empty() && !hdrs.iter().any(|(k, _)| k.eq_ignore_ascii_case("cookie")) {
            let line = cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            hdrs.push(("Cookie".into(), line));
        }
    }
    let sent = Instant::now();
    match crate::http::request(method, url, body, &hdrs, Duration::from_secs(timeout_secs)) {
        Ok(resp) => {
            if let Some(output) = output {
                *output = Some(if resp.body.len() <= values::MAX_CAPTURE_BYTES {
                    Ok(resp.body.clone())
                } else {
                    Err(anyhow::anyhow!("HTTP capture exceeds 1 MiB"))
                });
            }
            store_cookies(jar.entry(host).or_default(), &resp.set_cookie);
            let elapsed_ms = sent.elapsed().as_millis();
            // Evidence: what we actually observed.
            let head: String = resp.body.lines().take(20).collect::<Vec<_>>().join("\n");
            log_file.write(format!(
                "{method} {url}\nHTTP {} in {elapsed_ms}ms\n\n{head}\n",
                resp.status
            ));

            let status_ok = match expect {
                Some(code) => resp.status == code,
                None => (200..300).contains(&resp.status), // default: any 2xx
            };
            if !status_ok {
                let expected = expect
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "2xx".into());
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
            if let Some(hit) = absent.iter().find(|p| resp.body.contains(p.as_str())) {
                return report(
                    check,
                    log_file,
                    Status::Failed,
                    Some(format!("body contains \"{hit}\"")),
                    Some(Cause {
                        headline: format!("HTTP {} but body contains \"{hit}\"", resp.status),
                        correlated: resp
                            .body
                            .lines()
                            .filter(|l| l.contains(hit.as_str()))
                            .take(3)
                            .map(|l| l.trim().to_string())
                            .collect(),
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
                        headline: format!(
                            "HTTP {} but body doesn't contain \"{missing}\"",
                            resp.status
                        ),
                        correlated: resp.body.lines().take(3).map(String::from).collect(),
                    }),
                );
            }
            // Answered, and answered correctly — but too slowly. This is a
            // failure with positive evidence (the measured time), not a
            // "couldn't observe": we did observe, and it was late.
            if let Some(budget) = max_ms {
                if elapsed_ms > budget {
                    return report(
                        check,
                        log_file,
                        Status::Failed,
                        Some(format!("{elapsed_ms}ms (max {budget}ms)")),
                        Some(Cause {
                            headline: format!(
                                "HTTP {} was correct but took {elapsed_ms}ms, over the {budget}ms budget",
                                resp.status
                            ),
                            correlated: Vec::new(),
                        }),
                    );
                }
            }
            // The mirror: correct, but too fast for something meant to be
            // expensive. Noise makes a measurement slower, almost never
            // faster, so a floor is the sturdier of the two bounds.
            if let Some(floor) = min_ms {
                if elapsed_ms < floor {
                    return report(
                        check,
                        log_file,
                        Status::Failed,
                        Some(format!("{elapsed_ms}ms (min {floor}ms)")),
                        Some(Cause {
                            headline: format!(
                                "HTTP {} was correct but took only {elapsed_ms}ms, under the {floor}ms floor",
                                resp.status
                            ),
                            correlated: Vec::new(),
                        }),
                    );
                }
            }
            report(
                check,
                log_file,
                Status::Passed,
                Some(format!("HTTP {} in {elapsed_ms}ms", resp.status)),
                None,
            )
        }
        Err(e) => errored(check, log_file, format!("couldn't reach: {e}")),
    }
}

fn run_log(
    path: &str,
    contains: &[String],
    absent: &[String],
    baseline: LogBaseline,
    log_file: &Evidence,
    check: &Check,
) -> CheckReport {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return errored(check, log_file, format!("log file not found: {path}")),
    };
    // The window is [offset at scenario start .. now]. A replaced or truncated file
    // makes the window ambiguous: we can no longer say what happened during the run.
    let offset = match baseline {
        Some((ino, size)) => {
            if meta.ino() != ino {
                return errored(
                    check,
                    log_file,
                    "log file was replaced during the run — window is ambiguous".into(),
                );
            }
            if meta.size() < size {
                return errored(
                    check,
                    log_file,
                    "log file was truncated during the run — window is ambiguous".into(),
                );
            }
            size
        }
        None => 0, // didn't exist at scenario start: everything in it was written during the run
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
    log_file.write(text.as_bytes()); // evidence: the observed window

    if let Some(idx) = lines
        .iter()
        .position(|l| absent.iter().any(|p| l.contains(p.as_str())))
    {
        let hit = absent
            .iter()
            .find(|p| lines[idx].contains(p.as_str()))
            .unwrap();
        let lo = idx.saturating_sub(1);
        let hi = (idx + 3).min(lines.len());
        return report(
            check,
            log_file,
            Status::Failed,
            Some(format!("found \"{hit}\"")),
            Some(Cause {
                headline: lines[idx].trim().to_string(),
                correlated: lines[lo..hi]
                    .iter()
                    .map(|s| s.trim_end().to_string())
                    .collect(),
            }),
        );
    }
    if let Some(missing) = contains
        .iter()
        .find(|p| !lines.iter().any(|l| l.contains(p.as_str())))
    {
        return report(
            check,
            log_file,
            Status::Failed,
            Some(format!(
                "\"{missing}\" not found ({} new line(s))",
                lines.len()
            )),
            None,
        );
    }
    report(
        check,
        log_file,
        Status::Passed,
        Some(format!("{} new line(s) checked", lines.len())),
        None,
    )
}

/// Fold `Set-Cookie` answers into the host's jar: last value per name wins,
/// `Max-Age=0` (the logout idiom) deletes. Attributes (Path, Expires, Secure…)
/// are ignored — the jar lives one scenario, against one host, over plain http.
fn store_cookies(cookies: &mut Vec<(String, String)>, set_cookie: &[String]) {
    for sc in set_cookie {
        let Some((name, value)) = sc.split(';').next().unwrap_or("").split_once('=') else {
            continue;
        };
        let (name, value) = (name.trim().to_string(), value.trim().to_string());
        cookies.retain(|(n, _)| n != &name);
        if !sc
            .split(';')
            .skip(1)
            .any(|a| a.trim().eq_ignore_ascii_case("max-age=0"))
        {
            cookies.push((name, value));
        }
    }
}

/// First line matching a forbidden pattern (optionally including the default
/// crash markers), unless an `allow` pattern exempts it.
fn scan_lines(
    lines: &[LogLine],
    absent: &[String],
    allow: &[String],
    with_defaults: bool,
) -> Option<Cause> {
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
    contains
        .iter()
        .find(|p| !lines.iter().any(|l| l.text.contains(p.as_str())))
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

fn report(
    check: &Check,
    log_file: &Evidence,
    status: Status,
    detail: Option<String>,
    cause: Option<Cause>,
) -> CheckReport {
    CheckReport {
        scenario: String::new(), // stamped by the scenario runner
        step: None,
        captures: Vec::new(),
        output_withheld: log_file.private,
        label: check.label(),
        status,
        duration_ms: 0, // stamped by the caller once the check returns
        detail,
        cause,
        log_file: log_file
            .path
            .is_file()
            .then(|| log_file.path.display().to_string()),
        reason: None,
    }
}

fn errored(check: &Check, log_file: &Evidence, msg: String) -> CheckReport {
    log_file.write(format!("{msg}\n")); // evidence file always exists
    report(check, log_file, Status::Errored, Some(msg), None)
}

fn fmt_status(s: &std::process::ExitStatus) -> String {
    match s.code() {
        Some(c) => c.to_string(),
        None => s.to_string(), // killed by signal
    }
}

/// Reserve the next run directory by creating it — `create_dir` fails if it
/// already exists, so two probatum processes racing in the same repo cannot
/// land on the same number and overwrite each other's evidence.
fn next_run_dir() -> Result<PathBuf> {
    let base = PathBuf::from(".probatum/runs");
    std::fs::create_dir_all(&base).context("create .probatum/runs")?;
    let mut next = 1u32;
    for entry in std::fs::read_dir(&base)? {
        if let Ok(name) = entry.map(|e| e.file_name().to_string_lossy().into_owned()) {
            if let Ok(n) = name.parse::<u32>() {
                next = next.max(n + 1);
            }
        }
    }
    // Someone may take the number between our scan and our create: step over
    // whoever won and try the next one.
    for candidate in next..next.saturating_add(1000) {
        let dir = base.join(format!("{candidate:04}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).context("create run directory"),
        }
    }
    anyhow::bail!("cannot reserve a run directory under {}", base.display())
}
