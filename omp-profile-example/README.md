# Create an installer-compatible OMP profile

This kit helps a person or AI produce a standalone profile package for the [`omp-profile-installer`](https://github.com/digi4care/omp-profile-installer), which installs profiles with the `omp-profile` command. Follow the specification for package requirements; this page explains the workflow.

## Sources of truth

1. [`docs/PROFILE-PACKAGE-SPEC.md`](docs/PROFILE-PACKAGE-SPEC.md) is normative. It defines the package layout, manifest, payload, versioning, and validation requirements.
2. [`docs/PROFILE-README-TEMPLATE.md`](docs/PROFILE-README-TEMPLATE.md) is the starting point for the profile's user-facing README.
3. [`profile/`](profile/) is the installer source root: it contains `topology.yml`, `CHANGELOG.md`, and `agent/`. Pass this directory as `--source`; the example manifest exists only at [`profile/topology.yml`](profile/topology.yml).
4. [`profile/CHANGELOG.md`](profile/CHANGELOG.md) is the one package changelog, beside the manifest. Copying the complete `profile/` source directory carries it with the package.

The example repository root is `omp-profile-example/`; it holds this human-facing README and `docs/`. Its `profile/` child is the package source root. For a standalone profile repository, the same distinction applies: keep the human-facing README and profile-specific design records in repository-root `README.md` and `docs/`, and put the package files under `profile/`.

If this page and the specification conflict, follow the specification.

## Define the profile before writing it

Agree on the profile's purpose, intended users, tasks, behavior, extra instructions, required tools or configuration, prerequisites, limitations, and explicit non-goals. Do not infer permissions, integrations, or behavior that the owner has not approved.

## Human workflow

1. Read the specification and inspect the example package.
2. Copy the complete `profile/` directory into the new profile repository as `profile/`. This is the installer source root and contains `topology.yml`, `CHANGELOG.md`, and `agent/`; keep the human-facing `README.md` and profile-specific design records in the repository root and its `docs/`.
3. Replace the example slug, description, rules, and README content with the approved profile requirements. The README must explain what the profile does, what instructions it adds, prerequisites, limits, and how to install or use it. Link to the [installer repository](https://github.com/digi4care/omp-profile-installer) so readers can find the installer.
4. Only files under `profile/agent/` are installed.
5. Validate the assembled package, then commit and publish the manifest, payload, repository-root README, and `profile/CHANGELOG.md` together. Set the GitHub repository topics to `omp-profiles` and `omp-profile` so the profile can be found through periodic searches for those GitHub topics. These are GitHub repository topics, not `topology.yml` tags.

## AI-assisted workflow

> Read this kit's package specification, README template, and example before editing. Create a standalone OMP profile for **[purpose]** in **[target repository]**. Ask me about missing users, tasks, behavior, extra instructions, prerequisites, tools/configuration, boundaries, or non-goals; do not invent them. Follow the package specification exactly. Keep the human-facing README and design records at the repository root and in its `docs/`; put `topology.yml`, `CHANGELOG.md`, and `agent/` under `profile/`. Make the profile README explain its behavior and limits. Keep the one `CHANGELOG.md` beside `topology.yml`, bump SemVer for package changes, and record each version. Do not install into a live OMP profile. Validate with `omp-profile list --source ./profile` and an install dry-run using `--source ./profile`, then report assumptions, changed files, and exact checks.

## Versioning and updates

Every payload or metadata change requires a SemVer bump in `profile/topology.yml` and a dated entry under that version in the single `profile/CHANGELOG.md`, beside the manifest. Use the major/minor/patch rules in the specification; profile versions are independent of the installer version. `status --install-id <id>` reports whether the recorded source has moved on; `update --install-id <id>` reapplies an existing install from its recorded source and is a no-op when current. See the installer's [`update` guide](../docs/installer-guide.md).

## Validate the package

Run these from the profile repository root. The repository-root README and `docs/` are human-facing documentation; `profile/` is the source root passed to `--source` and contains `topology.yml`, `CHANGELOG.md`, and `agent/`:

```sh
PROFILE_SLUG=your-profile-slug
omp-profile list --source ./profile
omp-profile install -t "$PROFILE_SLUG" --source ./profile \
  --dry-run --agent-dir "$(mktemp -d)/agent"
```

Inspect the listed name, version, and description, then review the dry-run file set before installing. Profile repositories are not Rust crates: do not run the installer's Rust tests, Clippy, or formatting gates for profile changes. The shared profile workflow uses the installer CLI for package smoke checks; Rust quality gates belong to changes in the installer repository.
Git hooks are optional local conveniences, not required policy; CI remains the shared gate.
