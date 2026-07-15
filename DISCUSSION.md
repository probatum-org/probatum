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

### Pivot (2026-07-14): from proof engine to test-oriented runner

The proof framing (promise, oracles, evidence registry) was the assistant's
baggage, not the owner's need. The owner's actual requirement:

> One line, one config file: run my checks, and surface only the errors and
> info that matter.

The identity is therefore: **like a Makefile or Taskfile, but test-oriented,
with embedded features**. The curl is embedded, the grep is embedded, the
process supervision is embedded; the user only declares the rules that make a
check pass or fail.

The model: a check = **source × flat rules**.

- Sources: `run:` (a command), `get:` (an HTTP endpoint — embedded client),
  `log:` (an external log file — embedded matching, no shell-out to grep).
- Rules: exit 0 (implicit for `run`), `expect:` (HTTP status), `contains:`
  (must appear), `absent:` (must not appear), plus `ready:`/`timeout:` to
  start a service and keep it alive for later checks.
- Rules are a flat AND list. No general OR, no nesting, no logic. Two checks are
  also an AND and therefore do not emulate OR; common list-valued alternatives
  may be considered later. This protects the compass: a human or an LLM writes
  the file correctly on the first try, without reading documentation.

Design points settled during the discussion:

- **The noise filter is ~90% of the value.** Zero-config defaults (panic,
  traceback, FATAL, last ERROR) must work out of the box; `fail_on`/`allow`
  are app-specific exceptions, not the norm. If everything must be configured,
  the tool is a worse Makefile.
- **The filter must be humble.** Strong collapse when the signal is sure
  (panic, clean traceback); when unsure (exit 1 with no recognized marker),
  show the tail and say "exit 1" — never invent a cause. A false cause is
  worse than no cause.
- **Features, not plugins.** A small curated set of built-in verbs; no
  extension mechanism, no registry, no remote imports. A verb earns its place
  only if it asserts over something the tool already owns or embeds (process
  tree, captured output, HTTP response, log window).
- **Two log intentions, two rules.** `absent` (errors must not appear) and
  `contains` (positive confirmation must appear) are distinct declarations. An
  empty or missing log with `absent` is "couldn't run", not a silent pass.
- **Prefer the strongest signal.** HTTP 200 beats grepping "Listening on
  8080"; log matching is for what has no better signal.
- **External log files are in scope** — evaluated **from run start**: the file
  offset is noted at start and only new lines count. A target log that already
  exists before the run is normal; its identity and size establish the initial
  observation boundary. Unexpected replacement, truncation or rotation during
  the window makes the check ambiguous and therefore `couldn't-run`.
- **Detection over destruction.** probatum detects a dirty environment
  (pre-existing log, occupied port, already-running instance) and reports
  "couldn't run: environment not clean". It never purges what it does not own —
  purging is the user's own first `- run:` check (setup is just a check).
  Cheap sandbox for now (clean start + detection + declared setup); real OS
  isolation is a separate, later decision.
- **No template layer.** "Template" meant an example config to copy. A good
  example (possibly shipped by `probatum init`) is enough; parameterized
  reusable checks are reconsidered only if duplication starts hurting.
- **Sequential, top to bottom.** A log check comes after the traffic that
  produces its lines. No implicit parallelism in v1.

### Post-pivot refinements

The simplification is substantive rather than cosmetic: the former model had
conceptual depth, while the new model gives a user an immediate reason to run
the tool. A few semantics still need tightening before the config contract is
frozen.

#### OR is not two checks

The earlier statement "need an OR -> write two checks" is incorrect: two
top-level checks form part of the global sequential AND, so both must pass. Two
HTTP checks cannot express "200 or 204" against one response.

The accepted v1 direction is to keep general boolean logic out of the config.
Whether selected rules later accept a small list such as `expect: [200, 204]`
remains open; until then, unsupported OR semantics must be stated honestly
rather than approximated with two checks.

#### Existing log file is not automatically dirty

The run-start offset exists precisely so a long-lived log file can be observed
without deleting its history. Mere pre-existence is therefore not sufficient to
classify the environment as dirty.

At run start, probatum can record the file identity and size, then evaluate only
new bytes. Actual ambiguity includes replacement, unexpected truncation or
rotation during the observation window. A pre-existing service or occupied port
is dirty when it conflicts with a service probatum intends to start; an existing
log file by itself is normal.

#### Defaults are part of each source contract

