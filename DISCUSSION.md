# Discussion — probatum

This document preserves the context of discussions around `probatum` so that
future conversations can resume without starting from scratch.

It deliberately separates:

- principles the project has already asserted;
- observations and proposals still under discussion;
- decisions actually made.

## Context

`probatum` is currently a v0. It executes a proof manifest made of a promise,
steps and oracles, then produces a human or machine verdict along with an
evidence directory that lets you examine and replay the run.

The central proposition is:

> Don't trust the promise. Run the proof.

The project aims to verify the real behavior of a system, beyond isolated tests
or claims made about it.

### Intended use (clarified 2026-07-14)

probatum is the single, clean-output executor for **static and/or dynamic checks
— including LLM-generated ones**. The goal is twofold:

- an LLM agent stops chaining ad-hoc shell commands and drowning in their output;
  it runs one manifest and reads one verdict (`--json`);
- the human stops writing Makefiles, Taskfiles and command-stuffed scripts; the
  manifest *is* the declarative "what to prove".

This answers the "first user" open question: the first reader is the agent (and
the human looking over its shoulder). Two consequences follow:

- because the manifest may be machine-generated, strict validation is not
  cosmetic — a silently ignored field means the agent believes it tested
  something it did not;
- because the agent consumes `--json`, `run.json` is a public contract and must
  be versioned as one.

