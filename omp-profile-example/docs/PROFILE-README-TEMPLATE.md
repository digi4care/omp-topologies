# Profile README template

For the normative package contract, see
[`PROFILE-PACKAGE-SPEC.md`](PROFILE-PACKAGE-SPEC.md).

Copy this template to the profile repository root as `README.md`, replacing
every `<...>` placeholder. Keep profile-specific design records in the
repository-root `docs/`. The package is in the separate `profile/` installer
source root, containing `topology.yml`, `CHANGELOG.md`, and `agent/`; pass
`--source ./profile` from the repository root. In the installer repository,
the example manifest exists only at
[`omp-profile-example/profile/topology.yml`](../profile/topology.yml). The
installer repository also includes a catalog under `topologies/`; select it
explicitly when using those profiles. Link readers to the
[`omp-profile-installer`](https://github.com/digi4care/omp-profile-installer)
repository for the `omp-profile` CLI and its installation instructions.
Keep the README concise: it helps a human choose and install the topology and
helps a coding agent orient to its role, boundaries, and source-of-truth files.
Every claim must be verifiable in the topology's own source
(`profile/topology.yml`, `profile/agent/RULES.md`,
`profile/agent/config.yml`, `profile/agent/agents/`,
`profile/agent/skills/`) — do not describe behavior the files do not
implement. Delete sections that do not apply rather than leaving placeholders.
When publishing a profile repository, set its GitHub repository topics to
`omp-profiles` and `omp-profile` so it can be found through periodic searches.
These are GitHub topics, not `topology.yml` tags.

---

# `<Topology Name>` Topology

<One or two sentences: what the topology is and the problem it solves, drawn
from `profile/topology.yml` `description`.>

## When to choose it

<The concrete situations this topology fits, and — just as important — when
to pick a different topology instead. Name the difference from the closest
existing topology so users and agents do not have to diff them.>

## Roles

<Who this is for: the human's role (e.g. product owner approving briefs) and
each agent role the topology defines, with a one-line responsibility each.
Derive roles from `profile/agent/agents/` and `profile/agent/RULES.md`, not
aspiration.>

## Routing and coordination

<How work actually flows: which agent receives which kind of task, how
subagents are spawned or bounded (quote real limits such as
`task.maxRecursionDepth` from `profile/agent/config.yml`), and where
decomposition or integration happens. Describe the routing that is
implemented, not an ideal.>

## Approval boundaries

<What the topology may do autonomously and what requires explicit user
approval (scope changes, merges, external side effects, …). Cite the rule or
config that enforces each boundary.>

## Installation

Requires the `omp-profile` CLI (see the installer repository README for
install instructions). Set `PROFILE_SLUG` to the name in
`profile/topology.yml`, `PROFILE_SOURCE` to the package source root (usually
`./profile` from this repository root), and `OMP_PROFILE` to the target OMP
profile.

```sh
PROFILE_SLUG=your-profile-slug
PROFILE_SOURCE=./profile
OMP_PROFILE=your-omp-profile

# Preview the planned changes first
omp-profile install -t "$PROFILE_SLUG" \
  --source "$PROFILE_SOURCE" \
  --profile "$OMP_PROFILE" --dry-run

# Install for real
omp-profile install -t "$PROFILE_SLUG" \
  --source "$PROFILE_SOURCE" \
  --profile "$OMP_PROFILE"
```

<Note any topology-specific install considerations: required `--profile`,
`--skills-dir`, or follow-up configuration. Omit if none.>

## Versioning

This topology follows [Semantic Versioning](https://semver.org): every change
to its payload or metadata bumps `version` in `profile/topology.yml` and is
recorded in the one [`profile/CHANGELOG.md`](profile/CHANGELOG.md) beside the
manifest. These versions are independent of the `omp-profile` CLI's own
releases. Use `omp-profile update --install-id <install-id> --version X.Y.Z`
to assert the exact version an update installs.

## Limits

<Known boundaries: what the topology deliberately does not do, unverified
behavior, and constraints agents must respect. Honest limits here prevent
misuse; an empty "Limits" section on a non-trivial topology is a smell.>

## Source of truth

| File | Contents |
|---|---|
| [`profile/topology.yml`](profile/topology.yml) | Manifest: `name`, `version`, `description`, optional `profile` |
| [`profile/CHANGELOG.md`](profile/CHANGELOG.md) | Package version history; one dated entry per `version` bump |
| `profile/agent/RULES.md` | Top-level orchestration policy |
| `profile/agent/config.yml` | Task and model-role overlay |
| `profile/agent/agents/` | Agent definitions |
| `profile/agent/skills/` | Charter skills (if present) |

When adapting this template for a legacy catalog package, use paths relative
to that catalog package and remove rows for files that do not exist. For a
standalone profile, paths are relative to the repository root.

## Docs

A profile's design records and decision documents belong in the root `docs/`
directory of that profile's repository. These tracked documents are outside
the installed `profile/agent/` payload. Do not put profile-specific
documentation in the installer's shared `docs/` directory.

Shared project guidance lives under this installer's root `docs/`. Add a
repository-relative link to a specific shared file only when that target
exists in the repository.
