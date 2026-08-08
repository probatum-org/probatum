//! Config parsing — a flat list of checks. No ceremony, no logic, no nesting.
//!
//! A check = one source + flat AND rules:
//!
//!   [[check]] run = "<cmd>"                command to completion (exit code is the authority)
//!   [[check]] run = ... + ready/timeout    start a service, wait until it answers, keep it alive
//!   [[check]] get = "<url>"                HTTP GET (embedded client)
//!   [[check]] post = "<url>"               HTTP POST (+ body, headers)
//!   [[check]] log = "<path>"               external log file, only lines written during this run
//!
//! Rules: expect (HTTP status), contains (must appear), absent (must not appear),
//! allow (exempt lines from the default crash filter — services only).
//! Unknown keys are a typo, not a feature: they are rejected. So is a rule of
//! the wrong type — a dropped rule is a check that silently asserts less.

use anyhow::{bail, Result};
use toml::{Table, Value};

#[derive(Debug, Clone)]
pub enum Check {
    /// `run` — exit code is the authority; explicit contains/absent apply to
    /// the captured output even on exit 0. No default crash markers here
    /// (a passing `cargo test` may legitimately print "panicked at").
    Run {
        cmd: String,
        name: Option<String>,
        contains: Vec<String>,
        absent: Vec<String>,
    },
    /// `run` + `ready`/`timeout` — there is no exit code to trust while it
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
    /// `get` / `post` — embedded HTTP check. Omitted `expect` = any 2xx;
    /// `contains` applies to the response body. `post` adds `body` and
    /// `headers` (flat string table; Content-Type defaults to application/json
    /// when a body is present).
    Http {
        method: &'static str, // "GET" | "POST"
        url: String,
        name: Option<String>,
        body: Option<String>,
        headers: Vec<(String, String)>,
        expect: Option<u16>,
        contains: Vec<String>,
    },
    /// `log` — evaluated from run start (offset noted before any check runs).
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
            Check::Http {
                method, url, name, ..
            } => name.clone().unwrap_or_else(|| format!("{method} {url}")),
            Check::Log { path, name, .. } => name.clone().unwrap_or_else(|| path.clone()),
        }
    }
}

pub fn parse(text: &str) -> Result<Vec<Check>> {
    let doc: Table = text
        .parse::<Table>()
        .map_err(|e| anyhow::anyhow!("invalid config: {e}"))?;

    for key in doc.keys() {
        if key != "check" {
            bail!("unknown top-level key '{key}' — a config is a list of [[check]] entries");
        }
    }
    let items = match doc.get("check") {
        Some(Value::Array(a)) if !a.is_empty() => a,
        Some(_) => bail!("'check' must be a list of [[check]] entries"),
        None => bail!("config has no checks — a config is a list of [[check]] entries"),
    };

    let mut checks = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let n = i + 1;
        let map = item
            .as_table()
            .ok_or_else(|| anyhow::anyhow!("check {n} must be a table (`[[check]]`)"))?;

        if map.contains_key("get") {
            reject_unknown(map, n, &["get", "expect", "contains", "name"])?;
            checks.push(Check::Http {
                method: "GET",
                url: req_str(map, "get", n)?,
                name: opt_str(map, "name", n)?,
                body: None,
                headers: Vec::new(),
                expect: opt_u16(map, "expect", n)?,
                contains: str_list(map, "contains", n)?,
            });
        } else if map.contains_key("post") {
            reject_unknown(
                map,
                n,
                &["post", "body", "headers", "expect", "contains", "name"],
            )?;
            checks.push(Check::Http {
                method: "POST",
                url: req_str(map, "post", n)?,
                name: opt_str(map, "name", n)?,
                body: opt_str(map, "body", n)?,
                headers: str_map(map, "headers", n)?,
                expect: opt_u16(map, "expect", n)?,
                contains: str_list(map, "contains", n)?,
            });
        } else if map.contains_key("log") {
            reject_unknown(map, n, &["log", "contains", "absent", "name"])?;
            let contains = str_list(map, "contains", n)?;
            let absent = str_list(map, "absent", n)?;
            if contains.is_empty() && absent.is_empty() {
                bail!("check {n}: 'log' needs at least one rule (contains/absent) — a check without rules asserts nothing");
            }
            checks.push(Check::Log {
                path: req_str(map, "log", n)?,
                name: opt_str(map, "name", n)?,
                contains,
                absent,
            });
        } else if map.contains_key("run") {
            reject_unknown(
                map,
                n,
                &[
                    "run", "ready", "timeout", "contains", "absent", "allow", "name",
                ],
            )?;
            let cmd = req_str(map, "run", n)?;
            let name = opt_str(map, "name", n)?;
            let contains = str_list(map, "contains", n)?;
            let absent = str_list(map, "absent", n)?;
            let allow = str_list(map, "allow", n)?;
            if map.contains_key("ready") || map.contains_key("timeout") {
                checks.push(Check::Service {
                    cmd,
                    name,
                    ready: opt_str(map, "ready", n)?,
                    timeout_secs: opt_u64(map, "timeout", n)?.unwrap_or(30),
                    contains,
                    absent,
                    allow,
                });
            } else {
                if !allow.is_empty() {
                    bail!("check {n}: 'allow' only applies to a service (add ready/timeout) — a plain run has no default filter to exempt");
                }
                checks.push(Check::Run {
                    cmd,
                    name,
                    contains,
                    absent,
                });
            }
        } else {
            bail!("check {n} needs a 'run', 'get', 'post' or 'log' key");
        }
    }
    Ok(checks)
}