The differentiator versus "just run bash" is exactly the three principles below:
ownership/teardown (an agent looping 50 runs never piles up orphan servers), the
collapsed verdict (the agent doesn't parse logs), and evidence/replay.

## Current project principles

These principles are already presented as non-negotiable in the README:

- the runner owns and tears down everything it launches;
- stdout and stderr are captured continuously;
- diagnosis and verdict stay deterministic, without AI;
- every run produces evidence and a replay seed;
- the output serves two readers: the human and the machine.

## First look at v0

### Perceived strengths

- The proposition is easy to state: promise, real execution, evidence, verdict.
- The manifest is immediately understandable.
- Evidence is central to the model rather than bolted on after execution.
- The separation between a mechanical verdict and its possible downstream use by
  an AI is sound.
- Terminal output and JSON output serve two complementary uses.
- The demo shows a relevant case: tests pass, but the real boot fails.
- The small scope and low dependency count suit a v0 aimed at CI and agents.

### Positioning stake

At this stage `probatum` could be superficially perceived as a YAML runner of
commands and HTTP checks. Its singularity should become far sharper with
perturbations, snapshots/diffs and replayable mutations.

The LLM-agent framing above sharpens it further: the value is not the number of
drivers but the quality, completeness and replayability of the proof, plus the
ownership guarantee that makes it safe to run in a tight agent loop.

## Confirmed gaps between principles and v0

Updated 2026-07-14: the six gaps below were **confirmed by reading the code**
(previously static-read hypotheses), with `file:line` references.

The overarching finding: probatum's own three dogfooding promises are currently
either vacuous or violated. If probatum cannot keep its own promises, its verdict
on others' promises is not yet trustworthy. Making PROOF-001/002/003 truly green
is the priority.

- **PROOF-001** (same seed → same verdict): *vacuously true*. `random_seed()`
  (`src/main.rs:73`) is drawn but drives nothing in v0, so determinism is trivial.
  It becomes meaningful only once `matrix:` mutations exist.
- **PROOF-002** (never leaves an orphan): *violated*. The correct group-kill
  teardown (`src/runner.rs:106`) only iterates `services`; a service that times
  out does `child.kill()` alone (`src/runner.rs:263`) — killing the `sh -c`, not
  the group — and is never pushed into `services`. Same on the die-before-ready
  path.
- **PROOF-003** (evidence complete even on blow-up): *violated*. `run.json` is
  written only at the end of the nominal path (`src/runner.rs:131`); there is no
  RAII / panic guard. A panic between spawning a service and the teardown loop
  leaks an orphan (PROOF-002) *and* leaves no `run.json` (PROOF-003) — one cause,
  two broken promises.

The six specific gaps:

1. A service that dies before readiness is not added to `services`, so its boot
   errors escape the global `logs.clean` oracle — the oracle reports "held"
   (green) while the step failed with ERROR lines: **self-contradictory
   evidence**. `src/runner.rs:218` vs `src/runner.rs:301`. (medium)
2. Readiness timeout calls `child.kill()` without killing the group or `wait()`ing
   → orphan. `src/runner.rs:263`. (**high — PROOF-002**)
3. `run.json` written only on the nominal path, no panic guard.
   `src/runner.rs:131`. (**high — PROOF-003**)
4. `api.check` references a `log_file` it never creates → artifact pointing at
   nothing. `src/runner.rs:284`. (low)
5. `logs.clean.level` silently ignored and unknown fields not rejected → "strictly
   declarative" is overstated. `src/manifest.rs:145`, `src/manifest.rs:167`. (medium)
6. `next_run_dir` is max+1 with no atomicity → collision under concurrency, which
   blocks the multi-agent MCP vision. `src/runner.rs:388`. (medium)

The strongest single fix: a run-scoped RAII guard (`Drop`) owning `services` +
`run_dir` that, on destruction including panic, kills every process group and
writes a partial `run.json`. One shared exit point closes #2 and #3 on all paths.

Build and lint could not be verified in the first read (crates.io unreachable
offline). `Cargo.lock` is present.

## Proposed sequencing

Order deliberately inverted: **core and dogfooding before vocabulary.**

1. Run-scoped RAII guard (teardown + evidence finalization on every exit path) → #2, #3.
2. Track dead/timed-out services (with their logs) and kill the group → #1, #2 coherent.
3. `next_run_dir` with `O_EXCL` + retry → #6, unblocks concurrency.
4. Reject unknown fields, honor `level` → #5, makes "strict" true.
5. `api.check` writes its evidence log → #4.
6. Turn PROOF-001/002/003 into hostile acceptance tests (timeouts, signals,
   children and grandchildren, internal panic, concurrency).

The feature backlog (`service.stop`, snapshots, Playwright, `matrix:`, MCP)
becomes credible only afterward. `matrix:` is also what makes PROOF-001
non-vacuous.

## Open questions

- ~~First user?~~ **Answered 2026-07-14**: the AI agent + the human over its shoulder.
- What counts as a "proof" strong enough for probatum?
- Which part of the output format must be stable from v0? (leaning: `run.json` is a
  public contract now.)
- Does replay promise the same scenario, the same verdict, or identical artifacts?
- How far should the manifest stay declarative with a closed vocabulary?
- What must durably differentiate probatum from a test runner or a check
  orchestrator? (leaning: ownership + collapsed verdict + evidence/replay, not
  driver count.)

## Decisions

- Preserve the context of exchanges in this file at the project root.
- **Documentation language is English** (README and this file translated
  2026-07-14; the repo is international). Code output strings and the demo
  manifest remain French for now — a later i18n pass if desired.
- **Initialize the project as a git repository** (the evidence/replay story needs
  version control); remove stray `:Zone.Identifier` WSL artifacts.
- **Core and dogfooding before vocabulary**: honor PROOF-001/002/003 for real
  before extending steps/oracles.
- `run.json` is treated as a public contract and will be versioned.

## Journal

### 2026-07-14 — Opening the discussion

- The project is presented as a fresh v0.
- A first overall opinion is positive: clear identity, strong proposition, good
  product potential.
- Several gaps between the announced guarantees and some v0 paths were flagged for
  discussion.
- Decision: keep the context of exchanges in this file at the project root.

### 2026-07-14 — Direction: agent ergonomics first, then RAII core

- Chosen order: **(b) ephemeral agent-generated manifests first**, then the RAII
  core. probatum is built for agents and other chaining/orchestration.
- Caveat recorded: sequence the *work* ergonomics→RAII, but do not put a real
  agent loop on it before the RAII guard lands (easy invocation + orphan leak =
  an efficient orphan factory).
- Landed (agent-ergonomics slice, verified end-to-end offline):
  - manifest from stdin (`probatum run -`) — no temp file;
  - hermetic replay: `replay` references the frozen `manifest.yaml` under the run
    dir, not a mutable origin path (works for stdin too);
  - `run.json` gains a `schema` version field and a `source` field ("<stdin>" or a
    path) — first step of treating `run.json` as a public contract.
- Deferred: structured multi-failure output for `--json` (human verdict collapses
  to one cause; the machine contract should carry per-step failure lists). Next
  after the RAII core.

### 2026-07-14 — Code-grounded review and direction

- Full read of the ~918-line codebase.
- The six gaps were confirmed against the code with `file:line` references.
- Key finding: probatum does not yet keep its own three dogfooding promises
  (PROOF-001 vacuous, PROOF-002 and PROOF-003 violated); a single missing RAII
  guard breaks two of them.
- Vision clarified: probatum is the clean-output executor for static/dynamic
  (incl. LLM-generated) checks, replacing ad-hoc command chaining and
  Makefile/Taskfile scripts. First user = the agent + the human over its shoulder.
- Decisions: docs in English, init the git repo, core+dogfooding before
  vocabulary, `run.json` as a public contract.
