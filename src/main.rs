//! probatum — test-oriented check runner.
//!
//! `probatum run <probatum.toml> [--json] [--seed N]`

mod capture;
mod diagnose;
mod http;
mod manifest;
mod own;
mod runner;
mod values;
mod verdict;

use anyhow::{bail, Context, Result};
use std::io::Read;

fn main() {
    // Exit codes: 0 = all passed, 1 = at least one check failed,
    // 2 = couldn't run (invalid config, dirty environment, tool error),
    // 101 = probatum itself panicked.
    //
    // `--json` is read here, before anything can fail, so that a config we
    // cannot even parse still answers in the protocol the caller asked for.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = parse_args(&args)
        .map(|o| o.json)
        .unwrap_or_else(|_| args.iter().any(|a| a == "--json"));

    let code = match std::panic::catch_unwind(|| real_main(&args)) {
        Ok(Ok(code)) => code,
        Ok(Err(e)) => {
            eprintln!("probatum: {e:#}");
            if json {
                Outcome::of_error(kind_of(&e), format!("{e:#}")).print();
            }
            2
        }
        Err(panic) => {
            // The default hook already printed the panic to stderr; the caller
            // still gets a document, and 101 keeps saying "probatum broke",
            // not "your system failed".
            if json {
                Outcome::of_error("internal_error", panic_message(&panic)).print();
            }
            101
        }
    };
    std::process::exit(code);
}

/// Which failure class the caller is looking at. An agent switches on this:
/// a bad config will never fix itself by retrying, a busy port might.
fn kind_of(e: &anyhow::Error) -> &'static str {
    let text = format!("{e:#}");
    if text.contains("invalid config")
        || text.contains("check ")
        || text.contains("no checks")
        || text.contains("unknown top-level key")
        || text.starts_with("usage:")
        || text.contains("cannot read manifest")
        || text.contains("probatum.toml")
    {
        "invalid_config"
    } else {
        "execution_error"
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        format!("panic: {s}")
    } else if let Some(s) = panic.downcast_ref::<String>() {
        format!("panic: {s}")
    } else {
        "panic".into()
    }
}

const USAGE: &str =
    "usage: probatum run [probatum.toml|-] [--scenario NAME] [--json] [--seed N] | probatum init";
const DEFAULT_CONFIG: &str = "probatum.toml";

/// Schema 4 adds capture names, step identity, inferred prerequisites, output
/// privacy and dependency_unavailable skips. Captured values are never serialized.
const SCHEMA: u32 = 4;

/// What `--json` emits, for every outcome. The run fields are flattened in
/// when a run happened, so a reader of schema 1 still finds them where they
/// were; `error` is present exactly when the verdict is couldn't-run and
/// nothing ran.
#[derive(serde::Serialize)]
struct Outcome<'a> {
    schema: u32,
    verdict: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<OutcomeError>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    run: Option<&'a runner::RunReport>,
}

#[derive(serde::Serialize)]
struct OutcomeError {
    /// invalid_config | execution_error | internal_error
    kind: &'static str,
    message: String,
}

impl<'a> Outcome<'a> {
    fn of_run(run: &'a runner::RunReport) -> Self {
        Outcome {
            schema: SCHEMA,
            verdict: &run.verdict,
            error: None,
            run: Some(run),
        }
    }
    fn of_error(kind: &'static str, message: String) -> Self {
        Outcome {
            schema: SCHEMA,
            verdict: "couldn't-run",
            error: Some(OutcomeError { kind, message }),
            run: None,
        }
    }
    fn print(&self) {
        // stdout carries the protocol and nothing else; human text is stderr.
        match serde_json::to_string_pretty(self) {
            Ok(doc) => println!("{doc}"),
            // Serialization cannot realistically fail, but claiming a document
            // we did not emit would be worse than saying so.
            Err(e) => eprintln!("probatum: cannot serialize the outcome: {e}"),
        }
    }
}

/// The whole product in one --help: an agent (or a human) can use probatum
/// correctly from this text alone, no external docs needed.
const HELP: &str = r#"probatum — test-oriented check runner. One config, embedded checks,
only the failures that matter.

usage:
  probatum init                 write a commented example probatum.toml
  probatum run [file|-]         run checks (default ./probatum.toml, - = stdin)
      --scenario NAME           select a scenario and its capture prerequisites
      --json                    machine-readable verdict on stdout
      --seed N                  replay reference

config: named scenarios, with one operation directly in the table:
  [smoke]
  run = "cargo test"
  os = "linux"                  optional scope: linux, macos, or windows

