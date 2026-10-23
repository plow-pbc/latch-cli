# AGENTS.md

Lightweight guidance for AI coding agents working in this repository.

The goal is strong engineering judgment with low ceremony.

## Complexity Debt

- Complexity debt is worse than a product that breaks for one user. Do not fix an edge case in a way that adds complexity.
- Heuristic accretion is the canonical failure mode: one keyword exception, then another, then a special case. Avoid it absolutely.
- Complexity debt is doubly harmful: it makes the core solution appear more capable than it is, masking whether the real approach actually works, and it makes the codebase slower and more fragile to change.
- For open-ended interpretation or routing problems, prefer a generalizable semantic/model-based approach over ad hoc heuristic casework.

## Scope

- `latch/` is the latch CLI, one Rust crate (and the root of its Cargo workspace): a peer that lets an agent reach the files, programs, Google accounts and browser on a machine its owner trusts, over Plow's directory.
- `latch/tests/` holds the integration tests, their fixtures, and `browser-site/`, the local site the browser tests drive.
- `latch/*.md` documents each area: `BROWSER.md`, `VAULT.md`, `GOOGLE.md`, `GOOGLE-AGENT-GUIDE.md`, `DOCKER.md`, `MACOS.md`, `DEPENDENCIES.md`. `latch/SKILL.md` is the agents' guide `latch skill` prints.
- Plow's side of latch (the peer directory API in `api/plow/latch.py`, its model and migration) lives in [`plow-pbc/plow`](https://github.com/plow-pbc/plow), not here. latch reaches it over HTTP.

## Read First

Before making meaningful changes, read:

1. `README.md`
2. The `latch/*.md` doc for the area you are changing

## Dev State

