# Standalone profile package specification

This document defines the authoring contract for a standalone profile package accepted by `omp-profile`. It is the normative target for new standalone profile repositories. The installer repository, [`omp-profile-installer`](https://github.com/digi4care/omp-profile-installer), also includes catalog profiles under `topologies/`. Select a source explicitly when listing or installing.

## Repository root and installer source root

A standalone profile repository separates human-facing documentation from the package source passed to the installer:

```text
<repository-root>/
├── README.md                 # human-facing profile overview
├── docs/                     # optional profile-specific design records
└── profile/                  # installer source root; pass --source ./profile
    ├── topology.yml
    ├── CHANGELOG.md
    └── agent/
```

The repository root is where the profile's README and design records live. The installer source root is `profile/`, containing `topology.yml`, `CHANGELOG.md`, and `agent/`. Only the source root is supplied to `omp-profile` with `--source`; only its `agent/` directory is installed.

`profile/topology.yml` must parse and set `installable: true` for the package to be discoverable. It must declare:

- `name`: the lowercase kebab-case package slug used with `-t`; it is not required to match the repository directory name.
- `version`: the package's current semantic version.
- `description`: a concise human-readable package description.

`profile` is optional in the manifest and should be set only when the package needs a non-default profile suggestion. Keep the human-facing README at repository-root `README.md` and optional design records in repository-root `docs/`; neither is part of the installed payload. Keep exactly one `CHANGELOG.md` in the source root beside `topology.yml`. The example package source is `omp-profile-example/profile/`, with its sole example manifest at `omp-profile-example/profile/topology.yml`.

The repository-root README must explain the profile's purpose, intended users and tasks, installed behavior and additional instructions, prerequisites, limitations, and non-goals. State clearly which files or rules the profile adds and avoid claiming capabilities that are not included in its payload.

When publishing a profile repository, set the GitHub repository topics to
`omp-profiles` and `omp-profile`. These GitHub topics let profiles be found
through periodic topic searches; they are repository metadata, not tags in
`topology.yml`.

## Installed payload and file handling

The source-root `agent/` directory is the only package content copied by the installer. Its nested files and directories are copied recursively. Keep the repository-root README and design records outside `agent/`; keep the package's single `CHANGELOG.md` in `profile/`, beside `topology.yml` and outside `agent/`.

`agent/config.yml` is not copied verbatim. The installer handles it according to the selected config mode:

- In default candidate mode, an existing active config is preserved byte-for-byte and the package config is published as `config.yml.new`; if no active config exists, the package config becomes `config.yml`.
- `--config-merge` requests a structural merge into the profile config.
- `--config-replace` requests full replacement and can overwrite unrelated profile settings; the installer backs up the existing config first.

The installer rejects symlinks and unsafe relative paths anywhere under `agent/`. Use ordinary files and directories with safe relative paths in the payload.

Profile-specific design and decision documents belong in the profile repository's root `docs/`. They are tracked source documentation, not installer payload: only `profile/agent/` is installed. Do not treat local-only archives as the profile documentation source.

## Versioning

Each package is independently versioned with SemVer in `profile/topology.yml`. Bump the package version for every change to its payload or metadata, including shipped documentation. Keep exactly one `profile/CHANGELOG.md` beside `profile/topology.yml`, with a dated entry for each version describing the actual changes. Versions are independent of `omp-profile` CLI releases.

Use major for breaking changes to the package's user-facing contract, minor for additive backward-compatible behavior, and patch for fixes or clarifications that leave that contract intact. The CLI's `--version` option is an exact assertion against the selected source manifest; it does not resolve tags. The CLI refuses updates to an older package version.

After publishing a higher package version to the recorded source, `status --install-id <id>` reports that the source has moved on. `update --install-id <id>` reapplies an existing install from its recorded source pin and is a no-op when already current; it never creates a new install. See the installer's [update guide](../../docs/installer-guide.md) for the full lifecycle.

## Validation and source selection

Always select the standalone package source explicitly when listing or previewing installation. Run these commands from the profile repository root; `profile/` is the installer source root:

```sh
PROFILE_SLUG=your-profile-slug
omp-profile list --source ./profile
omp-profile install -t "$PROFILE_SLUG" --source ./profile \
  --dry-run --agent-dir "$(mktemp -d)/agent"
```

Confirm that `list` shows the expected name, version, and description. Inspect the dry-run plan and its file set before installing. The `/agent` basename in the example ensures the default install ID derived from the agent directory meets the installer's lowercase identifier rules. Use the repository's documented validation workflow for any additional package checks.
Profile repositories are not Rust crates. Their push validation should check the package contract with the installer CLI; Rust tests, Clippy, and formatting gates apply to changes in the installer repository, not profile payloads.

Legacy catalog packages at `topologies/<directory>/topology.yml` in an explicitly selected source remain supported for compatibility. They are not the standalone authoring target; new and maintained standalone profiles should use the source-root layout specified here.