For several operations, number the steps; there is no check wrapper:
  [api]
  os = "linux"                  scope declared once for the whole scenario

  [api.1]
  run = "./app"
  ready = "http://localhost:8080/health"

  [api.2]
  get = "http://localhost:8080/version"
  expect = 200

Steps run in the order they are written, and must be declared in ascending
order (1, 2, 10): a step after a higher one is an error. Use positive integers
without leading zeros; gaps are allowed. Multiple steps may use run.
Do not mix a direct operation and numbered steps in one scenario. Short steps
also support ordinary TOML inline tables: 1 = { run = "cargo test" } under [smoke].

Without os a scenario applies on any supported host. A command/HTTP/log check
may also declare os; it cannot conflict with the scenario. Service scope belongs
on the scenario. OS names describe applicability, not additional platform support.
No expressions or branching; the only dependencies are those capture references imply.
Validate the whole file before filtering.
Unknown names, keys, types and OS values are errors, including in excluded scenarios.

Existing [[check]] or check = [...] files are one scenario named default.
Do not mix that form with named scenarios. The fields below use that legacy spelling.
One check = one source + flat AND rules.
  [[check]]
  run = "<cmd>"                 command; exit code is the authority
  env = { KEY = "value" }       optional environment additions (commands/services)
  contains = [".."]             output must contain (applies even on exit 0)
  absent = [".."]               output must not contain
  expect = <code>               the exit code it should return (default 0)
  timeout = <secs>              kill it after N seconds and fail

  [[check]]                     with ready or background it is a service:
  run = "<cmd>"
  ready = "<url>"               started, polled until 2xx, kept alive
  background = true             kept alive with no probe (ready omitted)
  timeout = <secs>              not ready in time = failed
  allow = [".."]                exempt known noise from the default crash
                                filter (panic/traceback/FATAL/ERROR — on for
                                services, off for plain commands)
  [[check]]
  get = "<url>"                 HTTP GET; omitted expect = any 2xx
  headers = { k = "v" }         request headers (all HTTP methods)
  expect = <code>               exact status
  contains = [".."]             body must contain
  absent = [".."]               body must not contain (prove it is gone)
  timeout = <secs>              request deadline (default 5)
  max_ms = <ms>                 a correct but slower answer fails, with the
                                measured time as evidence (timeout gives up,
                                max_ms judges what it saw)

  [[check]]
  post = "<url>"                HTTP POST; same rules as get, plus:
  body = "<string>"             request body (Content-Type defaults to
                                application/json when body is set)
  put = / patch = / delete =    same shape as post, the method is the key

cookies: Set-Cookie answers are kept in a per-host jar for each scenario and
replayed on the later get/post checks — log in, then check what needed the
login. an explicit Cookie header on a check wins over the jar.

  [[check]]
  log = "<path>"                external file, only lines written during THIS
  contains = [".."]             scenario count; at least one rule required
  absent = [".."]

  name = "<label>"              optional display name on any check

captured values:
  [login]
  post = "http://localhost:8080/login"
  body = '{"username":"editor","password":"secret"}'
  capture = { token = "json.access_token" }

  [profile]
  get = "http://localhost:8080/profile"
  headers = { Authorization = "Bearer ${login.token}" }

Commands support capture = { value = "stdout" }; HTTP supports json.field or
json.object.field (identifier keys only, no arrays). Capture names are identifiers.
JSON captures are nonempty strings, numbers or booleans. Stdout is UTF-8, excludes
stderr and loses trailing CR/LF only. Captured output is limited to 1 MiB.
Missing/empty/non-scalar captures or invalid JSON fail (exit 1); unreadable,
non-UTF-8 or oversized output is couldn't-run (exit 2). Only passing checks publish.

Reference ${login.token}, ${auth.2.token}, or ${default.1.token} for legacy steps.
Quoted scenario names use their literal name inside the reference. Ambiguous
addresses, unknown references, cycles and forward references within a scenario
are config errors, even when excluded. Use $${...} for literal ${...} in templates.
Producers run once per invocation before consumers; --scenario includes required
producer scenarios. OS scope still applies; excluded consumers add no prerequisites.

Substitution applies to HTTP URLs/headers/JSON bodies, ready URLs, contains/absent/
allow rules, and env values. URL values are percent-encoded; headers reject CR/LF/NUL.
In JSON bodies, put references in string values: an entire reference preserves the
captured scalar's type, embedded references produce escaped text. Keys stay literal.
Commands use env = { TOKEN = "${login.token}" } and run = 'tool "$TOKEN"'.
run strings, log paths, labels and metadata are not interpolated by probatum.

