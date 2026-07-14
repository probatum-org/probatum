//! Manifest parsing — the DSL stays declarative: no logic, only composition.

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub proof: String,
    pub promise: Promise,
    #[serde(deserialize_with = "de_steps")]
    pub steps: Vec<Step>,
    #[serde(default, deserialize_with = "de_oracles")]
    pub oracles: Vec<Oracle>,
}

#[derive(Debug, Deserialize)]
pub struct Promise {
    pub id: String,
    pub statement: String,
}

#[derive(Debug, Clone)]
pub enum Step {
    /// `suite.run: {cmd: "cargo test"}` — run a command to completion.
    SuiteRun { cmd: String },
    /// `service.start: {cmd, ready, timeout}` — spawn, wait for readiness, keep running.
    ServiceStart {
        cmd: String,
        ready: Option<String>,
        timeout_secs: u64,
    },
    /// `api.check: {get, expect}` — HTTP GET, assert status.
    ApiCheck { get: String, expect: u16 },
}

impl Step {
    pub fn name(&self) -> &'static str {
        match self {
            Step::SuiteRun { .. } => "suite.run",
            Step::ServiceStart { .. } => "service.start",
            Step::ApiCheck { .. } => "api.check",
        }
    }
    pub fn label(&self) -> String {
        match self {
            Step::SuiteRun { cmd } => format!("suite.run — {cmd}"),
            Step::ServiceStart { cmd, .. } => format!("service.start — {cmd}"),
            Step::ApiCheck { get, expect } => format!("api.check — GET {get} (expect {expect})"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Oracle {
    /// `logs.clean: {level: error, allow: [...]}` — captured logs contain no error lines.
    LogsClean { allow: Vec<String> },
}

impl Oracle {
    pub fn label(&self) -> String {
        match self {
            Oracle::LogsClean { allow } => {
                if allow.is_empty() {
                    "logs.clean".into()
                } else {
                    format!("logs.clean (allow: {})", allow.join(", "))
                }
            }
        }
    }
}

pub fn load(path: &std::path::Path) -> Result<Manifest> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read manifest {}", path.display()))?;
    let m: Manifest = serde_yaml::from_str(&text)
        .with_context(|| format!("invalid manifest {}", path.display()))?;
    if m.steps.is_empty() {
        bail!("manifest has no steps");
    }
    Ok(m)
}

// ---- custom deserialization: steps are single-key maps, closed vocabulary ----

fn de_steps<'de, D>(de: D) -> std::result::Result<Vec<Step>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let raw: Vec<serde_yaml::Value> = Vec::deserialize(de)?;
    let mut steps = Vec::new();
    for (i, item) in raw.iter().enumerate() {
        let map = item
            .as_mapping()
            .ok_or_else(|| D::Error::custom(format!("step {} must be a map", i + 1)))?;
        if map.len() != 1 {
            return Err(D::Error::custom(format!(
                "step {} must have exactly one action key",
                i + 1
            )));
        }
        let (key, val) = map.iter().next().unwrap();
        let key = key.as_str().unwrap_or_default();
        let step = match key {
            "suite.run" => Step::SuiteRun {
                cmd: field_str(val, "cmd").map_err(D::Error::custom)?,
            },
            "service.start" => Step::ServiceStart {
                cmd: field_str(val, "cmd").map_err(D::Error::custom)?,
                ready: field_str(val, "ready").ok(),
                timeout_secs: field_u64(val, "timeout").unwrap_or(30),
            },
            "api.check" => Step::ApiCheck {
                get: field_str(val, "get").map_err(D::Error::custom)?,
                expect: field_u64(val, "expect").unwrap_or(200) as u16,
            },
            other => {
                return Err(D::Error::custom(format!(
                    "unknown action '{other}' (known: suite.run, service.start, api.check)"
                )))
            }
        };
        steps.push(step);
    }
    Ok(steps)
}

fn de_oracles<'de, D>(de: D) -> std::result::Result<Vec<Oracle>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let raw: Vec<serde_yaml::Value> = Vec::deserialize(de)?;
    let mut oracles = Vec::new();
    for (i, item) in raw.iter().enumerate() {
        let map = item
            .as_mapping()
            .ok_or_else(|| D::Error::custom(format!("oracle {} must be a map", i + 1)))?;
        let (key, val) = map
            .iter()
            .next()
            .ok_or_else(|| D::Error::custom(format!("oracle {} is empty", i + 1)))?;
        match key.as_str().unwrap_or_default() {
            "logs.clean" => {
                let allow = val
                    .get("allow")
                    .and_then(|v| v.as_sequence())
                    .map(|seq| {
                        seq.iter()
                            .filter_map(|s| s.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                oracles.push(Oracle::LogsClean { allow });
            }
            other => {
                return Err(D::Error::custom(format!(
                    "unknown oracle '{other}' (known: logs.clean)"
                )))
            }
        }
    }
    Ok(oracles)
}

fn field_str(v: &serde_yaml::Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(String::from)
        .ok_or_else(|| format!("missing field '{key}'"))
}

fn field_u64(v: &serde_yaml::Value, key: &str) -> Result<u64, String> {
    v.get(key)
        .and_then(|x| x.as_u64())
        .ok_or_else(|| format!("missing field '{key}'"))
}
