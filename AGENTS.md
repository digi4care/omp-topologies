# Repository Guidelines

## Project Overview

`omp-profile` is a Rust CLI for discovering and installing versioned topology overlays into OMP agent profiles. Sources may be standalone package roots (`topology.yml` plus `agent/`) or external legacy catalogs with `topologies/<slug>/topology.yml`. This repository bundles no profile catalog; `list` and `install` require an explicit `--source`. The tool supports listing, install/dry-run, status, update, removal, profile discovery, and signed self-update. There is no server component.

## Architecture & Data Flow

CLI parsing and orchestration in `src/main.rs` resolve a topology source (`src/source.rs`), validate its manifest and overlay (`src/topology.rs`), resolve the target profile (`src/profiles.rs`), then plan and apply filesystem changes (`src/install.rs` / `src/remove.rs`). Ownership manifests in `src/state.rs` support safe removal; `src/registry.rs` stores install-ID/source metadata. `src/update.rs` and `src/status.rs` use this state for refresh and freshness checks.

Important invariants: installation plans before mutation and rejects collisions; apply rechecks live files and rolls back failures. Removal preserves user edits. The installer rejects symlinked components in managed paths and uses descriptor-anchored filesystem operations in `src/anchored.rs`, the only scoped `unsafe` boundary. External processes are invoked with argv, not shell strings. Keep JSON output to one document on stdout; errors go to stderr.

## Key Directories

- `src/` — CLI and installer implementation; `src/anchored.rs` implements anchored filesystem operations, with tests in `src/anchored/tests.rs`.
- `tests/` — end-to-end CLI, installer contract, and regression suites.
- The standalone profile repositories are separate Git repositories outside this installer checkout; their package roots are not bundled here.
- `topologies/<slug>/` — legacy catalog layout supported only when present in an explicitly selected source; this repository has no bundled catalog.
- `docs/` — project-wide installer guidance and topology README template. Local profile documents are archived outside Git and are neither package source nor publishable content.
- `.github/workflows/` — Rust gates and release automation.

## Development Commands

Use the pinned Rust toolchain and lockfile. The canonical gate is:

```sh
scripts/rust-gates.sh
```

It runs formatting, Clippy with warnings denied, and the full test suite in the pinned Docker image. For the warm-cache development container:

```sh
docker compose up -d --build
docker compose exec rust ./scripts/rust-gates.sh
docker compose down
```

Focused installer tests: `cargo test --locked --test installer`. Run locally with `cargo run --locked -- list --source <repo-or-path>`; dry-run an install with `cargo run --locked -- install -t <slug> --source <repo-or-path> --dry-run --agent-dir "$(mktemp -d)/agent"` (target directory basename must be `agent`). Release builds use `cargo build --release --locked --target <target>`.

## Code Conventions & Common Patterns

- Follow existing Rust module patterns; modules document guarantees and failure behavior. Keep changes small and avoid dependencies or abstractions without a concrete need.
- Use the shared `CliError` and path-aware I/O error helpers; expected failures should be returned, not panicked.
- Preserve deterministic ordering (`BTreeMap`/`BTreeSet`), atomic writes, locking, rollback, and owner-only state permissions where applicable.
- Validate untrusted input at boundaries. Fail closed on malformed state, symlinks, and signature/checksum failures.
- Keep unsafe code confined to `src/anchored.rs`, with safety comments. The crate denies unsafe code by default.
- Command handlers capture output for human/JSON rendering rather than printing ad hoc. Parse SemVer with the existing parser; do not replace it with string splitting.
- Rust tests use descriptive snake_case names, standard `#[test]` assertions, isolated temporary directories, and controlled HOME/XDG environment. Follow local test helpers rather than adding a test dependency.

## Important Files

- `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` — crate metadata, locked dependencies, and Rust 1.100.0 toolchain (edition 2024).
- `README.md` — user-facing features, architecture, and CLI usage.
- `CONTRIBUTING.md`, `docs/rust-development-guide.md` — contribution workflow, verification, and Rust development guidance.
- `docs/installer-guide.md` — installer behavior and troubleshooting.
- `scripts/rust-gates.sh` — canonical local/CI quality gate.
- `scripts/install-release.sh`, `keys/omp-topology-release-public.pem` — signed bootstrap trust material; keep embedded and file keys synchronized when rotating.
- `compose.yaml`, `Dockerfile` — pinned development container and toolchain.

