# probatum

> Don't trust the promise. Run the proof.

`probatum` runs a **proof manifest**: a promise, steps, oracles. One command,
one verdict. Green: everything holds. Red: the cause is already on screen —
correlated logs, context, complete artifacts, replay seed.

## v0 — what works

```bash
probatum run proofs/dev-check.yaml           # human verdict (terminal)
probatum run proofs/dev-check.yaml --json    # machine verdict (AI agents, CI)
probatum run proofs/dev-check.yaml --seed N  # referenced replay
```

Manifest (strictly declarative — no logic, closed vocabulary):

```yaml
proof: dev-check
promise:
  id: DEV-000
  statement: The build is healthy — tests pass, the service boots, no error log at startup.
steps:
  - suite.run:     {cmd: "bash demo-app/tests/run.sh"}
  - service.start: {cmd: "python3 demo-app/app.py", ready: "http://127.0.0.1:8087/healthz", timeout: 15}
  - api.check:     {get: "http://127.0.0.1:8087/api/version", expect: 200}
oracles:
  - logs.clean: {level: error, allow: ["migration pending"]}
```

Non-negotiable principles (v0 → vN):

- **The runner owns what it launches**: continuous capture of everything
  (timestamped stdout/stderr), supervision, teardown by process group — never an
  orphan environment.
- **Deterministic diagnosis**: panic/FATAL first, otherwise the last ERROR, with
  correlated lines. No AI in the verdict.
- **Evidence by construction**: every run writes `.probatum/runs/NNNN/` — frozen
  manifest, complete logs, `run.json`, seed.
- **Two readers**: the human (terminal block) and the agent (`--json`).

## Intended use

probatum is the single, clean-output executor for **static and/or dynamic checks
— including LLM-generated ones**. An agent runs one manifest and reads one verdict
instead of chaining shell commands and drowning in their output; a human stops
writing Makefiles, Taskfiles and command-stuffed scripts, because the manifest
*is* the declarative "what to prove". The first reader is the agent (via `--json`)
and the human looking over its shoulder.

## Demo

`demo-app/` is an event-sourced app whose unit tests mock the store: they pass,
but the real boot replays the WAL — and segment 0004 is missing.
Run `rm demo-app/data/wal/segment-0004.json` then
`probatum run proofs/dev-check.yaml` to see the red verdict with the extracted
cause.

## Backlog (in order)

1. `service.stop` / `service.restart` / `network.cut` — the perturbations, the reputation.
2. `state.snapshot` / `state.diff` — the differential oracle.
3. Playwright driver, Compose driver (topologies).
4. `matrix:` — deterministic mutations driven by the seed.
5. Promise registry + `evidence/` per release.
6. MCP server — the agent calls `probatum.run`, receives the verdict.

## Dogfooding

The first system tested by probatum is probatum. First promises:

- `PROOF-001`: two runs with the same seed yield the same verdict.
- `PROOF-002`: a run never leaves an orphan environment behind.
- `PROOF-003`: the evidence directory is complete even when the run blows up.

> **Status (2026-07-14): these three promises are not yet honored** — see
> [DISCUSSION.md](DISCUSSION.md). PROOF-001 is currently vacuous (the seed drives
> nothing), PROOF-002 leaks on the timeout/panic paths, PROOF-003 has no crash
> guard. Making them truly green is the priority before extending the vocabulary.
