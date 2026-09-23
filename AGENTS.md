# probatum

Repository guidance for coding agents, adapted from `CLAUDE.md` and the current
implementation. Keep shared project conventions consistent across both files.

## Product and boundaries

probatum is a small Rust CLI for verification: one `probatum.toml`, embedded
command/service/HTTP/log checks, a concise verdict, and evidence for diagnosis.
It runs inside the project's existing execution context; it does not provision
toolchains or environments. cidx handles outer CI orchestration.

Read `README.md` for the public contract and `DISCUSSION.md` for design rationale
and decisions. Some implementation/debt sections in `DISCUSSION.md` are historical;
check the later dated decisions, current code, and checks before treating an item
as unfinished work.

## Repository map

- `src/main.rs`: CLI, `--help`, `init` example, exit codes, JSON outcome envelope.
- `src/manifest.rs`: strict TOML parsing, scenarios, OS scopes, check types,
  parser unit tests. Preserve scenario order with TOML's `preserve_order` feature.
- `src/values.rs`: capture selectors, references, dependency validation/planning,
  safe substitution and scalar extraction.
- `src/runner.rs`: sequential execution, service lifecycle, rules, log windows,
  cookie jar, reports, and atomic evidence-directory allocation.
- `src/http.rs`: minimal HTTP/1.1 client and chunked-body decoding; currently
  supports plain `http://`, not HTTPS.
- `src/own.rs`: process-group registry, teardown guard, signal handlers.
- `src/capture.rs`, `src/diagnose.rs`, `src/verdict.rs`: output capture,
  deterministic diagnosis, and human-readable results.
- `probatum.toml`: the repository's acceptance suite; `.probatum/*.toml`: its
  scenarios. `.probatum/runs/` holds ignored evidence, not source files.
- `demo-app/`: Python fixtures and helper scripts. The "142 tests" output in
  `demo-app/tests/run.sh` is simulated demo output, not real Rust test coverage.
- `cidx.toml`, `.cidx/presets.toml`, `.github/workflows/`: CI and packaging.

## Verification — use probatum itself

Run commands from the repository root. Verify changes with:

```bash
cargo build --offline && ./target/debug/probatum run
```

The root suite builds, runs clippy with warnings denied, checks formatting, runs
Rust unit tests, exercises the demo end-to-end, and checks negative outcomes,
process ownership, concurrency, and the JSON contract. Prefer the freshly built
binary over an installed release when validating source changes.

Requirements: Rust/Cargo with clippy and rustfmt, cached dependencies, Python 3,
Bash, and a Unix environment that allows local sockets and process signals.
Fixtures use ports 8087 and 8094; do not run full suites concurrently against
the same environment. If a sandbox denies sockets, diagnose the restriction and
rerun with the appropriate execution permission; do not change product behavior
to hide the environmental failure or kill an unrelated process occupying a port.

- Build offline by default; crates.io is often unreachable here.
- For a behavior change or bug fix, add a check that would catch the regression
  to `probatum.toml` or a referenced `.probatum/*.toml` scenario.
- Use `expect = 1` for an observed failure and `expect = 2` for a refusal or
  inability to observe. Do not accept just any nonzero exit code.
- Ownership checks must also prove that the process/port was released after a
  panic (`101`), SIGINT (`130`), or SIGTERM (`143`). Shell orchestration is
  appropriate where starting, signalling, waiting, and probing are required.
- Reuse the demo's environment switches (`WAL_DIR`, `DEGRADE`, `LOG_FILE`,
  `HANG`, `AUTH`) for scenarios where they fit. Keep regression checks in the
  suite rather than standalone ad-hoc scripts.
- Focused unit tests in `src/manifest.rs` pin parser edge cases cheaply; they
  complement the end-to-end suite. Do not refactor working code merely to chase
  coverage or introduce a general fixture framework without a concrete need.
- Inspect a red run's evidence before drawing conclusions. Report checks that
  could not run; never describe a skipped or blocked suite as passing.

For focused iteration, use the relevant command below, then finish with the full
suite above:

```bash
cargo test --offline
cargo clippy --offline -- -D warnings
cargo fmt -- --check
./target/debug/probatum run .probatum/dev-check.toml
```

## Design guardrails

- A check is one source (`run`, one HTTP method, or `log`) plus flat AND rules.
  No OR, nested check logic, arbitrary conditions, explicit job dependency graphs,
  or plugins.
  Admit a new source/rule only for a real recurring need: one operation, one
  observable result, flat pass/fail rules.
