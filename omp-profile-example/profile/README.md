# OMP Profile Example

## What it does

This is a minimal, general-purpose OMP instruction overlay that demonstrates an installable profile package. The installer copies `agent/RULES.md`; it does not add a domain-specific persona, skills, tools, or an agent configuration file.

## Installer

To install this profile, use [`omp-profile-installer`](https://github.com/digi4care/omp-profile-installer), which provides the `omp-profile` command. Follow its README for installation instructions.

The rules tell an agent to follow repository instructions, stay within the user's requested scope, and verify completed work with relevant focused checks.

## Package contents

- `topology.yml` identifies the package as `omp-profile-example` version `1.0.0`.
- `agent/RULES.md` contains the example behavior installed into the target profile.
- `CHANGELOG.md` sits beside `topology.yml` in this directory and holds the package history; it is already part of the complete package, so no separate changelog copy or assembly step is required.

## Preview installation

From the installer repository root, list and preview the package before installing:

```sh
omp-profile list --source ./omp-profile-example/profile
omp-profile install -t omp-profile-example \
  --source ./omp-profile-example/profile \
  --dry-run --agent-dir "$(mktemp -d)/agent"
```

Review the dry-run file set before removing `--dry-run`.

## Adapting this package

For a new profile, change the package slug, description, and version in `topology.yml`; replace `agent/RULES.md` with the profile's actual instructions; and add a dated version entry to `CHANGELOG.md` beside `topology.yml`. Keep profile-specific design records in the profile repository's own `docs/` directory, outside the installed `agent/` payload.

When publishing a profile repository, set its GitHub topics to `omp-profiles` and `omp-profile` so it can be found through periodic searches for those topics. These are GitHub repository topics, not `topology.yml` tags.