All captures are treated as sensitive. Output, response bodies and log excerpts
for scenarios that capture or reference values are withheld from logs and reports,
including on failures, panic and signals. probatum's own failure detail is kept,
captured values replaced by [redacted]. Checks still evaluate the real output.
Reports retain status/timing, template labels and capture names, never values.
The frozen config is verbatim: credentials written directly in it remain there.
Captured values live for this invocation; replay obtains fresh values. Services
and cookies remain local to their scenario; sharing a token does not keep an API alive:
for an app probatum starts, keep the sequence in one scenario (${auth.2.token}).

`timeout` means one thing everywhere: how long probatum waits before calling
it a failure. unknown keys are errors, and so is a rule of the wrong type — a
dropped rule is a check that silently asserts less. Scenarios run in file order,
with required producers first, numbered steps in declaration order, legacy checks in
list order. Execution stops
globally at the first failure or error. Each scenario gets fresh cookies
and log windows, and its process groups are cleaned up before the next scenario.
Files and databases are not reset implicitly. Every spawned process group is killed on every exit path —
even if probatum crashes or is Ctrl-C'd.

exit codes: 0 all passed · 1 a check failed (cause on screen) · 2 couldn't
run (invalid config, dirty environment, unobservable target, no applicable checks) · 101 probatum itself panicked.

Excluded checks say not_selected or os_mismatch; checks skipped after a failure
say previous_failure. If nothing applies, nothing was verified: exit 2, never green.
An unavailable capture skips its consumer with dependency_unavailable and makes
the run couldn't-run (exit 2), unless an observed failure already gives exit 1.

with --json, every outcome that returns through main emits exactly one
schema-valid document on stdout, human text staying on stderr — including an
invalid config, where `error.kind` is invalid_config and the run fields are
absent. a signal (Ctrl-C, SIGTERM) exits from the handler and emits nothing:
writing JSON there is not async-signal-safe.

JSON schema 4 keeps a flat checks list with scenario, step (null for a direct
operation), status, reason, duration_ms, captures (published names only),
output_withheld and log_file (null when not executed). The report records host_os,
selected_scenario, prerequisites, executed and excluded counts. No applicable checks gives
verdict couldn't-run and reason no_applicable_checks. Replay preserves selection.

evidence: .probatum/runs/NNNN/ (frozen config, logs, run.json — the same
document --json prints)"#;

#[derive(Debug)]
struct Options {
    help: bool,
    init: bool,
    path: Option<String>,
    scenario: Option<String>,
    seed: Option<u32>,
    json: bool,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut help = false;
    let mut json = false;
    let mut seed: Option<u32> = None;
    let mut scenario = None;
    let mut positional: Vec<String> = Vec::new();

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--help" | "-h" => help = true,
            "--json" => json = true,
            "--seed" => {
                if seed.is_some() {
                    bail!("--seed may only be specified once");
                }
                let v = it.next().context("--seed needs an integer")?;
                seed = Some(
                    v.parse()
                        .map_err(|_| anyhow::anyhow!("--seed needs an integer"))?,
                );
            }
            "--scenario" => {
                if scenario.is_some() {
                    bail!("--scenario may only be specified once");
                }
                let name = it
                    .next()
                    .filter(|v| !v.is_empty())
                    .context("--scenario needs a name")?;
                scenario = Some(name.clone());
            }
            _ if a.starts_with('-') && a != "-" => bail!("unknown option {a:?}\n{USAGE}"),
            _ => positional.push(a.clone()),
        }
    }

    match positional.first().map(String::as_str) {
        _ if help => {}
        Some("run") if positional.len() <= 2 => {}
        Some("init") if positional.len() == 1 && scenario.is_none() && seed.is_none() && !json => {}
        _ => bail!("{USAGE}"),
    }
    Ok(Options {
        help,
        init: positional.first().is_some_and(|s| s == "init"),
        path: positional.get(1).cloned(),
        scenario,
        seed,
        json,
    })
}