- Named scenarios put one operation and optional `os` directly in `[auth]`.
  Several operations use `[auth.1]`, `[auth.2]`, etc., with scope on `[auth]`.
  Execute positive step numbers in numeric order; gaps are allowed, leading
  zeros and mixing direct/numbered operations are not. No named `check` wrapper.
  Legacy root checks are the implicit `default` scenario; do not mix forms in one file.
  `--scenario NAME` selects one exact name plus inferred capture prerequisites.
  Validate all scenarios before filtering.
  Keep OS applicability a fixed criterion, not a general conditional language.
- Named captures expose command stdout or HTTP JSON scalars. Value references
  infer prerequisites, executed once per invocation. Validate unknown/ambiguous
  references, cycles and forward step references before filtering. Respect OS
  scopes; unavailable captures skip consumers and prevent a green verdict.
  This extends the no-dependency baseline for shared results only.
- Substitute command inputs through `env`, never by injecting captured data into
  shell source. Encode URL components and serialize JSON body values correctly.
  Treat all captures as sensitive: withhold payloads and excerpts for scenarios
  declaring/consuming captures before any evidence write, including failure,
  panic and signal paths. Keep probatum's own `detail` (status, network error,
  unmet rule) with captured values replaced by `[redacted]` — a withheld failure
  must still say what went wrong. Publish names only, never captured values.
  The original config remains verbatim; no cross-invocation value cache.
- Each scenario owns its service lifecycle, cookies, and log baseline. Clean up
  before the next scenario. Preserve global fail-fast and distinguish exclusions
  (`not_selected` / `os_mismatch`) from `previous_failure` skips. Nothing
  applicable means exit 2 with `no_applicable_checks`, never a green empty run.
- `run` is a command unless `ready` or `background = true` declares a service.
  `timeout` alone is a command deadline. A command's exit code is authoritative;
  implicit crash markers apply to services, not ordinary command output.
- Preserve `0` = pass, `1` = observed failure, `2` = couldn't run, `101` = probatum
  panicked, `130` = SIGINT, and `143` = SIGTERM. Never conflate failure with an
  inability to observe. Stop execution after a failed/errored check and mark
  later checks skipped.
- Unknown keys, invalid types, and incompatible rules must be rejected; never
  silently drop a user's assertion.
- Own and clean up the process groups probatum starts on normal exit, errors,
  panic unwind, SIGINT, and SIGTERM. Keep signal handlers async-signal-safe.
  Do not claim cleanup guarantees for SIGKILL/OOM or escaped process groups.
- Never purge resources probatum does not own. Detect and refuse a dirty
  environment; user-directed cleanup is an explicit `run` check.
- External logs are evaluated from their identity/size at scenario start. Detected
  replacement or truncation makes the observation window ambiguous: exit 2.
- Preserve the schema-4 `--json`/`run.json` contract, including scenario/step identity,
  inferred prerequisites, capture names, output privacy, dependency_unavailable,
  exclusion reasons/counts and null log paths for non-executed checks. Outcomes returning
  through main, including invalid configs and caught panics, emit one JSON
  document on stdout; diagnostics belong on stderr. Signal exits emit no JSON.
- Evidence includes the frozen config and per-check output/timing. The recorded
  seed is a reference, not a guarantee of deterministic or hermetic replay.

## Documentation, CI, and releases

- Write repository documentation in English. Record substantive design decisions
  and direction changes in `DISCUSSION.md`.
- Keep `README.md`, the `HELP` and `EXAMPLE` strings in `src/main.rs`, and the
  acceptance checks aligned when changing the public surface. `--help` should
  remain sufficient to use the tool without external documentation.
- Keep changes small and purposeful; preserve the offline build and minimal
  dependency footprint. Avoid speculative features and unrelated refactors.
- `.github/workflows/ci.yml` runs the native full suite. The probatum cidx
  preset runs `.probatum/selfcheck.toml` against the packaged image, which has
  a shell but no project toolchains; this does not replace the native suite.
- `.github/workflows/cidx.yml` is generated from `cidx.toml`; use
  `cidx generate github -o .github/workflows/cidx.yml` for regeneration.
- Before committing, remove disposable run artifacts you created under
  `.probatum/runs/` and `demo-app/data/app.log` after inspecting useful evidence.
  Never remove committed `.probatum/*.toml` scenarios or the demo WAL fixtures.
- Use conventional commits (`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`,
  `test:`; `!` or `BREAKING CHANGE:` for breaking changes).
- Do not bump versions by hand. For a requested release, `cidx release create`
  computes the bump from commits and updates Cargo.toml, Cargo.lock, `.cz.toml`,
  and CHANGELOG.md through a PR. The merged commit is tagged and `release.yml`
  publishes it; then update the image tag in `.cidx/presets.toml`.