## Runtime/Tooling Preferences

Rust 1.100.0 is pinned in `rust-toolchain.toml`; use Cargo with `--locked` for checks and builds. Docker is the supported path for the canonical gate; Compose caches Cargo artifacts. This is a Unix-focused project (Linux and macOS release targets); Windows is not supported. Runtime integrations use `git` for remote sources and `omp` for profile resolution. Release installation requires POSIX `sh`, `curl`, `openssl`, `tar`, and platform checksum tools (`sha256sum` on Linux, `shasum` on macOS).

## Testing & QA

Tests use Rust's built-in harness: unit tests live beside modules and integration tests are `tests/cli.rs`, `tests/installer.rs`, and `tests/regressions.rs`. Run the focused installer contract suite for installer/topology-discovery changes, and `scripts/rust-gates.sh` for the full format, lint, and test gate. No enforced coverage percentage is configured. Keep filesystem tests deterministic and isolated; assert observable behavior, including failure and rollback cases. Report the exact checks run; CI jobs are path-filtered, so a skipped job does not verify its domain.

Topology payload changes also require a version bump in `topology.yml` and a dated entry in that topology's `CHANGELOG.md` in the same change. Topology versions are independent of the CLI crate version.

## Beads tracker for this repository

This repository uses the Rust `br` (beads_rust) tracker for `.beads`; do not use the Go `bd` CLI here. Use `br ready --json` for work discovery and pass an explicit `--actor` when claiming issues. Use `bv --robot-*` for noninteractive triage; never run bare `bv` in automation. This repository-specific policy overrides generic examples in the generated Beads block below: do not sync, stage, commit, or push unless the task explicitly requests it. Run `br sync --flush-only` only when explicitly preparing JSONL for a handoff; never auto-sync at session end.



<!-- bv-agent-instructions-v8 -->

---

## Beads Workflow Integration

