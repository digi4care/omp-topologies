# Installer guide: how `omp-profile` works

This guide explains the moving parts behind `omp-profile install` and the
commands around it, so the behavior — including its fail-closed checks —
is predictable before you run it.

## The five things an install touches

- **Topology** — a versioned, self-contained package in a source. A standalone
  profile repository has `topology.yml` and `agent/` at its root; a catalog
  uses `topologies/<name>/` with the same manifest and payload. The installer
  repository includes catalog profiles under `topologies/`; select the source
  explicitly because the CLI does not choose one implicitly.
- **OMP profile** — a named OMP configuration (e.g. `digital-product-studio`).
  OMP itself selects a profile; `omp --profile <name>` starts OMP with that
  profile.
- **Agent directory** — the on-disk directory OMP resolves for the profile
  (via `omp --profile <name> config path`), where the overlay's files and
  `config.yml` land.
- **Install ID** — a short lowercase name (`[a-z0-9][a-z0-9-]{0,63}`)
  selected with `--install-id`, under which the installer records one
  install: target, topology, source, and ownership manifest. It is an
  **internal installer identifier only**. Passing `--install-id` does
  **not** create an executable, a launcher, a shell alias, or any other
  command — nothing new appears on your `PATH`.
- **Shell command** — an executable file on your `PATH` (e.g.
  `~/.local/bin/teddy`). The installer creates one only when `install` is
  given both `--profile` and `--alias`: after the topology install commits,
  it delegates to `omp --profile <profile> --alias <alias>`, which writes
  the shortcut into your shell configuration. The installer captures OMP's
  output and never reimplements the shell-rc edit itself. At install time
  the requested alias name must not collide with an executable already on
  `PATH` (see [Alias name preflight](#alias-name-preflight)).

The default install ID is the profile name (lowercased), or the lowercased
basename of the agent directory when there is no profile. Pass `--install-id`
explicitly to choose a different name.

## Commands at a glance

| Command | What it does |
|---|---|
| `list` | Shows every installable topology in the explicitly selected source, with `--json` for machine output. Changes nothing. |
| `install` | Plans, then applies a topology into a target in one transaction and registers the install under its install ID; with `--profile P --alias C`, also delegates shell-shortcut creation to `omp --profile P --alias C` after the install lands. |
| `status --install-id <name>` | Read-only: what the registry records, manifest health, and whether the source has moved on. |
| `update --install-id <name>` | Re-applies an *existing* install's recorded install against the current source pin; a no-op when already current. Never creates an install. |
| `remove --install-id <name>` | Restores prior config values and deletes what the topology added under that install ID. Files you edited since install are preserved and reported. |

Every command accepts `--json` (exactly one JSON document on stdout) and
failures print `error: <message>` on stderr with exit code 1.

> **Coming from v0.2?** The registry selector used to be `--alias`; it is
> now `--install-id`, and `--alias` means an OMP shell shortcut instead. See
> [v0.3 migration](cli-migration-v0.3.md) before updating scripts.

**Dry run.** `install`, `remove`, `update`, and `self-update` accept
`--dry-run`: the plan is printed and no topology, registry, or persistent
source-cache writes happen. A dry run reports a requested
`--profile P --alias C` delegation in its plan but never invokes
`omp --profile P --alias C`, so no shell shortcut is created or modified.
One caveat: resolving a named profile invokes
`omp --profile <name> config path`, which can create a missing profile
directory as an OMP side effect (the CLI removes it again only if empty,
best-effort). Everything else — including the preflight below — runs the same
checks as a real install, evaluated against the state at the time the
command runs; a later real run re-checks against whatever state it finds
then.

## Shell shortcuts (`--alias`)

`install --profile P --alias C` installs the topology as usual and then, in
a second step, asks OMP to create the shell shortcut:

```sh
omp-profile install -t digital-product-studio \
  --profile digital-product-studio --alias studio \
  --source github:digi4care/omp-profile-installer
# topology install commits, then the installer runs:
#   omp --profile digital-product-studio --alias studio
# and captures OMP's output.
```

- `--alias` requires a named `--profile` and may not be combined with
  `--agent-dir` (OMP creates the shortcut against a profile).
- The delegation runs only after the topology install has committed. If OMP
  fails to create the shortcut, the installer reports an explicit
  partial-completion error that names the manual recovery command
  `omp --profile <profile> --alias <alias>`; the topology install stays
  registered rather than being rolled back unsafely. Run the recovery
  command to finish the shortcut.
- `update` and `remove` never create or delete shell shortcuts. Shortcuts
  belong to the OMP profile, not to an install's lifecycle; removing an
  install leaves any shortcut it once created in place.

- `--source` selects the topology source: `github:OWNER/REPO`, an HTTPS
  GitHub URL, or a local path. It is required for `list` and `install`; this
  repository has no bundled catalog or default source. The source can be a
  standalone package root containing `topology.yml` and `agent/`, or a legacy
  catalog containing `topologies/<slug>/topology.yml`. The declared manifest
  name selects the package with `-t <slug>`. For example:

  ```sh
  omp-profile list --source /path/to/omp-profile-digital-product-studio
  omp-profile install -t digital-product-studio \\
    --source /path/to/omp-profile-digital-product-studio \\
    --profile digital-product-studio
  omp-profile list --source github:digi4care/omp-profile-installer
  ```

  Legacy catalog sources remain supported when explicitly selected. `update`
  uses the source recorded for the existing install unless `--source` overrides
  it; an installed binary has no checkout from which to infer a source.

- `--ref` picks *which Git ref of the source tree* is read — a branch, tag,
  or commit. It only selects the checkout; it never implies a topology
  version.
- `--version` on `install`/`update` is an **exact SemVer assertion** against
  the selected source's `topology.yml` `version`: the command fails unless
  the source declares exactly that version. It is not a tag resolver.
- `update` refuses to move an install to an older topology version, so a
  stale `--ref` or source cannot silently roll you back.

## Profiles, install IDs, and shared agent directories

Each install ID maps to exactly one target. Re-installing under the same
install ID for the same target is an upsert; reusing an install ID for a
different topology or target is refused before anything is mutated.

Several install IDs may target the **same agent directory** — for example
one profile extended by two independent topologies. This works only when
the installs are independent: each install owns only the files and config
keys it installed, tracked in its own ownership manifest. Any overlap with
another install's files or config keys is refused before any mutation — no
install overwrites, deletes, or depends on another install's files.

## Alias name preflight

Two preflights protect the names an install introduces. Both run before
anything is mutated — including in a `--dry-run` that would create them.

1. **Registry collision (install ID).** If the install ID is already
   registered for a different topology or target, the install stops and the
   error points you at the existing entry: update the existing install with
   `omp-profile update --install-id <name>`, or choose another
   `--install-id`.
2. **Executable collision (shell alias).** If `--alias C` is requested and
   an **executable file** named exactly `C` exists on your `PATH`, the
   install stops before touching the target or the profile. The error names
   the alias and the colliding executable path and suggests choosing
   another `--alias`. The install ID is *not* checked against `PATH`: it is
   an internal registry key and never becomes a command, so an install ID
   that happens to match a `PATH` executable name is allowed.

An install under an **already-registered** install ID for the same target
and topology is an intentional upsert, not a collision: it is not
re-checked against the `PATH` and proceeds normally. `update` remains the
normal workflow for refreshing an existing install, and `remove` and
`status` are unchanged by this preflight — all three only ever touch an
install ID the registry already holds.

Scope of the executable check:

- It matches **executable files on `PATH` only**. A non-executable file or
  a directory with the same name does not block an install.
- **Shell aliases and shell functions cannot be detected** — they live in
  your shell session, not on `PATH` — and are out of scope. If you defined
  `alias studio=...` in your shell, the installer cannot see it; pick a
  distinct alias anyway.

### Worked example: an existing `teddy` command

Say you have an executable at `/home/digi4care/.local/bin/teddy` (some
unrelated app), and you want the `digital-product-studio` topology in an
OMP profile named `teddy`:

```sh
# Blocked: the requested shell shortcut 'teddy' collides with the
# executable /home/digi4care/.local/bin/teddy — the installer stops
# before any target or profile change and suggests another alias.
omp-profile install -t digital-product-studio \
  --profile teddy --alias teddy \
  --source github:digi4care/omp-profile-installer

# Works: install under the default install ID (the profile name) with no
# shell shortcut; the install ID 'teddy' matching a PATH executable is
# allowed because it never becomes a command. Run OMP with
# `omp --profile teddy`.
omp-profile install -t digital-product-studio \
  --profile teddy \
  --source github:digi4care/omp-profile-installer

# Works too: a distinct alias with no PATH collision; this also creates
# the 'omp-teddy' shell shortcut for `omp --profile teddy`.
omp-profile install -t digital-product-studio \
  --profile teddy --alias omp-teddy \
  --source github:digi4care/omp-profile-installer
```

The install ID is only the installer's registry key — it does not shadow or
replace the external `teddy` app, and neither does a shell shortcut unless
its name collides (which the preflight refuses). You always run OMP with
`omp --profile teddy`; that selects the OMP **profile** and does not run
the external `teddy` executable.

## File ownership collisions

Independent of the preflight above, the install transaction itself refuses
to overwrite anything it does not own: existing files that differ from the
topology's bytes, and config keys already owned by another install ID.
These checks run before any mutation, so a refused install leaves the target
untouched. To resolve one, remove the owning install first
(`remove --install-id <name>`) or adjust the topology.

## Host diagnostics and shell completion

`omp-profile doctor` checks the local CLI/OMP environment, registered
installs, ownership manifests, and managed state. With no selector it checks
all registered installs; pass exactly one of `--install-id`, `--profile`, or
`--agent-dir` to narrow the target. It is read-only: it does not run shell
startup files, contact remote topology sources, or repair state. `--json`
prints one JSON document. Exit code 1 means diagnostic errors were found;
warnings alone do not cause failure.

The command does not infer expected shell aliases; alias names are not part of
the install registry. A detected installation without registry ownership is
reported as unregistered data, not adopted or removed. If `omp-profile` itself
cannot run, use the manual CLI and OMP checks in this guide.

Generate shell completion scripts with `omp-profile completions bash`,
`omp-profile completions fish`, or `omp-profile completions zsh`. The
command writes to stdout and never edits shell startup files. Install each
script in the appropriate completion directory; after a CLI upgrade, generate
and install it again so the script matches the new command set.

For Zsh, the generated `_omp-profile` file must live in a directory on
`fpath`. For the README example's `~/.zfunc` directory, add
`fpath=(~/.zfunc $fpath)`, `autoload -Uz compinit`, and `compinit` to
`~/.zshrc`, then restart the shell. Bash loading depends on the installed
`bash-completion` setup; Fish loads scripts from its completions directory.


## Troubleshooting: `unknown install id`

`update`, `remove`, and `status` operate on install IDs already in the
registry. `unknown install id '<name>'` means no install is registered
under that exact name — install IDs are case-sensitive lowercase slugs, and
`update` never creates one. Inspect the registered install IDs with
`omp-profile profiles list`, which shows the discovered profiles and the
managed installs under each, and mind the difference between the install ID
(installer selector) and the profile name (what `omp --profile` uses):
they default to the same value but diverge as soon as you pass
`--install-id`.

Passing the old v0.2 `--alias` to `update`, `remove`, or `status` is a clap
usage error (exit code 2); use `--install-id` instead — see the
[cli-migration-v0.3.md](cli-migration-v0.3.md) for the full before/after.

There is no `rename` subcommand: the registered install ID remains the
lifecycle identifier for `update`, `remove`, and `status`, so choose it
deliberately at first install.