fn req_str(map: &Table, key: &str, n: usize) -> Result<String> {
    match map.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => bail!("check {n}: '{key}' must be a string"),
    }
}

fn opt_str(map: &Table, key: &str, n: usize) -> Result<Option<String>> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(v) => bail!(
            "check {n}: '{key}' must be a string, found {}",
            v.type_str()
        ),
    }
}

fn opt_u64(map: &Table, key: &str, n: usize) -> Result<Option<u64>> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Integer(i)) if *i >= 0 => Ok(Some(*i as u64)),
        Some(v) => bail!(
            "check {n}: '{key}' must be a positive integer, found {}",
            v.type_str()
        ),
    }
}

fn opt_u16(map: &Table, key: &str, n: usize) -> Result<Option<u16>> {
    match opt_u64(map, key, n)? {
        None => Ok(None),
        Some(v) if v <= u16::MAX as u64 => Ok(Some(v as u16)),
        Some(v) => bail!("check {n}: '{key}' = {v} is not a valid HTTP status"),
    }
}

/// A string or a list of strings. A non-string entry is an error, never a
/// silent drop: a vanished rule is a check that asserts less than it says.
fn str_list(map: &Table, key: &str, n: usize) -> Result<Vec<String>> {
    match map.get(key) {
        None => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.clone()),
                other => bail!(
                    "check {n}: '{key}' must contain only strings, found {} — quote it",
                    other.type_str()
                ),
            })
            .collect(),
        Some(v) => bail!(
            "check {n}: '{key}' must be a string or a list of strings, found {}",
            v.type_str()
        ),
    }
}

/// A flat table of strings (e.g. `headers`) — anything nested is rejected.
fn str_map(map: &Table, key: &str, n: usize) -> Result<Vec<(String, String)>> {
    match map.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Table(t)) => t
            .iter()
            .map(|(k, v)| match v {
                Value::String(s) => Ok((k.clone(), s.clone())),
                other => bail!(
                    "check {n}: '{key}.{k}' must be a string, found {}",
                    other.type_str()
                ),
            })
            .collect(),
        Some(_) => bail!(
            "check {n}: '{key}' must be a table (e.g. `headers = {{ content-type = \"application/json\" }}`)"
        ),
    }
}

/// Unknown keys are a typo, not a feature — surface them instead of ignoring.
fn reject_unknown(map: &Table, n: usize, known: &[&str]) -> Result<()> {
    for k in map.keys() {
        if !known.contains(&k.as_str()) {
            bail!("check {n}: unknown key '{k}' (known: {})", known.join(", "));
        }
    }
    Ok(())
}
