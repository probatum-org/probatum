//! probatum — Don't trust the promise. Run the proof.
//!
//! v0: `probatum run <manifest.yaml> [--json] [--seed N]`

mod capture;
mod diagnose;
mod http;
mod manifest;
mod own;
mod runner;
mod verdict;

use anyhow::{bail, Context, Result};
use std::io::Read;

fn main() {
    // Exit codes: 0 = all passed, 1 = at least one check failed,
    // 2 = couldn't run (invalid config, dirty environment, tool error).
    let code = match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("probatum: {e:#}");
            2
        }
    };
    std::process::exit(code);
}

const USAGE: &str = "usage: probatum run [probatum.yaml|-] [--json] [--seed N] | probatum init";
const DEFAULT_CONFIG: &str = "probatum.yaml";

/// The whole product in one --help: an agent (or a human) can use probatum
/// correctly from this text alone, no external docs needed.
const HELP: &str = "\
probatum — test-oriented check runner. One config, embedded checks,
only the failures that matter.

usage:
  probatum init                 write a commented example probatum.yaml
  probatum run [file|-]         run checks (default ./probatum.yaml, - = stdin)
      --json                    machine-readable verdict on stdout
      --seed N                  replay reference

config: a flat YAML list. one check = one source + flat AND rules.
  - run: <cmd>                  command; exit code is the authority
    contains: [..]              output must contain (applies even on exit 0)
    absent: [..]                output must not contain
  - run: <cmd>                  with ready:/timeout: it becomes a service:
    ready: <url>                started, polled until 2xx, kept alive
    timeout: <secs>             not ready in time = failed
    allow: [..]                 exempt known noise from the default crash
                                filter (panic/traceback/FATAL/ERROR — on for
                                services, off for plain commands)
  - get: <url>                  HTTP GET; omitted expect = any 2xx
    expect: <code>              exact status
    contains: [..]              body must contain
  - log: <path>                 external file, only lines written during THIS
    contains: [..]              run count; at least one rule required
    absent: [..]
  name: <label>                 optional display name on any check

unknown keys are errors. checks run top to bottom and stop at the first
failure. every spawned process group is killed on every exit path — even if
probatum crashes or is Ctrl-C'd.

exit codes: 0 all passed · 1 a check failed (cause on screen) · 2 couldn't
run (invalid config, dirty environment, unobservable target — fix the env,
don't force). evidence: .probatum/runs/NNNN/ (frozen config, logs, run.json)";

fn real_main() -> Result<i32> {
    own::install_signal_handlers(); // Ctrl-C/kill must not leave orphans

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return Ok(0);
    }
    let mut json = false;
    let mut seed: Option<u32> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--seed" => {
                let v = it.next().unwrap_or_default();
                seed = Some(v.parse().map_err(|_| anyhow::anyhow!("--seed attend un entier"))?);
            }
            _ => positional.push(a),
        }
    }

    match positional.first().map(String::as_str) {
        Some("run") => {}
        Some("init") => return init(),
        _ => bail!("{USAGE}"),
    }
    // No path? The convention file is right there — like make and Makefile.
    let default = DEFAULT_CONFIG.to_string();
    let path = match positional.get(1) {
        Some(p) => p,
        None if std::path::Path::new(DEFAULT_CONFIG).exists() => &default,
        None => bail!("no {DEFAULT_CONFIG} here — run `probatum init` or pass a path\n{USAGE}"),
    };

    // Manifest source: a file, or `-` for stdin (agents pipe an in-memory manifest).
    let (text, source) = if path == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading manifest from stdin")?;
        (s, "<stdin>".to_string())
    } else {
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read manifest {path}"))?;
        (s, path.clone())
    };

    let checks = manifest::parse(&text)?;
    let seed = seed.unwrap_or_else(random_seed);
    let report = runner::run(&checks, &text, &source, seed)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        verdict::print(&report);
    }
    Ok(match report.verdict.as_str() {
        "pass" => 0,
        "fail" => 1,
        _ => 2, // couldn't-run
    })
}

/// `probatum init` — drop a commented example config to copy and edit.
fn init() -> Result<i32> {
    let path = std::path::Path::new(DEFAULT_CONFIG);
    if path.exists() {
        bail!("{DEFAULT_CONFIG} already exists — not overwriting it");
    }
    std::fs::write(path, EXAMPLE)
        .map_err(|e| anyhow::anyhow!("can't write {DEFAULT_CONFIG}: {e}"))?;
    println!("wrote {DEFAULT_CONFIG} — edit it, then: probatum run");
    Ok(0)
}

const EXAMPLE: &str = r#"# probatum.yaml — probatum run
# A check = one source (run / get / log) + flat rules. Unknown keys are errors.

# a command — passes if it exits 0
- run: echo "replace me with cargo test / npm test / pytest"

# a service — start it, wait until it answers, keep it alive for later checks
#- name: api boots
#  run: ./myapp --port 8080
#  ready: http://127.0.0.1:8080/healthz    # polls until 2xx
#  timeout: 15
#  allow: ["known noise to ignore"]        # exempt lines from the crash filter

# an HTTP endpoint — embedded curl (omitted expect = any 2xx passes)
#- get: http://127.0.0.1:8080/api/version
#  expect: 200
#  contains: ['"version"']                 # body must contain this

# an external log file — only lines written during THIS run count
#- log: /var/log/myapp/app.log
#  contains: ["started"]                   # must appear
#  absent: ["ERROR", "panic"]              # must not appear
"#;

/// Seed from /dev/urandom — recorded in the evidence so every run is replayable
/// by reference even before the seed drives any randomness (v0).
fn random_seed() -> u32 {
    let mut buf = [0u8; 4];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok()
    {
        u32::from_le_bytes(buf)
    } else {
        0xC0FFEE
    }
}
