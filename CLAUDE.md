# probatum

Test-oriented check runner: one `probatum.toml`, embedded checks (run/get/post/log),
only the failures that matter. Rust, offline-buildable.

## Dogfooding — the rule of this repo

**probatum is its own test suite.** Verify any change with the tool itself:

```bash
cargo build --offline && ./target/debug/probatum run
```

The root `probatum.toml` builds, lints (clippy -D warnings), runs the demo
end-to-end (service + HTTP + external log) and asserts the negative scenarios
are *caught* (exit 1 exactly).

- Fixed a bug or added behavior? **Add a check that would have caught it** to
  `probatum.toml` or `.probatum/*.toml` — not an ad-hoc test script. Negative
  scenarios live as env switches on `demo-app/app.py` (`WAL_DIR`, `DEGRADE`,
  `LOG_FILE`, `HANG`) + an inverted check (`...; test $? -eq 1` for caught
  failures, `-eq 2` for couldn't-run refusals, `-eq 101` + port probe for
  probatum's own crash, `-eq 130` + port probe for Ctrl-C/SIGINT).
- A dogfooding run that goes red is a finding, not an annoyance — it already
  caught one real doc/code gap (missing ERROR markers in the service filter).

## Conventions

- Build offline: `cargo build --offline` (crates.io is often unreachable here).
- Docs in English. `DISCUSSION.md` is the design log — record decisions and
  direction changes there (it's how sessions resume without context loss).
- Before committing: `rm -rf .probatum/runs demo-app/data/app.log` (run
  artifacts are gitignored but keep the tree clean).
- Commits follow conventional commits (`feat:`, `fix:`, `docs:`, `chore:`,
  `refactor:`, `test:`; `!` or `BREAKING CHANGE:` for a breaking change).
  commitizen checks them in CI (cidx `code` phase). Do NOT bump the version
  by hand: `cidx release create` computes it from the commits since the last
  tag, bumps Cargo.toml/Cargo.lock/.cz.toml + CHANGELOG.md through a PR, tags
  the merged commit, and release.yml publishes. Then bump the image tag in
  `.cidx/presets.toml`.

## Design guardrails (frozen — see DISCUSSION.md for the why)

- A check = one source (`run` / an HTTP method / `log`) + flat AND rules. No OR,
  no nesting, no logic in the config — the day it needs an `if`, the design
  failed. Capture references are the one sanctioned dependency (see below).
- Named scenarios put one operation and optional `os` directly in `[auth]`.
  Several operations use `[auth.1]`, `[auth.2]`, etc., with scope on `[auth]`.
  Step numbers are positive, ordered numerically, may have gaps, and have no
  leading zeros. Do not mix direct/numbered operations or add a `check` wrapper.
  `--scenario NAME` selects one plus capture prerequisites; legacy root checks
  remain scenario `default`.
  Validate the whole file before filtering; keep OS scope a fixed criterion.
  Each scenario owns its services, cookies and log baseline. Keep global
  fail-fast; distinguish exclusions and dependency/failure skips in JSON schema 4.
  Nothing applicable means exit 2 (`no_applicable_checks`), never a green empty run.
- Named captures expose stdout or HTTP JSON scalars; references infer prerequisites
  executed once per invocation. Validate references/cycles before filtering and
  respect OS scope. An unavailable capture prevents a green consumer verdict.
  Pass command values through `env`, never shell-source substitution. Captures
  are sensitive: withhold what the system under test said (bodies, output, log
  excerpts, `cause`) for producer and consumer scenarios, even on failure, panic
  and signals — but keep probatum's own `detail`, with captured values replaced
  by `[redacted]`: a masked failure must still say what went wrong. Reports
  expose names only; frozen config stays verbatim. This adds no expressions or
  shared service lifetimes: a service dies with its scenario.
- New verb/rule admission test: *one operation, one observable result, flat
  pass/fail rules* — and only against a real, recurring need.
- failed (exit 1) ≠ couldn't-run (exit 2). Never conflate them: a false
  "failed" makes users chase ghosts.
- probatum never purges what it doesn't own — dirty environments are detected
  and refused, not destroyed.
- Unknown config keys are errors. A typo must never silently skip a check.