- All dev and test state lives under the gitignored `.dev/` at the repo root, passed with `--state-dir`. Tests make their throwaway folders in `.dev/tests/`.
- `just test browser` (and the gate, `just test`) installs the pinned browser engines into `.dev/cft-state` first, where the browser tests look for them.
- `just build` builds the checkout for hand testing into `.dev/bin/latch`, with `.dev/state` and the local Plow stack (see Local Plow For Development) baked in as its defaults, so it never touches `~/.latch`, an installed latch or production Plow (`just build api=https://api.plow.co` targets it, for the chef's release smoke only). `just dev-login` logs `.dev/state` in to the local stack; `just gatekeeper` asks the real decision model on that login.
- A binary built with `LATCH_BUILD_STATE_DIR` set has that state folder as its default. Running hosts and clients may depend on it: do not move, copy, or delete another peer's state folder, and do not stop or restart a running `latch serve`, unless asked.
- Never print a token, key, or vault value. Credentials stay in their files with mode 600.

## Local Plow For Development

Dev and test flows that need Plow use a local Plow dev stack, never production:

1. In a plow checkout, start its stack: `just api up`, then `cd dtu/linq && just sync && cd ../.. && just dtu linq up` (the Linq twin, which stands in for the phone `just dev-login` texts from). Plow derives each checkout's ports from its `PLOW_VARIANT` into that checkout's `.plow-dev-env`.
2. Here, `cp .env.local.example .env.local` and set `LATCH_DEV_PLOW_CHECKOUT` to that checkout; its `.plow-dev-env` gives the API and twin addresses. `LATCH_DEV_PLOW_API` and `LATCH_DEV_PLOW_TWIN` override either. With none set, the plow checkout `main`'s ports are used (API `127.0.0.1:18934`, twin `127.0.0.1:18935`).
3. `just dev-login`, then `just build` or `just gatekeeper`. `dev-login` checks the stack is reachable first and says which variable gave the address it could not reach. Every checkout logs in to the same local account, so its machine is named `dev-<checkout folder>` (`--name` to override).

Use the published `127.0.0.1` ports, not `*.orb.local`: macOS keeps an unapproved latch off OrbStack's network. `just` loads `.env.local`; `scripts/dev_plow.py` reads these variables, and the latch binary never does.

## Core Standard

Write code that is:

- correct
- minimal
- obvious
- consistent with the local codebase
- typed where the shape matters
- easy to delete, extend, or debug

Prefer the simplest implementation that clearly solves the real problem.

## Product Stage

This product is still early. The main goal is to build the right core product, not to preserve every intermediate implementation.

- Prefer clean replacement over migration code.
- Prefer rebuild/rebootstrap over gradual cutover logic.
- Do not add dual-write paths, compatibility layers, fallback implementations, or "old path/new path" systems unless the user explicitly asks for them.
- Do not keep multiple paths that solve the same problem.
- It is acceptable to make breaking changes to local structure, derived state, or bootstrap flows when that yields a cleaner design.
- Do not spend time on edge-case hardening or operational complexity that does not help the core use case.
- Design so the core model can extend to adjacent use cases later, but do not prebuild for special cases.
- Do not prematurely optimize. Prefer the simpler solution that generalizes across diverse inputs, even at higher token, CPU, memory, disk, or resource marginal cost.

Important:

- Prefer deleting and rebuilding over writing migration code.
- Prefer a single robust primary path over a primary path plus backup path.
- Prefer generalizing the core design over adding one-off branches for narrow cases.
- Do not fix open-ended understanding problems with growing lookup tables or keyword-based special cases.
- Do not perform destructive actions against real external systems or user data unless the user explicitly asks.

## How To Work

- Act autonomously by default. Read files, inspect state, and run commands without asking for permission.
- Ask questions only when ambiguity materially changes architecture, business logic, or user intent.
- Search before building. Reuse or extend existing code when it is already close to the needed behavior.
- Match the grain of the codebase. Prefer local consistency over introducing a new pattern you happen to like better.
- Touch the smallest surface area that solves the problem.

## Keep It Simple

- Prefer one obvious execution path.
- Do not add speculative abstractions, fallback layers, retries, or knobs unless there is a real requirement.
- Let internal errors surface. Catch only specific errors you can handle meaningfully.
- Do not build migrations, compatibility shims, phased rollouts, or temporary bridging layers.
- When the problem is semantic understanding or routing, prefer a generalizable inference step over brittle heuristic casework.
- Avoid single-use helpers unless they materially improve readability.
- Remove dead code, commented-out code, stale TODOs, and debug leftovers.

## Types And Data Shape

- Prefer explicit shaped data (plain structs) over loose `serde_json::Value` when practical.
- If a payload shape matters across module boundaries, model it clearly.
- Avoid widening types just to silence checks.

## Test Philosophy

**Never skip failing tests. Fix the problem instead.**

A skipped test is a lie — it tells you the suite is green when it is not. Skipping accumulates quietly until the suite is meaningless.

- **Do not skip tests because credentials or keys are missing.** Get them. If you don't have them, ask the user — do not `#[ignore]` your way past the failure.
- **Do not skip tests because the environment is not set up.** Set up the environment: install the engines (`just test browser` does), start Docker for `just test --linux`. The test is telling you what needs to be true — make it true.
- **Do not comment out failing assertions.** That is the same as deleting the test but pretending you didn't.
- **Do not weaken assertions to make tests pass.** The fix is in the code, not in the assertion.

When you encounter a failing test:

1. Read the error. Understand what it expects and what actually happened.
2. Fix the root cause — missing setup, broken code, stale fixture, wrong environment.
3. Run the test again and confirm it passes for the right reason.

## Tests

- Test behavior and outcomes, not implementation details.
- Prefer tests that validate safety gates (sandbox, ownership, what reaches an agent or the audit) and regressions that matter.
- Avoid brittle assertions tied to incidental formatting, exact internal call structure, or mock choreography.
- Do not write tests for self-evident language or library behavior.
- Keep setup compact and readable. When changing behavior, update or add tests near the closest existing test module unless there is a clear reason to create a new one.

## Validation

The gate is fmt, clippy with `-D warnings`, and the tests, on macOS and on Linux. From the repo root:

```bash
just test             # the gate for this machine's OS
just test --linux     # the Linux gate, in the Linux test container (Docker)
just test core        # one suite: the quick loop
just test --only NAME # just the tests with NAME in their names, in whichever suites hold them
just test-list        # the suites: what each covers and needs, and where it runs
just test-plan ...    # what `just test ...` would run, without running it
```

A suite that cannot run where you asked fails and says why; nothing is skipped. CI runs these same commands. The Rust suites run on [cargo-nextest](https://nexte.st) (CI pins 0.9.148), with `latch/.config/nextest.toml`.

`just gatekeeper` runs the checks that ask the decision model for real, through Plow on the login of `.dev/state` (not tests).

Rules:

- Run both gates before presenting changed code. If you could run only one, say which.
- Do not claim "tests pass" for a gate you did not run.
- If you only changed docs, say that tests were not run.

## Definition Of Done

A change is done when:

- it solves the actual problem with minimal code
- the implementation is easy to follow
- both gates pass
- no unnecessary abstractions or debug artifacts remain

## Communication

- Be concise and concrete.
- State what changed, what you verified, and any real risks or open questions.
- Do not pad responses with process theater.
