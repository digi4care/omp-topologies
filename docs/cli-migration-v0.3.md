# Migrating to the v0.3.0 CLI contract

v0.3.0 splits the v0.2 registry `--alias` flag into two independent options
with distinct meanings: `--install-id` for the installer's registry, and
`--alias` for an OMP shell shortcut. This guide shows how to update existing
commands, scripts, and JSON consumers.

> **Release status.** v0.3.0 is not published yet — this document describes
> the contract the v0.3.0 CLI implements, so scripts and habits can be
> prepared in advance. A v0.2 CLI keeps its old behavior; everything below
> applies once you run a v0.3.0 binary.

## The four identifiers

| Flag | What it selects | Example |
|---|---|---|
| `--topology` (`-t`) | Which topology in the source tree to install | `digital-product-studio` |
| `--profile` | Which OMP profile to install into | `digital-product-studio` |
| `--install-id` | The installer's registry/lifecycle key: the stable name under which an install is recorded and later inspected, updated, or removed | `studio` |
| `--alias` | A shell command OMP creates (`omp --profile <profile> --alias <alias>`) that starts OMP with the given profile | `studio` |

`--install-id` and `--profile` both default to the same value, which is why
they are easy to conflate: the profile is what `omp --profile <name>` uses,
the install ID is what `omp-profile update/remove/status` use.

## What changed since v0.2

| v0.2 | v0.3 |
|---|---|
| `update --alias X` | `update --install-id X` |
| `remove --alias X` | `remove --install-id X` |
| `status --alias X` | `status --install-id X` |
| `install --profile P --alias X` (registry key only) | `install --profile P --install-id X` (registry only), or `install --profile P --alias C` (registry *and* shell command `C`) |

Rules that come with the split:

- `--alias` requires a named `--profile` and may not be combined with
  `--agent-dir`. `--install-id` has no such restriction.
- The same v0.2 command `install --profile P --alias X` registered a
  registry key only. Under v0.3 it *additionally* creates the OMP shell
  shortcut `X`. To keep a registry-only install, switch it to
  `--install-id X`.
- Passing the old `--alias` to `update`, `remove`, or `status` is a clap
  usage error (exit code 2) — these commands take `--install-id` only.
- The `PATH` executable preflight applies to the requested shell `--alias`,
  not to `--install-id`. An install ID that happens to match a `PATH`
  executable name is allowed; it is an internal key and never becomes a
  command.

## Existing registry records

Records registered under v0.2 stay readable: the on-disk registry schema is
compatible and no record is rewritten or dropped at upgrade. The install ID
of a migrated record is the name it was registered under — use that name
with `--install-id` for `update`/`remove`/`status`.

## JSON output

The `--json` field `alias` is renamed to `install_id` (same value, same
semantics: the registry key of the install). Anything parsing installer JSON
should read `install_id` and treat `alias` as the v0.2 name. No other JSON
fields change.

## Shell shortcut semantics

- `--dry-run` reports the alias delegation in its plan but never invokes
  `omp --profile <profile> --alias <alias>`, so a dry run never creates or
  modifies a shell shortcut.
- `update` and `remove` never create or delete shell shortcuts; shortcuts
  belong to the OMP profile, not to an install's lifecycle.
- If the topology install commits but OMP's alias creation fails, the
  installer reports an explicit partial-completion error naming the manual
  recovery command `omp --profile <profile> --alias <alias>`. The install
  stays registered rather than being rolled back unsafely; run the recovery
  command to finish the shortcut.