This project uses a Beads tracker—either the Go `bd` CLI or the Rust `br` CLI—for issue tracking, plus [beads_viewer](https://github.com/Dicklesworthstone/beads_viewer) (`bv`) for graph-aware triage. Issues are stored in `.beads/`. `bv` auto-discovers supported JSONL exports, including `.beads/issues.jsonl` and legacy `.beads/beads.jsonl`.

**Choose the tracker CLI from this repository's instructions and configuration.** Use `bd` commands in a Go Beads workspace and `br` commands in a beads_rust workspace. Do not run both trackers against the same workspace or infer the tracker solely from the JSONL filename.

### Using bv as an AI sidecar

bv is a graph-aware triage engine for Beads projects. Instead of parsing .beads/issues.jsonl / .beads/beads.jsonl directly or hallucinating graph traversal, use robot flags for deterministic, dependency-aware outputs with precomputed metrics (PageRank, betweenness, critical path, cycles, HITS, eigenvector, k-core).

**Scope boundary:** bv handles *what to work on* (triage, priority, planning). The selected tracker CLI (`bd` or `br`) handles creating, claiming, modifying, and closing beads.

**CRITICAL: Use ONLY --robot-* flags. Bare bv launches an interactive TUI that blocks your session.**

#### The Workflow: Start With Triage

**`bv --robot-triage` is your single entry point.** Its `triage` object contains:
- `quick_ref`: at-a-glance counts + top 3 picks
- `recommendations`: ranked actionable items with scores, reasons, unblock info
- `quick_wins`: low-effort high-impact items
- `blockers_to_clear`: items that unblock the most downstream work
- `project_health`: status/type/priority distributions, graph metrics
- `commands`: copy-paste shell commands for next steps

```bash
bv --robot-triage        # THE MEGA-COMMAND: start here
bv --robot-next          # Minimal: just the single top pick + claim command

# TOON output (--format toon): a compact tabular encoding. Measured on this
# repository it is 7% smaller than JSON for --robot-graph but 9-15% LARGER for
# nested payloads (--robot-triage, --robot-plan, --robot-insights,
# --robot-label-health); use --stats to see both sizes before adopting it.
# TOON encoding shells out to the tru binary. With no encoder installed,
# --format toon prints a fallback warning, emits JSON with output_format "json",
# and --stats prints no sizes at all.
bv --robot-graph --format toon
bv --robot-triage --format toon --stats
```

By default, `triage.recommendations` and `triage.quick_ref.top_picks` surface ready, actionable leaf work (pass `--robot-include-inactive` to inspect blocked, deferred, parked, or umbrella epic items; assigned `in_progress` items remain marked `claimable=false`). A suggested action records its original local ID, working directory, and tracker route. Use that route rather than a namespaced display ID or an unrelated current directory. Inspect current tracker state before execution: analysis does not reserve work or guarantee that a later claim succeeds.

#### Other bv Commands

| Command | Returns |
|---------|---------|
| `--robot-plan` | Parallel execution tracks with unblocks lists |
| `--robot-priority` | Priority misalignment detection with confidence |
| `--robot-insights` | Full metrics: PageRank, betweenness, HITS, eigenvector, critical path, cycles, k-core |
| `--robot-alerts` | Stale issues, blocking cascades, priority mismatches |
| `--check-drift --robot-drift` | Baseline drift evaluation + live issue health alerts |
| `--robot-suggest` | Hygiene: duplicates, missing deps, label suggestions, cycle breaks |
| `--robot-diff --diff-since <ref>` | Changes since ref: new/closed/modified issues |
| `--bead-history <id>` | Single-bead lifecycle events and correlated git commits |
| `--robot-graph [--graph-format=json\|dot\|mermaid]` | Dependency graph export |

Robot analysis commands default to JSON; `--format toon` selects TOON, and `--robot-help` defaults to text. In JSON mode, `--graph-format=dot` or `mermaid` puts diagram text in the `graph` field (`bv --robot-graph --graph-format=dot | jq -r .graph`).

#### Scoping & Filtering

```bash
bv --robot-plan --label backend              # Scope to label's subgraph
bv --robot-insights --as-of HEAD~30          # Historical point-in-time
bv --recipe actionable --robot-plan          # Pre-filter: ready to work (no blockers)
bv --recipe high-impact --robot-triage       # Pre-filter: top PageRank scores
```

### Tracker Commands for Issue Management

Use exactly one command family, matching the tracker configured for the repository.

#### Rust beads_rust (`br`)

Use `br` 0.6.0 or newer when executing saved claim commands. It rechecks
deferred status and future `defer_until` values when the claim runs, so a
recommendation captured before a deferral cannot bypass it. This requirement
applies to executing tracker claims.

```bash
br ready --json                       # Show issues ready to work (no blockers)
br list --status=open --json          # All open issues
br show <id> --json                   # Full issue details with dependencies
br create --title="..." --type=task --priority=2 --json
br update <id> --claim --json         # Claim for the current actor and start work
br close <id> --reason="Completed" --json
br close <id1> <id2> --reason="Completed" --json
br sync --flush-only                  # Export DB to JSONL after Beads mutations
```

#### Go Beads (`bd`)

```bash
bd ready --json                       # Show issues ready to work
bd show <id> --json                   # Full issue details
bd create "..." -t task -p 2 --json
bd update <id> --claim --json         # Atomically claim work
bd close <id> --json
bd dep add <issue> <depends-on>
bd export -o .beads/issues.jsonl        # Refresh the compatibility export read by bv
```

### Workflow Pattern

1. **Triage**: Run `bv --robot-triage` to find the highest-impact actionable work
2. **Verify**: Check the selected tracker's `show`/`ready` output before claiming
3. **Claim**: Use `br update <id> --claim --json` or `bd update <id> --claim --json`
4. **Work**: Implement the task
5. **Complete**: Use the selected tracker's `close` command
6. **Refresh for bv**: Run `br sync --flush-only` or the `bd export` command above so the JSONL export is current

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready --json` and `bd ready --json` show unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers 0-4, not words)
- **Types**: task, bug, feature, epic, chore, docs, question
- **Blocking**: Use `br dep add <issue> <depends-on>` or `bd dep add <issue> <depends-on>` to add dependencies

### Git Policy

Tracker commands do not grant permission to commit or push application code. Follow this repository's own git and tracker instructions before staging, committing, syncing, or pushing. If the repository says "commit only when asked," that rule overrides any generic workflow advice.

<!-- end-bv-agent-instructions -->
