# probatum

Test-oriented check runner: one `probatum.yaml`, embedded checks (run/get/log),
only the failures that matter. Rust, ~1k lines, offline-buildable.

## Dogfooding — the rule of this repo

**probatum is its own test suite.** Verify any change with the tool itself:

```bash
cargo build --offline && ./target/debug/probatum run
```

The root `probatum.yaml` builds, lints (clippy -D warnings), runs the demo
end-to-end (service + HTTP + external log) and asserts the negative scenarios
are *caught* (exit 1 exactly).

- Fixed a bug or added behavior? **Add a check that would have caught it** to
  `probatum.yaml` or `.probatum/*.yaml` — not an ad-hoc test script. Negative
  scenarios live as env switches on `demo-app/app.py` (`WAL_DIR`, `DEGRADE`,
  `LOG_FILE`, `HANG`) + an inverted check (`...; test $? -eq 1` for caught
  failures, `-eq 2` for couldn't-run refusals, `-eq 101` + port probe for
  probatum's own crash).
- A dogfooding run that goes red is a finding, not an annoyance — it already
  caught one real doc/code gap (missing ERROR markers in the service filter).

## Conventions

- Build offline: `cargo build --offline` (crates.io is often unreachable here).
- Docs in English. `DISCUSSION.md` is the design log — record decisions and
  direction changes there (it's how sessions resume without context loss).
- Before committing: `rm -rf .probatum/runs demo-app/data/app.log` (run
  artifacts are gitignored but keep the tree clean).

## Design guardrails (frozen — see DISCUSSION.md for the why)

- A check = one source (`run` / `get` / `log`) + flat AND rules. No OR, no
  nesting, no logic in the config — the day it needs an `if`, the design failed.
- New verb/rule admission test: *one operation, one observable result, flat
  pass/fail rules* — and only against a real, recurring need.
- failed (exit 1) ≠ couldn't-run (exit 2). Never conflate them: a false
  "failed" makes users chase ghosts.
- probatum never purges what it doesn't own — dirty environments are detected
  and refused, not destroyed.
- Unknown config keys are errors. A typo must never silently skip a check.
