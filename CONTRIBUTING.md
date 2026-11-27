# Contributing

Contributions should be focused, evidence-backed, and easy to review. Keep changes within the agreed scope; do not combine unrelated fixes or silently broaden a topology's behavior.

## Before changing files

- Read the relevant topology, its `topology.yml`, agent rules, and charter skills. Reuse established conventions.
- For behavior or policy changes, identify the user-visible problem and the acceptance criteria the change must satisfy. Ask the maintainer when scope or a material decision is unclear; do not resolve it by assumption.
- Keep topology-specific changes in the relevant standalone profile repository or external legacy package. For profile package creation, use the single authoring kit at [`omp-profile-example/README.md`](omp-profile-example/README.md).
- Do not edit generated or unrelated files. Preserve existing user changes and report any unrelated workspace state you encounter.

## Adding or changing a topology package

Use [`omp-profile-example/README.md`](omp-profile-example/README.md) as the single entry point for the package specification, copyable example, README template, and validation instructions. Keep profile-specific design documents in the owning profile repository's `docs/` directory.


## Implementation and verification

- Make the smallest complete change that satisfies the agreed scope. Avoid new dependencies and abstractions unless required.
- Choose checks that exercise the changed behavior. The installer contract
  suite lives in `tests/installer.rs`; the focused run for installer and
  topology-discovery changes is:

  ```sh
  cargo test --locked --test installer
  ```

  The full local gate is `cargo test --locked` plus `scripts/rust-gates.sh`
  (the same entry point the Rust gates workflow runs). For repeated Docker
  runs, `compose.yaml` keeps a persistent toolchain container with warm Cargo
  caches: `docker compose up -d --build`, then
  `docker compose exec rust ./scripts/rust-gates.sh`. `docker compose down`
  preserves the caches; `docker compose down -v` deletes them.

- CI validates changes in the repository that owns the package. Standalone
  profile-repository payload changes run that repository's validation workflow;
  keep its manifest, `agent/` payload, and validation inputs in scope.
- This installer repository's `.github/workflows/rust-gates.yml` runs
  `scripts/rust-gates.sh` only when a PR changes Rust inputs (Rust sources,
  Cargo manifests or lockfile, the toolchain file, `.cargo/`, or the gate
  script/workflow itself). It does not validate external topology payloads.
  There is no bundled catalog workflow; validate legacy catalog changes using
  the applicable checks for the external source repository.

  CI never replaces local verification: run the applicable checks locally
  before opening the PR and report them in the PR description. A job that
  skipped its domain gates is not evidence for that domain.

- For topology or prompt changes, inspect the resulting files and compare them with the source topology or approved requirements as applicable. Do not claim tests, installation, or runtime behavior were verified unless those checks were actually run.
- In reports and documentation, distinguish verified results from implementation-reported claims, assumptions, pending work, and unverified criteria. A passing narrow check proves only the behavior it exercised.
- Update `CHANGELOG.md` for user-visible changes when appropriate. Do not change versions or release metadata unless the change requires it.

## Pull requests

Use one logical change per PR. Keep the title specific and use the repository's conventional style, for example `fix(installer): preserve config comments` or `docs(example): clarify verification status`.

Write the PR description for a reviewer who has not followed the discussion. Include:

1. **Why** — the problem or requirement and relevant context.
2. **Changes** — concrete behavior and important files; call out intentional non-goals.
3. **Verification** — exact commands or review performed and their observed results. Say `Not run` with the reason when checks were not run; never imply a check passed from historical evidence.
4. **Limitations / follow-up** — known gaps, unverified criteria, dependencies on another PR, or pending decisions.

Keep the description aligned with the actual PR diff. For stacked PRs, link the prerequisite and state the merge order. Do not include unrelated branch history as if it were part of the change.
For package versioning, changelog, and validation requirements, follow the profile authoring kit linked above.

## Review and merging

- Resolve review feedback with focused changes and update the PR description when scope or evidence changes.
- Do not bypass required checks or branch protection. If direct merge or push is blocked, report the exact required check and use the repository's PR workflow.
- Merge only after the required reviews and checks pass. Do not claim a change is merged to the remote default branch until the merge is visible there.

## Licensing and attribution

This repository is licensed under Apache-2.0; contributions are subject to the terms in `LICENSE`. Preserve existing copyright and `NOTICE` information. Credit third-party projects and authors accurately, with a source link, and distinguish inspiration from code copied or adapted under a license.