Zero-config behavior must be defined per source, not left as filter folklore.
The remaining questions include:

- `run`: non-zero exit is a failure; do recognized fatal markers also fail a
  command that exits zero?
- `get`: does omitted `expect` mean exactly 200, any 2xx, or no status rule?
- `log`: must at least one of `contains` or `absent` be declared?
- missing expected content after valid observation is `failed`; unreadable or
  ambiguously rotated input is `couldn't-run`.

Good defaults should remove repetitive configuration without making invisible
assertions surprising.

**Answered 2026-07-14 (contract frozen, implemented):**

- `run`: exit code is the authority. Default crash markers do NOT fail an
  exit-0 command (a passing `cargo test` legitimately prints "panicked at" for
  `should_panic` tests). Explicitly declared `contains`/`absent` apply to the
  output even on exit 0.
- Service (`ready:`/`timeout:`): default crash markers ON — there is no exit
  code to trust while it runs; `allow` exempts known noise.
- `get`: omitted `expect` = any 2xx (owner's choice). `contains` applies to
  the body.
- `log`: at least one rule required, otherwise config error (owner's choice —
  zero invisible assertions). Missing file, replacement or truncation during
  the window → couldn't-run. **Deviation to confirm**: an empty window with
  `absent`-only rules passes with an explicit "(0 new line(s) checked)" detail
  rather than couldn't-run — watching an error log that stays empty is the
  success case, and the count keeps the pass from being silent.
- Vocabulary unification: `fail_on` folded into `absent` — the same rule words
  on every source; `allow` is reserved for the service crash filter.
- Sequential stop at first failed/errored check; the rest is reported skipped.
- Run verdict: `pass | fail | couldn't-run` (failed wins over errored); exit
  codes 0/1/2.

#### A check may perform an operation

"Setup is just a check" is intentionally pragmatic, but a setup command mutates
the environment rather than making a pure assertion. The honest general model
is:

> A check performs one operation and applies pass/fail rules to its observable
> result.

This definition covers setup commands, service starts, HTTP requests and log
inspection without pretending every config entry is a pure assertion.

#### Product language must follow the pivot

`Don't trust the promise. Run the proof.` remains strong project DNA, but it no
longer describes the config surface or primary product category. The main pitch
should become direct and check-oriented; candidates raised during discussion
include:

> Run the checks. See what matters.

and:

> One config. Clean checks. Useful failures.

No replacement tagline is decided yet.

What carries over from the proof-engine phase, in plainer words:

- ownership/teardown (the RAII core) → "the service you started gets killed,
  no zombie port between runs";
- violated vs inconclusive → **failed vs couldn't-run** — same invariant, same
  value (a false "failed" makes you chase ghosts), simpler words;
- strict validation (unknown key = error) — a generated config must not
  silently claim a check that never ran;
- `--json` with a versioned schema for agents;
- continuous timestamped capture as the substrate for log rules.

Dropped or shelved: the promise/oracle ceremony (`proof:`, `promise:`),
seed-driven mutations (`matrix:`), the per-release evidence registry, and
perturbations as the core identity. Any of these may return later as features;
none of them is the identity anymore.

State note: the committed code (`d4712f2`) still implements the pre-pivot
design; an uncommitted working-tree rewrite implements most of the new one
(see journal).

### Boundary with cidx (2026-07-15)

The owner also maintains [cidx](https://github.com/cidx-org/cidx), a
declarative CI/CD runner: `cidx.toml` declares *tools* to run against the
source (trivy, gitleaks, golangci-lint, go-test…), containerized via presets,
grouped in phases and pipelines, with GitHub/GitLab workflow generation.

Not a duplicate — the two verify different objects:

- **cidx**: the *code*, statically — which scanners/linters/test tools run,
  identically local and CI.
- **probatum**: the *running system*, dynamically — boot, readiness, HTTP
  behavior, log window, teardown. cidx has none of that machinery.

The honest overlap: both can run an arbitrary command, so a custom cidx stage
*could* script its way to the same checks. probatum's value inside that
overlap is concentration: embedded primitives, process ownership, the noise
filter, failed ≠ couldn't-run, and the agent-readable verdict — one powerful,
expressive check runner instead of a pile of stage scripts.

Sharing rule: *run a tool against my sources* → cidx stage; *start my system
and observe its behavior* → probatum.

**Owner's decision**: probatum maps to the **test phase of the DevOps loop** —
in cidx terms it belongs to `cidx run test` (e.g. `[test] containers =
["go-test", "probatum"]`), not to a separate phase. Do not re-declare
lint/scan/unit-test stages inside `probatum.yaml` in repos that have cidx
(the probatum repo's own build+clippy checks are the dogfooding exception).

Guardrails both ways: if probatum ever wants phases/pipelines/presets it is
becoming cidx — refuse; if cidx ever wants readiness/process ownership it is
becoming probatum — same.

Integration work item (packaging, not design): a static musl binary or a
small image so probatum fits cidx's containerized presets.

**Decided (2026-07-15): separate tools, linked by a preset — no merge.**
cidx never absorbs its tools (trivy, gitleaks, go-test are all external
binaries it orchestrates); probatum joins that roster like any other. cidx
launches everything *too*, not *exclusively*: the inner loop (a dev or an
agent, in seconds) calls `probatum run` directly; the outer loop (pipeline,
CI) reaches it through `cidx run test`. Both paths read the same
`probatum.yaml` — two launchers, one source of truth. Reinforcing reasons:
containerized cidx vs native process ownership, Go vs Rust and different
release rhythms, and probatum's non-cidx uses (MCP/agents, repos without
cidx).

## Current project principles

*Historical pre-pivot wording. The ownership, deterministic diagnosis and dual
human/machine output principles survive; promise/evidence/seed language does
not define the current product surface.*

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

One positioning boundary is now explicit: probatum is not intended to replace
general task automation. A Makefile or Taskfile can describe how to build an
artifact and manage dependencies; probatum describes what must be established
as true. It may replace ad-hoc verification scripts, even when both happen to
run similar commands.

> A task says "do X". A proof says "establish that P is true".

This boundary is about the contract, not the mechanics: probatum rejects the
"do X" contract, not every mechanism a task runner happens to share. Some of
those mechanisms are proof-relevant — notably parallelism (starting three
services concurrently can be part of establishing P) — and are in scope when a
proof needs them.

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

### RAII and evidence finalization are related but distinct

The initial RAII proposal is refined as follows:

- a `Drop` guard makes a best-effort teardown attempt unavoidable, including
  during panic unwinding — but it does only non-panicking work (`libc::kill` on
  the process groups, bounded reaping where possible), no allocation and no
  serialization;
- an explicit `catch_unwind` boundary around the run body produces the public
  `run.json` for every unwinding outcome.

Serializing and writing the report directly from `Drop` is not just fragile, it
is dangerous: in Rust, a panic *while already unwinding* (a panic inside a `Drop`
that runs during unwind) calls `abort()` — the process dies and no evidence is
written, worse than today. Fallible work therefore belongs in the `catch_unwind`
boundary, not in `Drop`.

The verdict/outcome model (single source of truth, replacing the earlier
conflicting lists):

- `verdict: held | violated | inconclusive` — the promise axis; maps to the
  human UI green / red / grey ("I don't know" is grey, not red);
- `error: { kind, message }` — present only when `inconclusive`, with
  `kind ∈ { invalid_manifest, execution_error, internal_error, interrupted }`.

Mixing a real proof result (`violated`) with a tooling failure
(`invalid_manifest`) in one field would force every consumer to know all values
just to answer "did the promise hold?". Two fields serve the two readers: the
human reads `verdict`, the machine switches on `error.kind` (invalid_manifest →
do not retry, execution_error → maybe retry, internal_error → report a bug).

"A file always exists" and "the evidence is complete" are separate guarantees —
and neither can be absolute. `catch_unwind` covers exit, internal error and
panic-unwind, but not a hard kill (SIGKILL, OOM, stack overflow, double-panic).
The honest form of PROOF-003 is therefore: *`run.json` is guaranteed for every
outcome probatum can observe while unwinding; an external hard kill stays out of
scope.*

### Replay fidelity

Freezing the manifest in the run directory gives probatum a stable replay
recipe, but not yet a hermetic replay. Commands may still depend on the current
commit and files, toolchain versions, environment variables, network services,
time and machine state.

The accurate v0 term is therefore **frozen replay recipe** or **replay by
reference**. A stronger replay guarantee would require capturing or controlling
some combination of revision, working directory, environment, tool versions and
external inputs.

### The machine contract must cover tool failures

If the agent is the first reader, `--json` must describe probatum's own failures
as well as held or violated promises. The target property:

> With `--json`, every outcome produces exactly one schema-valid JSON document
> on stdout.

Outcomes are expressed with the two-field model above (`verdict` +
`error.kind`), not a flat enum. This has a concrete implementation consequence:
`--json` must be parsed **before** manifest loading can fail, and every error
path must emit a JSON document to **stdout** (human text staying on stderr) — a
restructuring of `main.rs`, not only the runner. The document is a
**discriminated union with optional fields**: `run_dir`, `steps` and `oracles`
are absent when there is no run to report (e.g. `invalid_manifest`). Distinct
process exit codes remain alongside the structured document.

### Manifest trust boundary

Strict validation prevents a generated manifest from silently claiming a check
that was never performed. It does not make an arbitrary `cmd` safe to execute.
The project must eventually state whether a manifest is:

- trusted executable code;
- an untrusted proof request governed by an authorization policy; or
- accepted in both modes, with a restricted mode using closed drivers and no
  unrestricted `sh -c` escape hatch.

Sandboxing is not necessarily a v0 responsibility, but the trust assumption
must be explicit before unsupervised agent loops become a supported use case.
For v0 the declared mode is **trusted executable code** (the same trust as a
Makefile one chooses to run); declaring the mode is a v0 responsibility, its
enforcement is not.

Key convergence: closing the vocabulary — typed drivers (`http.get`,
`process.start` with an argv) instead of an open `sh -c` — makes enforceable
sandboxing possible, and it simultaneously improves replay fidelity, because a
typed driver is capturable and controllable where opaque shell is not. Typed
drivers are not a security boundary by themselves: an argv can still invoke a
destructive program, HTTP can enable SSRF, file operations can escape the
workspace, and child processes inherit probatum's ambient authority.

A future untrusted mode therefore requires the combination:

```text
typed drivers + policy/allowlist + OS isolation
```

Two of the three design axes (manifest trust, replay fidelity) still converge on
the same initial investment: shrink the `sh -c` surface and capture the
environment. Only proof strength (perturbations, oracles) is orthogonal. The
restricted-driver mode remains a high-leverage future step, but it is the basis
for a sandbox, not the complete sandbox.

### Violation versus inconclusive

The central semantic boundary is whether a failed execution positively
establishes that the promise is false or merely prevents probatum from deciding.
The accepted invariant is:

```text
held         => the proof completed and every oracle held
violated     => the proof completed sufficiently to establish at least one violation
inconclusive => probatum could not establish either result reliably
```

A proof does not need to finish every planned step once a violation has been
positively established. Conversely, absence of evidence is never evidence of
absence. A timeout, missing executable, occupied port or unreachable endpoint
cannot be classified from mechanics alone in every case: its meaning may depend
on what the promise and oracle claim.

The design question for each driver and oracle is therefore:

> When does failed execution constitute evidence of violation, and when does it
> make the proof inconclusive?

Examples leaning toward `inconclusive` include probatum being unable to spawn a
required tool or losing the ability to observe the target. A test suite that
runs successfully and returns a defined failing result leans toward `violated`.
Ambiguous infrastructure failures require explicit semantics rather than a
blanket mapping.

### Invocation outcome versus run report

The machine-facing stdout document and the persisted `run.json` share a schema
family but have different lifetimes and guarantees:

```text
invocation outcome  — exists for every observable CLI invocation
run report          — exists only after a run has actually been created
```

For example, `invalid_manifest` can produce a structured invocation outcome
without a `run_dir` or persisted `run.json`. The discriminated union must make
that absence explicit rather than fabricate an empty run.

All output guarantees have a physical ceiling. Disk full, an unwritable
directory or a closed stdout pipe can prevent persistence or delivery. The
architectural guarantee is that probatum constructs and attempts to emit exactly
one schema-valid document for every observable outcome, provided the output
channel remains available.

Build and lint could not be verified in the first read (crates.io unreachable
offline). `Cargo.lock` is present; the project builds offline now.

## Proposed sequencing

*(Pre-pivot. The engineering items survive — teardown guard, tracking all
spawned services, strict validation, atomic run dirs; the proof-vocabulary
items do not. See the post-pivot decisions and journal for the current plan.)*

Order deliberately inverted: **core and dogfooding before vocabulary.**

1. Run-scoped RAII guard (teardown + evidence finalization on every exit path) → #2, #3.
2. Track dead/timed-out services (with their logs) and kill the group → #1, #2 coherent.
3. `next_run_dir` with `O_EXCL` + retry → #6, unblocks concurrency.
4. Reject unknown fields, honor `level` → #5, makes "strict" true.
5. `api.check` writes its evidence log → #4.
6. Turn PROOF-001/002/003 into hostile acceptance tests (timeouts, signals,
   children and grandchildren, internal panic, concurrency).

The guard should own teardown, while evidence finalization should have an
explicit, schema-valid recovery path rather than relying on fallible work in
`Drop`. The normal path performs controlled teardown and records its result;
`Drop` is the non-panicking, best-effort safety net. System calls such as `kill`
and `wait` can fail, so "infallible" means that the destructor itself never
panics, not that cleanup is physically guaranteed.

The feature backlog (`service.stop`, snapshots, Playwright, `matrix:`, MCP)
becomes credible only afterward. `matrix:` is also what makes PROOF-001
non-vacuous.

## Open questions

- ~~First user?~~ **Answered 2026-07-14**: the AI agent + the human over its shoulder.
- What counts as a "proof" strong enough for probatum?
- Which part of the output format must be stable from v0? (leaning: `run.json` is a
  public contract now.)
- Does replay promise the same scenario, the same verdict, or identical artifacts?
- Which environment and input metadata must be captured before replay can claim
  more than a frozen recipe?
- How far should the manifest stay declarative with a closed vocabulary?
- ~~Is a manifest trusted executable code or an untrusted proof request?~~
  **Answered for v0**: trusted executable code. A future untrusted mode requires
  typed drivers, enforceable policy and OS isolation.
- How should each driver distinguish positive evidence of violation from an
  infrastructure or observation failure that makes the proof inconclusive?
- Which fields belong to the common schema family, and which are specific to an
  invocation outcome or a persisted run report?
- What are the exact zero-config defaults for each source (`run`, `get`, `log`)?
- Should a small number of rules accept lists for common alternatives, or should
  all OR semantics remain unsupported in v1?
- What check-oriented tagline replaces or complements the historical proof
  slogan?
- What must durably differentiate probatum from a test runner or a check
  orchestrator? (leaning: ownership + collapsed verdict + evidence/replay, not
  driver count.)

## Decisions

### Post-pivot (2026-07-14)

- **Pivot**: probatum is a test-oriented check runner — "a Makefile/Taskfile
  with embedded features, oriented toward tests" — not a proof engine. The
  promise/oracle ceremony is dropped from the config surface.
- A check = one source (`run` | `get` | `log`) + a flat AND list of rules
  (`expect`, `contains`, `absent`, `ready`/`timeout`). No OR, no nesting, no
  logic in the config.
- Primitives are embedded (HTTP client, log matching, process supervision);
  no shell-out to curl/grep.
- Zero-config crash detection is the default; `fail_on`/`allow` are per-app
  exceptions. The filter never invents a cause: unsure → tail + exit code.
- External log files: evaluated from run start only; a pre-existing target log
  is not automatically dirty. Record its identity and offset, inspect only new
  content, and report `couldn't-run` on ambiguous truncation/replacement. An
  occupied port or already-running instance is dirty when it conflicts with a
  service probatum intends to start.
- Detection over destruction: probatum never purges what it doesn't own;
  cleanup/setup is an ordinary first `- run:` check.
- No plugin system; no template layer — one example config (and possibly a
  `probatum init`) instead.
- Sequential top-to-bottom execution in v1.
- General OR/nested logic remains out of v1. Two checks do not express OR;
  selected list-valued rules such as `expect: [200, 204]` remain an open design
  option.
- A check is allowed to mutate state: it performs one operation and applies
  pass/fail rules to the observable result. Setup is pragmatic, not a pure
  assertion.
- Kept from before, renamed or unchanged: failed ≠ couldn't-run (was violated ≠
  inconclusive); strict unknown-key rejection; `--json` versioned schema;
  ownership/teardown as the core guarantee.
- **Naming convention (owner's choice)**: tool = file = directory. The default
  config is `probatum.yaml` at the repo root (`probatum run` with no argument
  finds it, like make/Makefile); `.probatum/` holds secondary check files
  (committed) and `.probatum/runs/` the evidence (ignored). "checks.yaml" was
  too generic.

### Pre-pivot (historical)

Decisions below predate the pivot. Those tied to the proof vocabulary
(promise/oracle naming, `held|violated|inconclusive`, seed/matrix, the
evidence registry) are superseded in naming, but their engineering content
largely carries over as noted in the pivot section.

- Preserve the context of exchanges in this file at the project root.
- **Documentation language is English** (README and this file translated
  2026-07-14; the repo is international). Code output strings and the demo
  manifest remain French for now — a later i18n pass if desired.
- **Initialize the project as a git repository** (the evidence/replay story needs
  version control); remove stray `:Zone.Identifier` WSL artifacts.
- **Core and dogfooding before vocabulary**: honor PROOF-001/002/003 for real
  before extending steps/oracles.
- `run.json` is treated as a public contract and will be versioned.
- probatum targets ad-hoc verification scripts, not general-purpose build or task
  automation: its contract is to establish a proposition, not merely perform a
  task.
- The frozen manifest currently provides a replay recipe, not a hermetic replay;
  replay strength must be described honestly and strengthened deliberately.
- Process teardown belongs in a run-scoped RAII guard; fallible evidence
  finalization should use an explicit recovery path rather than `Drop` alone.
- Outcome model: `verdict: held | violated | inconclusive` plus an
  `error { kind, message }` field (kinds: invalid_manifest, execution_error,
  internal_error, interrupted). The human reads `verdict`; the machine switches
  on `error.kind`.
- `Drop` does best-effort, non-panicking teardown only; `run.json` is written
  from a `catch_unwind` boundary. PROOF-003 is stated honestly: guaranteed for
  every observable unwinding outcome while the evidence channel remains
  writable, not against a hard external kill (SIGKILL, OOM, double-panic).
- v0 declares manifests to be **trusted executable code**; enforcement via
  restricted typed drivers is a later milestone. Closing the vocabulary makes
  enforceable sandboxing possible and improves replay fidelity, but typed
  drivers alone are not a sandbox; an untrusted mode also needs policy and OS
  isolation.
- `--json` will guarantee exactly one schema-valid document per outcome on
  stdout (discriminated union, optional fields); this requires parsing `--json`
  before manifest load.
- Verdict invariant: `held` means all applicable oracles held; `violated` means
  sufficient positive evidence established at least one violation;
  `inconclusive` means neither result could be established reliably. Absence of
  evidence is not evidence of absence.
- Distinguish the invocation outcome emitted for every observable CLI invocation
  from the persisted run report, which exists only once a run has been created.
- Output and evidence guarantees are conditional on their physical channels
  remaining writable; probatum guarantees construction and attempted emission,
  not success against disk-full, permissions failure or a closed pipe.
- Normal execution performs controlled teardown and reports its result; `Drop`
  is a best-effort, non-panicking safety net. Cleanup system calls may fail.

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
  - frozen replay recipe: `replay` references the frozen `manifest.yaml` under
    the run dir, not a mutable origin path (works for stdin too); this is not yet
    a hermetic replay because the execution environment and inputs are not
    frozen;
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

### 2026-07-14 — Proof boundary, replay fidelity and manifest trust

- Clarified that probatum replaces ad-hoc verification scripts, not Makefiles or
  Taskfiles as general automation systems: "do X" and "establish P" are
  different contracts.
- Refined the RAII design: `Drop` guarantees teardown; explicit finalization or
  recovery produces a schema-valid report, including incomplete outcomes.
- Reclassified the current replay feature as a frozen replay recipe rather than
  a hermetic replay.
- Raised a machine-contract target: under `--json`, every outcome should produce
  exactly one schema-valid JSON document on stdout.
- Identified the manifest trust boundary: strict parsing does not make arbitrary
  shell commands safe. Trusted-code, policy-governed and restricted-driver modes
  remain to be decided.
- The three connected design axes are now: **proof strength, replay fidelity and
  manifest trust**.

### 2026-07-14 — Outcome model, abort ceiling, trust/vocabulary convergence

- Reconciled the two conflicting outcome lists into one model: `verdict`
  (held | violated | inconclusive) plus `error { kind, message }`.
- Named the abort ceiling: `catch_unwind` cannot catch SIGKILL/OOM/double-panic,
  so PROOF-003 is stated as guaranteed only for observable unwinding outcomes.
- Corrected two earlier points: "hermetic replay" was an overclaim (it is a
  frozen replay recipe); writing `run.json` from `Drop` is dangerous (a panic
  during unwind → `abort()`), so finalization moves to a `catch_unwind` boundary
  and `Drop` does best-effort, non-panicking teardown only.
- Established the convergence: closing the vocabulary (typed drivers, no `sh -c`)
  is both the sandboxing strategy and a replay-fidelity improvement — two of the
  three axes share one investment.
- Next: implement the RAII core to this design (Drop teardown + catch_unwind
  finalize + ternary verdict + error.kind).

### 2026-07-14 — Verdict semantics and guarantee boundaries

- Defined the core verdict invariant: `violated` requires positive evidence;
  inability to establish either truth or violation is `inconclusive`.
- Recognized that failed execution cannot always be classified from mechanics
  alone. Each driver/oracle must define when timeout, spawn failure or lost
  observation is evidence versus infrastructure failure.
- Corrected the security model: typed drivers enable enforceable policy but are
  not themselves a sandbox. A future untrusted mode needs typed drivers,
  policy/allowlists and OS isolation.
- Distinguished the stdout invocation outcome from the persisted run report;
  invalid input can have the former without creating the latter.
- Scoped output guarantees to observable outcomes and available physical
  channels. probatum attempts exactly one schema-valid document but cannot
  defeat disk-full, permission failures or a closed stdout pipe.
- Refined teardown: the explicit path performs and reports controlled cleanup;
  `Drop` remains a non-panicking, best-effort last line of defense.

### 2026-07-14 — Pivot: "I have nothing to prove"

- The owner rejected the proof framing bluntly: no promises, no oracles — just
  "one line, one config file, run my checks, surface only what matters". The
  proof vocabulary was overhead, not value.
- Reframed as: Makefile/Taskfile-like, test-oriented, with embedded features
  (curl, grep, process supervision built in; the user declares the pass/fail
  rules).
- Settled through discussion: source × flat-rules model (`run`/`get`/`log` ×
  `expect`/`contains`/`absent`); features not plugins; humble noise filter
  with zero-config crash defaults; external logs read from run start;
  pre-existing log = dirty environment; detection over destruction (never
  purge what probatum doesn't own — setup is the user's own first check);
  no template layer (an example config *is* the template); sequential
  execution.
- A concrete example config format was drafted and approved by the owner.
- Working-tree note: an unsolicited code rewrite to the flat syntax exists,
  compiled and verified end-to-end, **uncommitted and paused** at the owner's
  request ("we're discussing — don't code"). It predates the `log:` source,
  the run-start window and dirty-env detection; reconcile or discard it when
  implementation resumes.
- Next, when the owner says go: reconcile the rewrite with this design, add
  `log:` + run-start window + dirty-env detection, then the teardown (RAII)
  core that makes the ownership guarantee true.

### 2026-07-14 — Contract frozen, post-pivot v1 implemented

- The owner's refinements were reviewed and accepted; one of them corrected
  the assistant twice (OR-is-not-two-checks; typed drivers ≠ sandbox earlier).
- Two remaining defaults settled by the owner: `get` without `expect` = any
  2xx; `log` without rules = config error.
- Implemented and verified end-to-end (12 scenarios, offline):
  sources `run`/service/`get`/`log`; rules `expect`/`contains`/`absent`/`allow`;
  run-start offset window with inode+size rotation/truncation detection;
  dirty-environment refusal (ready URL answering pre-start); stop-on-first-
  failure with skipped display; verdict pass/fail/couldn't-run and exit codes
  0/1/2; strict rejection of unknown keys, rule-less `log`, and `allow` on a
  plain run.
- Demo fixed: `segment-0004.json` is now shipped (the repo previously shipped
  the demo already broken while the README said to break it by hand);
  `proofs/` renamed to `checks/`; README rewritten to the post-pivot product.
- Known deviation flagged for owner review: empty log window with
  `absent`-only rules = explicit pass "(0 new lines)", not couldn't-run.
- Still open, next: the panic-safe teardown guard (ownership on probatum's own
  crash paths), `probatum init`, product tagline.

### 2026-07-14 — Ownership on every exit path + `probatum init`

- Owner confirmed: if probatum itself dies, everything it started must be
  killed. Implemented as a lock-free process-group registry (`src/own.rs`,
  async-signal-safe atomics): children register at spawn; the registry is
  swept with SIGKILL on every exit path — normal end (explicit teardown),
  panic unwinding (`Drop` guard, kill-only, non-panicking per the frozen
  design), SIGINT and SIGTERM (signal handler, then `_exit(130)`).
- Plain `run:` commands now also get their own process group: a command that
  leaks background children gets swept at run end too.
- Verified: panic mid-run (exit 101), SIGINT and SIGTERM mid-run (exit 130) —
  port clean and no leftover processes in all three cases. A first "leak"
  finding was a false positive: `pgrep -f` matched the test harness's own
  command line.
- Ceiling stated honestly: SIGKILL of probatum itself remains uncoverable (no
  handler, no unwinding); that path leaks until a wrapper/supervisor exists.
- `probatum init` ships: writes a commented `checks.yaml` example, refuses to
  overwrite an existing one. First generated example had a YAML parse bug
  (unquoted `:` in a scalar) — caught by running the generated file, fixed.

### 2026-07-14 — Dogfooding: probatum checks probatum

- A root `checks.yaml` now runs probatum on itself: build, clippy, the demo
  end-to-end (service + HTTP + external log), and two inverted negative
  scenarios asserting exit code exactly 1 ("probatum MUST report this as a
  caught failure; exit 2 would mean couldn't-run").
- One mock, several failure stories via env switches on `demo-app/app.py`
  instead of several mock apps: `WAL_DIR` (boot crash), `DEGRADE=1` (ready,
  then logs ERROR), `LOG_FILE` (writes an external log for `log:` checks).
- **The dogfooding caught a real bug on its first run**: the degraded mock
  logged ERROR after readiness and probatum said "all passed" — the service
  default filter only had crash-class markers (panic/FATAL/traceback) while
  the frozen contract says "panic, traceback, FATAL, ERROR out of the box".
  Doc/code gap fixed (ERROR/error: added to the service default markers).
- Side validation: the three nested runs share port 8087 sequentially, so any
  teardown leak would trip the dirty-env refusal on the next run — ownership
  is implicitly re-proven on every dogfooding run.
- This replaces the pre-pivot PROOF-001/002/003 idea in the product's own
  language: the acceptance tests are a probatum config.

### 2026-07-14 — Naming: probatum.yaml / .probatum/

- Owner rejected "checks.yaml" as too generic; chose the tool-name convention:
  `probatum.yaml` (default config, found by a bare `probatum run`),
  `.probatum/` for secondary configs (committed: dev-check, broken-check,
  degraded-check) and `.probatum/runs/` for evidence (ignored).
- `probatum init` now writes `probatum.yaml`; a bare `probatum run` without a
  config gives a helpful error pointing at init.

### 2026-07-15 — Boundary with cidx settled

- Reviewed the owner's cidx project against probatum: no structural duplicate
  (tools-against-source vs behavior-of-running-system), one honest overlap
  strip (any command runs in either).
- Owner's framing accepted: custom cidx stages could reach the same result,
  but probatum concentrates those tests into one more powerful, more
  expressive runner — both tools are good, each keeps its lane.
- Decision: probatum belongs to the **test phase** of the DevOps loop —
  `cidx run test` includes it (preset alongside e.g. go-test), no separate
  smoke phase. Boundary and both-ways guardrails recorded above.
- Deferred packaging item: static binary or small image for cidx's
  containerized presets.
- Follow-up question settled: **separate tools, no merge into cidx**. cidx
  orchestrates external tools by design; probatum joins the roster as one of
  them. Inner loop = `probatum run` direct; outer loop = `cidx run test` via
  preset; both read the same `probatum.yaml` (one source of truth).

### 2026-07-14 — Post-pivot semantic cleanup

- Confirmed that the pivot is a genuine product improvement: the value is one
  config, owned execution and aggressively useful output, not proof ceremony.
- Corrected the proposed OR workaround: two checks are an AND, not an OR.
  General boolean logic stays out; small list-valued rules remain undecided.
- Corrected external-log cleanliness: a pre-existing file is normal and can be
  observed from its initial identity/offset. Conflict or ambiguous
  replacement/truncation, not existence alone, causes `couldn't-run`.
- Identified source-specific zero-config defaults as part of the public config
  contract; their exact semantics still need a decision.
- Accepted that checks may mutate state. The general model is an operation plus
  rules over its observable result, not a pure assertion.
- Recognized that the historical proof slogan no longer describes the primary
  product surface. A direct check-oriented tagline remains to be chosen.
- The discussion file now contains two architectures. Historical sections must
  remain clearly marked and should eventually move to an archive so future
  context retrieval does not confuse superseded design with current direction.
