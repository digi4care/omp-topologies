# Rust Development Guide

> This is a reusable planning and engineering guideline for Rust work. It is not a project-specific recipe. When given this file, an AI must inspect the target project, research the practices and tools that apply to the requested work, and adapt its plan to the evidence. Do not copy this repository's tooling, dependencies, or architecture into another project without a reason.

## How to use this guide

Apply the principles below to the task at hand, not mechanically. The user’s requirements, repository instructions, supported platforms, and existing contracts take precedence over generic defaults. If they conflict, identify the conflict and explain the chosen resolution instead of silently ignoring either one.

During planning, do not jump straight from the request to a generic implementation recipe. Discover the project context, research important unknowns, and make the results visible in the plan. If approval is required, stop at the approved planning boundary before implementing.

## 1. Discover the project and task

Before proposing changes:

- Restate the intended user-visible outcome and acceptance conditions. Identify non-goals and compatibility requirements.
- Inspect the relevant source, tests, documentation, configuration, build scripts, CI workflows, and recent local conventions. Use repository-aware symbol/reference tools where available; do not infer APIs from filenames alone.
- Identify the Rust edition, minimum supported Rust version (MSRV), pinned toolchain, target platforms, crate layout, dependency policy, and existing quality gates. Do not assume they match current Rust defaults.
- Determine whether the work affects a CLI, library, service, embedded target, build tool, or multiple components. Their runtime and compatibility needs differ.
- Record constraints that change the design: data volume, concurrency, latency, memory, disk growth, uptime, security boundaries, offline behavior, or migration requirements.
- Treat the working tree and parallel work as shared user state. Preserve unrelated changes. Use an isolated worktree or branch when required by the user, repository policy, or concurrent work.

Do not invent repository files, commands, supported targets, or behavior. If a key fact cannot be established from the repository or available tools, label it as an assumption and include a way to verify it.

## 2. Research what the task actually needs

Research is targeted, version-aware, and proportional to the risk. Do not research every Rust topic for every task, and do not present remembered API details as current without checking them when they matter.

For each material unknown, consult the most authoritative applicable sources first:

1. The project’s own source, tests, design notes, release policy, and locked dependency versions.
2. Official Rust documentation relevant to the question: the [Rust Book](https://doc.rust-lang.org/book/), [Rust Reference](https://doc.rust-lang.org/reference/), [standard library documentation](https://doc.rust-lang.org/std/), and [Cargo documentation](https://doc.rust-lang.org/cargo/).
3. Version-matched documentation and release notes for candidate crates, including their MSRV, supported targets, feature flags, maintenance status, and license.
4. Security advisories or supply-chain information when dependencies, parsing, serialization, networking, credentials, or updates create a relevant risk.

Use the documentation for the versions and targets the project supports, not just the newest example found online. Check that proposed APIs exist in the selected versions. Prefer primary sources; include links and the versions or dates that informed a consequential choice.

The plan should summarize research rather than dump it. For each important choice, state:

- the requirement or uncertainty;
- the evidence and source consulted;
- the chosen approach and why it fits this project;
- alternatives rejected and the material tradeoff;
- how the choice will be verified.

Research only far enough to make a sound decision. If evidence is inconclusive, state what small experiment or compatibility spike will resolve it before committing to a dependency or architecture.

## 3. Make a project-specific plan

A useful plan is a sequence of complete, reviewable changes, not a list of Rust buzzwords. Include:

- affected components, interfaces, data formats, and callers;
- behavior to preserve, behavior to change, and migration or rollback needs;
- the smallest suitable architecture and dependency set;
- error cases, security boundaries, and resource limits;
- test scenarios tied to acceptance conditions;
- applicable formatting, lint, test, build, documentation, and release gates;
- risks, assumptions, and unresolved decisions.

Reuse the project’s established patterns when they are sound. Do not add a framework, abstraction layer, dependency, configuration surface, or compatibility shim merely because it is familiar. Prefer a direct, boring solution that fully covers the requested behavior.

Do not implement placeholders, no-ops, fake fallbacks, or silently narrowed behavior. If the scope cannot be completed as specified, describe the constraint and propose the smallest explicit decision needed.

## 4. Rust implementation principles

### Dependencies and project structure

- Prefer the standard library when it is sufficient; add a crate when it materially improves correctness, security, maintainability, or delivery speed.
- Evaluate candidate crates against the project’s MSRV, target platforms, maintenance, license, feature footprint, security posture, and actual required behavior. Keep dependencies focused; avoid duplicate crates that solve the same problem.
- Use Cargo conventions already present in the repository. Choose workspace structure only when multiple packages or shared build needs justify it.
- Decide deliberately whether a toolchain pin and committed `Cargo.lock` are appropriate. They are common for applications and reproducible CI, but are not universal requirements for every published library.
- Make feature flags and optional dependencies explicit. Avoid enabling broad default features without a reason.

### Safety, errors, and boundaries

- Prefer safe Rust. Forbid unsafe code at crate level where practical. If unsafe is necessary, isolate it, document its safety invariants, minimize its surface, and test the boundary.
- Validate untrusted inputs at system boundaries. Handle malformed data, missing files, permissions, interrupted operations, and unsupported platforms deliberately.
- Return actionable errors with enough context to diagnose the failure, without exposing secrets. Use typed errors where they improve caller handling; do not create an elaborate error framework for trivial code.
- Avoid panics for expected user, environment, I/O, or network failures. Use assertions for internal invariants, not input validation.
- Consider atomic writes, locking, rollback, and file permissions whenever persistent state can be partially updated or contains sensitive data.
- Keep secrets out of logs, fixtures, generated files, caches, and state registries unless the design explicitly requires secure secret storage.

### APIs and compatibility

For public APIs, CLIs, file formats, and network contracts, define compatibility and failure behavior before implementation. Check all callers and user-visible help/docs when changing an interface. Prefer explicit, predictable behavior over implicit discovery or broad side effects. Add deprecation or migration paths only when the project needs them; do not preserve dead interfaces without a compatibility requirement.

## 5. Data structures, DSA patterns, and Big-O

DSA patterns are reusable ways to recognize and solve recurring problems, not templates to force onto every code change. Consider a pattern when the input and operations match it; use the simplest correct approach when they do not.

Examples of useful patterns include hash-based indexing for repeated membership/lookups, sorting plus two pointers for ordered pair/range scans, sliding windows for contiguous ranges, queues for breadth-first traversal, heaps for repeated min/max selection, and maps/sets for deduplication. Verify assumptions such as ordering, mutability, duplicate handling, and input size before choosing one.

For every non-trivial algorithm or data-processing path in the plan, state expected time and space complexity in Big-O notation, including relevant variables. Explain why the selected data structure fits the workload. Avoid avoidable quadratic scans, repeated sorting, redundant parsing, cloning, and allocations in compiled code. Do not add Big-O comments to trivial statements or claim asymptotic improvements without evidence.

Complexity is not the only performance measure. Consider constants, allocations, cache behavior, I/O, contention, startup cost, and data shape when they matter. Optimize measured or clearly bounded hot paths; avoid speculative complexity. If performance is a requirement, define the workload and a measurable baseline before selecting an optimization.

## 6. Performance over time

A program that is fast in a short test can degrade as its state, cache, queue, registry, files, or number of repeated operations grows. Translate “must stay fast over hours, days, or weeks” into observable long-run requirements for the actual program type.

For long-running processes, plan to observe relevant measures such as latency/throughput, memory, file descriptors, task/thread counts, queue depth, cache size, lock contention, and persistent storage. Identify which resources must remain bounded, how stale state is reclaimed, and how shutdown, restart, and partial failure behave.

For repeatedly invoked tools and CLIs, test repeated commands and accumulated state: many profiles or records, large manifests, backups, caches, logs, and interrupted/retried operations. Check that idempotent operations do not grow state unnecessarily and that cleanup/retention policies are explicit and safe.

For libraries or batch processing, use representative small, typical, and worst-case input sizes; account for allocation volume and output size as well as elapsed time.

Choose tests that model accumulated use. A deterministic scale test or a bounded soak test can exercise many repeated cycles and growing data; a scheduled or opt-in longer run can explore duration-dependent behavior. A test that simulates weeks of accumulated state does not need to literally run for weeks, but the model and its limits must be stated. Avoid flaky wall-clock assertions in ordinary unit tests. Keep benchmarks separate from correctness tests, use stable inputs and a documented environment, and compare against an explicit baseline when performance regression detection is required.

## 7. Test behavior and adversarial edges

Tests should catch plausible user-visible failures and protect contracts. Choose test types based on risk; not every project needs every category.

- Unit tests for focused logic and edge cases.
- Integration or contract tests for CLI/API behavior, persistence, and component boundaries.
- Property-based or fuzz tests for parsers, serializers, state machines, and complex input spaces when they add meaningful coverage.
- Scale, soak, or benchmark tests when workload size or elapsed use affects the requirements.
- Platform-specific tests when OS, filesystem, architecture, or compiler behavior differs.

Tests should be deterministic, isolated, and safe to run with the full suite. Include malformed, boundary, duplicate, interrupted, permission-denied, and adversarial inputs when relevant. Assert externally observable results and invariants, not private implementation details, source text, wiring, incidental defaults, or mere absence of a panic. Never weaken a test or suppress a real failure to make a change pass.

Use temporary directories and isolated state for filesystem tests. Do not touch a developer’s active configuration or credentials. Clean up test resources reliably. For concurrent code, test races and cancellation boundaries with controlled synchronization rather than timing guesses.

## 8. Strict, reproducible quality gates

Plan a small set of authoritative gates appropriate to the project and enforce them consistently in local development and CI. Common Rust starting points are:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Use `--locked`, workspace/package selectors, target triples, doctests, build checks, or feature matrices when the project’s lockfile and support policy require them. Research and document the exact commands; do not copy this list blindly or claim a platform is tested when CI does not test it.

- Keep local and CI validation aligned: prefer one checked-in script or task entry point invoked by both.
- Make required checks blocking for the relevant merge/release path; do not silently mark failures as allowed or skip supported targets.
- Treat warnings as errors where the project can support that policy. If an exception is necessary, scope it narrowly, document why, and track its removal where appropriate.
- Add dependency, license, documentation, coverage, or security audit gates only when justified by project risk and maintenance cost.
- Report the exact commands and outcomes actually observed. Never claim a check passed if it was not run.

## 9. Definition of a complete change

A change is complete when it meets the requested behavior end to end, preserves unrelated user work, has focused tests for meaningful risks, passes the applicable strict gates, and updates the documentation or migration notes affected by the behavior. Remove code, comments, aliases, and documentation made obsolete by the change when compatibility does not require them.

In the delivery summary, state:

1. What changed and which files or interfaces were affected.
2. Important design/dependency choices and why they fit this project.
3. Big-O and performance considerations for non-trivial paths.
4. Tests and gates actually run, including any failures or skips.
5. Remaining assumptions, risks, and unsupported targets.

## Planning response template

Use or adapt this outline when asked to plan Rust work. Omit sections that genuinely do not apply and explain material omissions.

```markdown
# Goal and acceptance criteria
- User-visible outcome:
- In scope / out of scope:
- Compatibility and safety constraints:

# Repository findings
- Relevant components, existing conventions, tests, and CI:
- Rust edition, MSRV/toolchain, targets, and dependency policy:
- Unknowns or assumptions:

# Research and decisions
| Question | Evidence/source and version | Decision | Tradeoff / verification |
|---|---|---|---|

# Design and implementation slices
1. ...

# Complexity and performance
- Important operation: expected time/space complexity and workload
- Long-run resource/state behavior and how it will be tested

# Validation
- Focused tests and adversarial cases:
- Formatting/lint/build/test/CI gates:
- Platform and long-run coverage:

# Risks and open decisions
- ...
```

## Starting points for research

These are entry points, not a mandatory checklist. Select the sources relevant to the task and verify the versions in use:

- [The Rust Programming Language](https://doc.rust-lang.org/book/)
- [The Rust Reference](https://doc.rust-lang.org/reference/)
- [Rust standard library](https://doc.rust-lang.org/std/)
- [Cargo documentation](https://doc.rust-lang.org/cargo/)
- [Clippy lint list](https://doc.rust-lang.org/clippy/lints.html)
- [Rust crate search](https://crates.io/)
- [RustSec Advisory Database](https://rustsec.org/) for relevant dependency security review