fn real_main(args: &[String]) -> Result<i32> {
    own::install_signal_handlers(); // Ctrl-C/kill must not leave orphans
    let options = parse_args(args).context("invalid config")?;
    if options.help {
        println!("{HELP}");
        return Ok(0);
    }
    if options.init {
        return init();
    }
    // No path? The convention file is right there — like make and Makefile.
    let path = match options.path.as_deref() {
        Some(p) => p,
        None if std::path::Path::new(DEFAULT_CONFIG).exists() => DEFAULT_CONFIG,
        // A leftover probatum.yaml is the pre-0.3 format: say so instead of
        // "no config here", which sends people looking for the wrong problem.
        None if std::path::Path::new("probatum.yaml").exists() => bail!(
            "found probatum.yaml, but probatum 0.3+ reads {DEFAULT_CONFIG} (TOML) — convert it: each `- run: x` becomes `[[check]]` + `run = \"x\"`"
        ),
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
        (s, path.to_string())
    };

    let manifest = manifest::parse(&text).context("invalid config")?;
    let plan = values::Plan::build(&manifest).context("invalid config")?;
    manifest
        .validate_selection(options.scenario.as_deref())
        .context("invalid config")?;
    let seed = options.seed.unwrap_or_else(random_seed);
    let report = runner::run(
        &manifest,
        &text,
        &source,
        seed,
        options.scenario.as_deref(),
        &plan,
    )?;

    // The evidence copy is the same document the caller gets.
    let outcome = Outcome::of_run(&report);
    if let Ok(doc) = serde_json::to_string_pretty(&outcome) {
        std::fs::write(std::path::Path::new(&report.run_dir).join("run.json"), doc).ok();
    }
    if options.json {
        outcome.print();
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

const EXAMPLE: &str = r#"# probatum.toml — probatum run (all), or probatum run --scenario smoke
# A single operation goes directly in its scenario; number steps for a sequence.
# Unknown keys/types are errors. Declare numbered steps in ascending order.
# Existing root [[check]] files also work as scenario "default"; do not mix forms.

[smoke]
run = "echo replace me with cargo test / npm test / pytest"

# A service and its checks share one scenario. It owns its services/cookies/log window.
#[api]
#os = "linux"                             # optional: linux, macos, windows
# Windows applicability does not imply Windows runtime support.

# Steps are positive numbers without leading zeros; gaps are allowed.
#[api.1]
#name = "api boots"
#run = "./myapp --port 8080"
#ready = "http://127.0.0.1:8080/healthz"   # polls until 2xx
#timeout = 15
#allow = ["known noise to ignore"]

#[api.2]
#get = "http://127.0.0.1:8080/api/version"
#expect = 200
#contains = ['"version"']

#[api.3]
#post = "http://127.0.0.1:8080/login"
#body = '{"username":"editor","password":"secret"}'
#capture = { token = "json.access_token" }

#[api.4]
#post = "http://127.0.0.1:8080/api/posts"
#body = '{"slug": "hello"}'                # defaults to Content-Type: application/json
#headers = { Authorization = "Bearer ${api.3.token}" }
#expect = 201

#[api.5]
#log = "/var/log/myapp/app.log"             # only additions during this scenario
#contains = ["started"]
#absent = ["ERROR", "panic"]

# Independent command/HTTP/log checks may also have os; service scope belongs
# on the scenario. Exclusions are reported; if nothing applies, exit is 2.
# Captures can also come from command stdout. Pass them to commands via env:
# env = { TOKEN = "${api.3.token}" }; run = 'tool "$TOKEN"' (on separate TOML lines).
# Outputs of scenarios using captures are withheld from persisted traces.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(input: &[&str]) -> Result<Options> {
        parse_args(&input.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn selection_is_one_exact_name_and_options_are_strict() {
        let o = args(&[
            "run",
            "custom.toml",
            "--scenario",
            "auth with spaces",
            "--json",
            "--seed",
            "42",
        ])
        .unwrap();
        assert_eq!(o.scenario.as_deref(), Some("auth with spaces"));
        assert_eq!(o.path.as_deref(), Some("custom.toml"));
        assert_eq!(o.seed, Some(42));
        assert!(o.json);
        for bad in [
            vec!["run", "--scenario"],
            vec!["run", "--scenario", ""],
            vec!["run", "--scenario", "auth", "--scenario", "auth"],
            vec!["run", "file", "extra"],
            vec!["run", "--scenaro", "auth"],
            vec!["init", "--scenario", "auth"],
            vec!["run", "--seed", "not-an-integer"],
        ] {
            assert!(args(&bad).is_err(), "accepted {bad:?}");
        }
        // A value that resembles an option is still an exact scenario name.
        assert!(!args(&["run", "--scenario", "--help"]).unwrap().help);
        assert!(args(&["--help"]).unwrap().help);
    }

    #[test]
    fn init_example_is_a_valid_named_scenario() {
        let manifest = manifest::parse(EXAMPLE).unwrap();
        assert!(manifest.validate_selection(Some("smoke")).is_ok());
    }
}
