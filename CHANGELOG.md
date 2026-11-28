# Changelog

All notable changes to this project are documented here.

## [Unreleased]

### Added

- Add read-only local host diagnostics with `doctor` and Bash, Fish, and Zsh
  completions that honor the CLI's valid option combinations.

## [0.3.0] - 2026-11-26

### Changed

- **Breaking:** install `--alias` now delegates real OMP shell-alias creation and requires `--profile`; use `--install-id` for the registry identity and lifecycle selection.
- Rename registry-facing JSON and human-readable output to `install_id` while retaining the existing on-disk registry schema.

### Fixed

- Check shell-alias PATH collisions without blocking same-install-ID updates, report dry-run alias plans, and prune source caches even when OMP alias creation fails.

## [0.2.7] - 2026-11-25

### Added

- Explain alias/profile independence and add collision-specific recovery guidance for separate profiles and registered-alias updates.
- Add persistent Docker Compose Rust gates with named Cargo caches and focused CLI workflow regressions.

### Fixed

- Clarify that registered-alias updates retain their existing topology and never install the newly requested topology.
- Distinguish topology manifests from registry aliases in human-readable status output.

## [0.2.6] - 2026-11-25

### Added

- Refuse new registry aliases that collide with existing executable files on
  `PATH` before named-profile resolution, target creation, or registration,
  including dry-run; same-alias updates retain their upsert behavior.
- Add an installer guide covering profiles, topologies, aliases, executable
  names, version-pinned installs, and troubleshooting.

## [0.2.5] - 2026-11-25

### Added

- Add exact SemVer `--version` assertions to `install` and `update`, and
  reject updates that would downgrade a valid installed topology version.
- Document the per-topology SemVer and changelog workflow, and add a
  changelog for each example topology.

### Fixed

- Include `topology.yml` in local source freshness so metadata-only upgrades,
  downgrades, and invalid declarations are not skipped as up to date.
- Reject malformed declared or recorded topology versions before mutation,
  and preflight install versions before registry locking can create state.

## [0.2.4] - 2026-11-25

### Fixed

- Scope cross-alias skill ownership by the recorded skills root, so removing
  one alias cannot retain or delete a same-named skill under another root.
- Refuse replace-mode installs and updates that would overwrite or remove
  config keys owned by another alias, including dry-run updates.

## [0.2.3] - 2026-11-25

### Changed

- Allow multiple registry aliases to target the same agent directory, each
  with independent per-alias ownership. Cross-alias file or config ownership
  collisions fail closed before any mutation — no alias overwrites or deletes
  another alias's files or config — and `status`, `update`, and `remove`
  stay scoped to their alias. Existing single-alias legacy ownership
  manifests remain supported.

## [0.2.2] - 2026-11-25

### Fixed

- Accept the legacy boolean `config.file_existed` manifest field while
  preserving current ownership validation and user-edited configuration.

## [0.2.1] - 2026-11-25

### Fixed

- Resolve release-gate clippy failures without changing behavior.
- Preserve the file mode conversion on Apple targets.

### Added

- Add `--profile NAME` to `install`, `remove`, and `status` for explicit
  profile targeting, resolved via `omp --profile NAME config path` as an
  argument vector. It is mutually exclusive with `--agent-dir`; without either
  option the ambient profile from `omp config path` remains the target.
- `remove` falls back to the legacy active-profile directory when the topology
  source was deleted after installing.
- Assign `digital-product-studio` its own OMP profile and bump the topology to
  `2.0.0`. The previous default-profile installation is retained until the
  operator verifies the new profile and explicitly removes the old overlay.
- Add local and GitHub topology discovery, profile installation, aliases, and lifecycle management to the Rust CLI.
- Add signed Linux/macOS release automation and signature-verified install/self-update.

### Changed

- Preserve user-edited state during updates and keep incomplete-rollback recovery snapshots until reconciliation.
- Protect cached GitHub checkouts from pruning while commands are using them.
- Remove now deletes a `config.yml` the installer created from an absent file once its topology-owned keys are resolved away and no other content remains.
- Refuse incompatible config value types before any install mutation, including before the registry lock creates state.
- Adopt a byte-identical pre-existing file behind a supported main->topology link instead of refusing as a collision; record it as pre-existing so remove and source drops never delete it.
- Release pre-existing adopted files from the ownership manifest during remove without deleting them or counting them as conflicts, and reject malformed `preexisting` manifest metadata before any mutation.
- Track config-link merge ownership explicitly (`config_link_merge`) so a candidate-only link history no longer defeats profile precedence on a later `--config-merge`; existing profile values keep winning until a merge run actually reconciled them.
- Keep a valid `config_replace` backup across candidate and merge updates so remove can still restore the pre-replacement config; only a later explicit `--config-replace` supersedes it.
- Canonicalize the actual `~/.omp` root in profile-link resolution and require the canonical profiles root and link target beneath it, so a `profiles` symlink redirecting outside the OMP root is rejected even when every lexical check passes.

## [0.2.0] - 2026-11-20

### Added

- Add optional `--skills-dir` to install charter skills in a separate directory.
- Record both roots so removal can safely manage an external skills location.
- Keep v0.1.0 single-root ownership manifests removable.

## [0.1.0] - 2026-11-20

### Added

- Add the `omp-topology` CLI with `list`, `install`, `remove`, `--dry-run`, and `--version` commands.
- Add semantic config merging, collision protection, restricted backups, and edit-aware removal with retryable conflict handling.
- Add `digital-product-studio` topology metadata at version `1.0.0` and installer contract tests.
