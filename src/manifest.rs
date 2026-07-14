//! Config parsing — a flat list of checks. No ceremony, no logic, no nesting.
//!
//! A check = one source + flat AND rules:
//!
//!   - run: <cmd>                      command to completion (exit code is the authority)
//!   - run: <cmd> + ready:/timeout:    start a service, wait until it answers, keep it alive
//!   - get: <url>                      HTTP GET (embedded client)
//!   - log: <path>                     external log file, only lines written during this run
//!
//! Rules: expect (HTTP status), contains (must appear), absent (must not appear),
//! allow (exempt lines from the default crash filter — services only).
//! Unknown keys are a typo, not a feature: they are rejected.

use anyhow::{bail, Result};
use serde_yaml::{Mapping, Value};

#[derive(Debug, Clone)]
pub enum Check {
    /// `run:` — exit code is the authority; explicit contains/absent apply to
    /// the captured output even on exit 0. No default crash markers here
    /// (a passing `cargo test` may legitimately print "panicked at").
    Run {
        cmd: String,
        name: Option<String>,
        contains: Vec<String>,
        absent: Vec<String>,
    },
    /// `run:` + `ready:`/`timeout:` — there is no exit code to trust while it
    /// runs, so the default crash filter applies to its logs; `allow` exempts.
    Service {
        cmd: String,
        name: Option<String>,
        ready: Option<String>,
        timeout_secs: u64,
        contains: Vec<String>,
        absent: Vec<String>,
        allow: Vec<String>,
    },
    /// `get:` — omitted `expect` means any 2xx; `contains` applies to the body.
    Get {
        url: String,
        name: Option<String>,
        expect: Option<u16>,
        contains: Vec<String>,
    },
    /// `log:` — evaluated from run start (offset noted before any check runs).
    /// At least one rule is required: a check without rules asserts nothing.
    Log {
        path: String,
        name: Option<String>,
        contains: Vec<String>,
        absent: Vec<String>,
    },
}

impl Check {
    pub fn label(&self) -> String {
        match self {
            Check::Run { cmd, name, .. } | Check::Service { cmd, name, .. } => {
                name.clone().unwrap_or_else(|| cmd.clone())
            }
            Check::Get { url, name, .. } => name.clone().unwrap_or_else(|| format!("GET {url}")),
            Check::Log { path, name, .. } => name.clone().unwrap_or_else(|| path.clone()),
        }
    }
}

pub fn parse(text: &str) -> Result<Vec<Check>> {
    let items: Vec<Value> =
        serde_yaml::from_str(text).map_err(|e| anyhow::anyhow!("invalid config: {e}"))?;
    if items.is_empty() {
        bail!("config has no checks");
    }
    let mut checks = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let n = i + 1;
        let map = item
            .as_mapping()
            .ok_or_else(|| anyhow::anyhow!("check {n} must be a map (e.g. `- run: cargo test`)"))?;

        if map.get("get").is_some() {
            reject_unknown(map, n, &["get", "expect", "contains", "name"])?;
            checks.push(Check::Get {
                url: req_str(map, "get", n)?,
                name: opt_str(map, "name"),
                expect: opt_u64(map, "expect").map(|v| v as u16),
                contains: str_list(map, "contains"),
            });
        } else if map.get("log").is_some() {
            reject_unknown(map, n, &["log", "contains", "absent", "name"])?;
            let contains = str_list(map, "contains");
            let absent = str_list(map, "absent");
            if contains.is_empty() && absent.is_empty() {
                bail!("check {n}: 'log' needs at least one rule (contains/absent) — a check without rules asserts nothing");
            }
            checks.push(Check::Log {
                path: req_str(map, "log", n)?,
                name: opt_str(map, "name"),
                contains,
                absent,
            });
        } else if map.get("run").is_some() {
            reject_unknown(map, n, &["run", "ready", "timeout", "contains", "absent", "allow", "name"])?;
            let cmd = req_str(map, "run", n)?;
            let name = opt_str(map, "name");
            let contains = str_list(map, "contains");
            let absent = str_list(map, "absent");
            let allow = str_list(map, "allow");
            if map.get("ready").is_some() || map.get("timeout").is_some() {
                checks.push(Check::Service {
                    cmd,
                    name,
                    ready: opt_str(map, "ready"),
                    timeout_secs: opt_u64(map, "timeout").unwrap_or(30),
                    contains,
                    absent,
                    allow,
                });
            } else {
                if !allow.is_empty() {
                    bail!("check {n}: 'allow' only applies to a service (add ready:/timeout:) — a plain run has no default filter to exempt");
                }
                checks.push(Check::Run { cmd, name, contains, absent });
            }
        } else {
            bail!("check {n} needs a 'run', 'get' or 'log' key");
        }
    }
    Ok(checks)
}

fn req_str(map: &Mapping, key: &str, n: usize) -> Result<String> {
    map.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("check {n}: '{key}' must be a string"))
}

fn opt_str(map: &Mapping, key: &str) -> Option<String> {
    map.get(key).and_then(|v| v.as_str()).map(String::from)
}

fn opt_u64(map: &Mapping, key: &str) -> Option<u64> {
    map.get(key).and_then(|v| v.as_u64())
}

/// Accept either a single string or a list of strings.
fn str_list(map: &Mapping, key: &str) -> Vec<String> {
    match map.get(key) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Sequence(seq)) => {
            seq.iter().filter_map(|v| v.as_str().map(String::from)).collect()
        }
        _ => Vec::new(),
    }
}

/// Unknown keys are a typo, not a feature — surface them instead of ignoring.
fn reject_unknown(map: &Mapping, n: usize, known: &[&str]) -> Result<()> {
    for (k, _) in map.iter() {
        if let Some(k) = k.as_str() {
            if !known.contains(&k) {
                bail!("check {n}: unknown key '{k}' (known: {})", known.join(", "));
            }
        }
    }
    Ok(())
}
