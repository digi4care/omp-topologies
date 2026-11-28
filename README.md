<div align="center">

<br>

### omp-topologies
**Versioned, self-contained topology overlays for [OMP](https://github.com/can1357/oh-my-pi) agent profiles**<br>
The `omp-topology` CLI discovers topologies in a source tree and installs them into
named OMP profiles: a signed-release bootstrap for the CLI itself, dry-runnable
and reversible installs, an install registry with OMP shell-shortcut support,
and signature-verified self-update — with no server to run and no sudo required.

<br>

<a href="#-quick-start"><img src="https://img.shields.io/badge/Quick_Start-06B6D4?style=for-the-badge&logoColor=white" alt="Quick Start"></a>
<a href="#-features"><img src="https://img.shields.io/badge/Features-D98324?style=for-the-badge&logoColor=white" alt="Features"></a>
<a href="#-architecture"><img src="https://img.shields.io/badge/Architecture-0D1117?style=for-the-badge&logoColor=white" alt="Architecture"></a>
<a href="#️-installing-and-updating-the-cli"><img src="https://img.shields.io/badge/Install_CLI-22C55E?style=for-the-badge&logoColor=white" alt="Install CLI"></a>

<br><br>

![Rust](https://img.shields.io/badge/Rust-1.100.0-CE422B?style=flat-square&logo=rust&logoColor=white)
![crate](https://img.shields.io/badge/crate-0.2.0-orange?style=flat-square)
![License](https://img.shields.io/badge/License-Apache_2.0-blue?style=flat-square)
![Platforms](https://img.shields.io/badge/platforms-Linux_x86_64_%7C_macOS-2C5F2E?style=flat-square)

</div>

---

## 📑 Table of Contents

<table>
<tr>
<td valign="top" width="33%">

**Getting Started**
- [✨ Features](#-features)
- [🧠 Architecture](#-architecture)
- [🚀 Quick Start](#-quick-start)
- [🎮 Interactive Workflow](#-interactive-workflow)

</td>
<td valign="top" width="33%">

**Operations**
- [📖 How the Installer Works](docs/installer.md)
- [⚙️ Installing and Updating the CLI](#️-installing-and-updating-the-cli)
- [📦 Topology Versioning and Updates](#-topology-versioning-and-updates)
- [🧱 Project Structure](#-project-structure)
- [🔧 Configuration Matrix](#-configuration-matrix)

</td>
<td valign="top" width="33%">

**Reference**
- [💡 Design Decisions](#-design-decisions--reliability)
- [⚠️ Known Limitations](#️-known-limitations)
- [📜 License & Credits](#-license--credits)

</td>
</tr>
</table>

---

## ✨ Features

<table>
<tr>
<td width="33%" valign="top">

### 🗂️ Topology discovery
`omp-topology list` reads every `topologies/<name>/topology.yml`
in a source tree and reports name, version, and description —
with `--json` for machine-readable output.

</td>
<td width="33%" valign="top">

### 🔍 Plan before you apply
`install`, `remove`, `update`, and `self-update` accept `--dry-run`:
the plan is printed (`would write: ...`). Dry runs skip topology,
registry, and persistent source-cache writes; named-profile resolution
may invoke OMP and create missing profile directories, which the CLI
removes only if empty, on a best-effort basis.

</td>
<td width="33%" valign="top">

### 🔒 Signed installs, fail closed
The bootstrap and `self-update` verify the RSA/SHA-256 signature of
`SHA256SUMS` and the asset checksum before replacing anything.
There is no unsigned fallback.

</td>
</tr>
<tr>
<td width="33%" valign="top">

### 🧩 Config modes that preserve yours
Candidate mode (default) creates an absent active config from the topology.
With a pre-existing or user-edited active config it publishes `config.yml.new`
without rewriting the active file; an installer-created, unedited config can
refresh in place. `--config-merge` (profile or topology precedence) and
`--config-replace` (backup + restore on remove) are explicit.

</td>
<td width="33%" valign="top">

### ♻️ Edit-aware removal
`remove` restores what the topology wrote, but preserves files you
edited after install and config keys you changed — then reports
them, so a retry can finish the job.

</td>
<td width="33%" valign="top">

### 🏷️ Install registry
Each install is registered under a stable install ID (default: the
profile name) in a versioned JSON registry under the XDG state
root. Several install IDs may target the same agent directory, each
with independent ownership. Writes are locked; interrupted runs
stay honestly `Partial`.

</td>
</tr>
<tr>
<td width="33%" valign="top">

### 🖥️ Profile-aware targeting
Targets resolve through `omp config path` /
`omp --profile NAME config path`; profiles are discovered from
`~/.omp`, honoring `OMP_PROFILE`/`PI_PROFILE`. Supported
main→profile symlinks are followed with identity checks.

</td>
<td width="33%" valign="top">

### 🔄 In-place updates
`update --install-id <name>` re-applies the recorded install against
the current source pin — a no-op when current, refusing
downgrades, with `--ref`, `--version`, and `--source` overrides
for deliberate retargeting.

</td>
<td width="33%" valign="top">

### 📊 Status without side effects
`status` reports manifest health and freshness. GitHub freshness
uses `git ls-remote` only — never a clone or cache write — and an
unresolvable source is reported `unknown`, never guessed.

</td>
</tr>
<tr>
<td width="33%" valign="top">

### 🗄️ Bounded source cache
GitHub sources are pinned to full 40-hex commit SHAs. Per source,
the 10 most recent unreferenced checkouts are retained, and a
checkout in use by a running command is never pruned.

</td>
<td width="33%" valign="top">

### 🤖 Machine-readable output
`--json` on every command emits exactly one JSON document on
stdout. Errors go to stderr as `error: <message>` with exit code
1; clap usage errors exit 2.

</td>
<td width="33%" valign="top">

### 🛡️ Descriptor-anchored filesystem
Every mutation re-walks paths from pinned `openat(O_NOFOLLOW)`
descriptors, so a symlink swapped mid-command fails closed instead
of being followed. It is the crate's only `unsafe` boundary.

</td>
</tr>
<tr>
<td colspan="3" valign="top">

### 🔗 Shell shortcut installs
`install --profile P --alias C` additionally asks OMP to create a shell
command `C` that starts OMP with profile `P`: after the topology install
commits, the installer delegates to `omp --profile P --alias C` and
captures OMP's output. A failed shortcut is an explicit partial-completion
error naming the manual `omp --profile P --alias C` recovery command —
the committed install stays registered. Dry runs report the delegation but
never invoke it; `update` and `remove` never create or delete shortcuts.

</td>
</tr>
</table>

---

## 🧠 Architecture

The CLI is a single Rust crate. `src/main.rs` parses subcommands with clap and
orchestrates; each install has an ownership manifest recording SHA-256 hashes of
installed files and prior config values. A cross-agent install registry lives
under the XDG state root. One install request flows like this:

```
$ omp-topology install -t digital-product-studio \
    --source github:digi4care/omp-topologies --profile digital-product-studio
        │
        ▼
src/main.rs       clap parse; resolve target profile via
                  `omp --profile NAME config path` (argv, never a shell)
        │
        ▼
src/source.rs     resolve github: source → git checkout pinned to a
                  40-hex commit (persistent cache, or ephemeral for --dry-run)
        │
        ▼
src/topology.rs   locate topologies/<name>, parse topology.yml,
                  walk the agent/ overlay (symlinks rejected)
        │
        ▼
src/install.rs    plan every change, refuse collisions, then apply in one
                  transaction with timestamped backups and rollback
        │
        ▼
src/state.rs      write the ownership manifest (files → SHA-256,
                  config prior/installed leaf values)
        │
        ▼
src/registry.rs   stage the install entry as Partial, run the install under
                  the registry lock, flip it to Installed
        │
        ▼
(optional)        with --profile P --alias C, delegate shell-shortcut
                  creation to `omp --profile P --alias C` after the
                  install lands
```

---

## 🚀 Quick Start

Install the CLI with the signed-release bootstrap (needs `curl`, `openssl`,
`tar`, and `sha256sum`/`shasum`; installs into `~/.local/bin` by default, no sudo):

```sh
curl -fsSL https://raw.githubusercontent.com/digi4care/omp-topologies/main/scripts/install-release.sh | sh
```

> **Release availability.** The bootstrap requires a signed GitHub Release with
> assets, and **no release has been published yet** — check the
> [releases page](https://github.com/digi4care/omp-topologies/releases) before
> running it. Until a signed release exists, run the CLI from a checkout
> instead (`cargo run -- <args>`), as shown below.

Once the CLI is available, discover and install a topology into an OMP profile:

```sh
# List installable topologies in a source (an installed binary has no
# checkout, so pass the source explicitly)
omp-topology list --source github:digi4care/omp-topologies

# Preview the plan; change nothing
omp-topology install -t digital-product-studio \
  --source github:digi4care/omp-topologies \
  --profile digital-product-studio --dry-run

# Apply it, optionally also creating a `studio` shell command that runs
# OMP with this profile (via `omp --profile digital-product-studio
# --alias studio`), then start OMP with that profile
omp-topology install -t digital-product-studio \
  --source github:digi4care/omp-topologies \
  --profile digital-product-studio --alias studio
omp --profile digital-product-studio
```

### Diagnose a host and enable shell completion

`doctor` checks local OMP and installer state without repairing it, contacting
topology sources, or evaluating shell startup files. It does not infer whether
a shell alias is configured. `doctor --json` emits one JSON document; errors
return exit code 1. See the [installer guide](docs/installer.md) for scope and
safe handling of unregistered data.

Generate completions for Bash, Fish, or Zsh with `omp-topology completions
<shell>` and install the output in that shell's completion directory. The CLI
does not edit shell configuration. Regenerate the script after updating the
CLI.

```sh
mkdir -p ~/.local/share/bash-completion/completions
omp-topology completions bash > ~/.local/share/bash-completion/completions/omp-topology

mkdir -p ~/.config/fish/completions
omp-topology completions fish > ~/.config/fish/completions/omp-topology.fish

mkdir -p ~/.zfunc
omp-topology completions zsh > ~/.zfunc/_omp-topology
```

For Zsh, make `~/.zfunc` available to the completion loader by adding this
to `~/.zshrc`, then restart the shell:

```sh
fpath=(~/.zfunc $fpath)
autoload -Uz compinit
compinit
```

From a checkout of this repository, drop `--source` (the checkout root is the
default source) and prefix commands with `cargo run --`:

```sh
git clone https://github.com/digi4care/omp-topologies.git
cd omp-topologies
cargo run -- list
cargo run -- install -t digital-product-studio --profile digital-product-studio --dry-run
```

More guidance: [docs/installer.md](docs/installer.md) (how the installer
works: install IDs, profiles, shell aliases, preflight checks, and
troubleshooting),
[docs/v0.3-migration.md](docs/v0.3-migration.md) (the v0.3.0 `--alias` →
`--install-id` contract for users coming from v0.2),
[topologies/README.md](topologies/README.md) (payload layout and
discovery), [CONTRIBUTING.md](CONTRIBUTING.md) (adding a topology, verification).

---

## 🎮 Interactive Workflow

The day-to-day lifecycle is `list → install → status → update → remove`:

1. **Discover.** `omp-topology list --source github:digi4care/omp-topologies`
   prints each installable topology's name, version, and description as declared
   in its manifest. `omp-topology profiles list` shows the profiles found under
   `~/.omp` and the installs managed under each.
2. **Preview.** `install --dry-run` prints the planned changes (`would write: ...`,
   including paths that go through a supported profile symlink) and exits 0
   without touching the target, the registry, or the cache. Profile resolution
   through `omp --profile NAME config path` can create a missing profile as an
   OMP side effect; explicit `--agent-dir` and ambient `omp config path`
   resolution do not create a target directory.
3. **Install.** Creating a new install ID first passes the registry
   preflight: the ID must not already be registered for another target.
   Requesting a shell shortcut with `--profile P --alias C` additionally
   passes the alias name preflight before any target or profile mutation:
   no executable file named `C` may exist on `PATH` (shell aliases and
   functions are not detectable). The real run then applies the overlay in
   one transaction, registers the install, and — with `--alias` — delegates
   shortcut creation to `omp --profile P --alias C`.
   Re-installing under the same install ID and target is an intentional
   upsert; an install ID pointing elsewhere is refused with an actionable
   error. See
   [docs/installer.md](docs/installer.md#alias-name-preflight).
   A second install ID may target the same agent directory: each install
   owns only what it installed, and any overlap with another install's
   files or config keys is refused before anything is mutated.
4. **Inspect.** `status --install-id <name>` reports what the registry records,
   whether the ownership manifest is healthy, and whether the source has moved
   on (`up to date` / `update available` / `unknown`).
5. **Update.** `update --install-id <name>` re-applies the recorded install against
   the current pin; it is a no-op (no clone) when already current and refuses
   to move the install to an older topology version.
6. **Remove.** `remove --install-id <name>` restores prior config values and deletes
   what the topology added under that install. Files you edited after install are
   preserved and listed, and the removal stays retryable until everything is
   resolved. Neither `update` nor `remove` creates or deletes shell shortcuts.

Every command accepts `--json`, which emits exactly one JSON document on stdout;
in human mode the captured command lines are printed unchanged. Failures print
`error: <message>` on stderr and exit 1.

---

## 📦 Topology Versioning and Updates

Every topology carries its own [SemVer](https://semver.org) version in its
`topology.yml` `version` field, plus its own Keep-a-Changelog-style
`CHANGELOG.md` next to the manifest. These versions are independent of each
other and of the CLI's own release version.

**Versioning contract for topology authors:**

- **Every change bumps the version.** Any change to a topology's payload or
  metadata — `agent/` files, `topology.yml` fields, included docs that ship
  with the payload — requires a SemVer bump in the same PR, with a matching
  entry in that topology's `CHANGELOG.md`. There is no "trivial" topology
  change that skips the bump: `update`'s downgrade check and exact
  `--version` assertions compare against the declared version. `status`
  never compares versions — it reports source commit/digest freshness.
- **Bump semantics.** `major` for changes that alter the topology's contract
  with its users (routing model, role boundaries, approval gates, removed or
  renamed agents/skills/config keys); `minor` for additive, backward
  compatible changes (a new agent or skill, a new optional config key); `patch`
  for fixes and clarifications that do not change the contract (policy
  corrections, ownership fixes, wording).
- **CLI releases are separate.** `omp-topology self-update --version` (and the
  bootstrap's `--version`) pin the *CLI* release; topology versions live in
  `topology.yml` and never move the CLI.

**Versioning contract for users:**

- `--ref` selects *which Git ref of the source tree* is read — a branch, tag,
  or commit. It only picks the checkout; it does not imply a topology version.
- `--version` on `install` and `update` is an **exact SemVer assertion**, not
  a tag resolver: the command succeeds only if the selected source's
  `topology.yml` declares exactly that version, and fails otherwise. Combine
  it with `--ref` when you need both a specific ref and a specific declared
  version.
- `update --install-id <name>` always targets the install recorded under
  that *existing* install ID and re-applies it against the selected source.
  It refuses to move an install to an older topology version — downgrades
  are rejected, so a stale `--ref` or an older source cannot silently roll
  you back.

**Verifying an update from version X to Y** (use an install ID that already
exists — `update` does not create installs):

```sh
# 1. Dry-run against the existing install ID; change nothing. The plan lists the
#    planned file and config changes but prints no version numbers. The
#    --version 2.0.1 assertion is itself the version proof: the command
#    fails here unless the selected source's topology.yml declares exactly
#    2.0.1.
omp-topology update --install-id digital-product-studio \
  --ref main --version 2.0.1 --dry-run

# 2. Apply for real once the plan looks right. The same --version assertion
#    is re-checked before anything is mutated.
omp-topology update --install-id digital-product-studio \
  --ref main --version 2.0.1

# 3. Confirm what the registry records: source, commit/digest, and
#    freshness. status compares commit/digest freshness only and does not
#    print a topology version.
omp-topology status --install-id digital-product-studio
```

Omit `--ref` to update against the source recorded at install time; omit
`--version` when you do not need to assert the exact target version.

---

## ⚙️ Installing and Updating the CLI

There is no server to deploy — the release workflow builds a single static
Linux binary and native macOS binaries per platform, installed per user and
updated in place.

**From the repository root** (runs the checked-in bootstrap; downloads, verifies, installs):

```sh
# Latest signed release into ~/.local/bin
sh scripts/install-release.sh

# Or pipe it directly:
curl -fsSL https://raw.githubusercontent.com/digi4care/omp-topologies/main/scripts/install-release.sh | sh

# Pinned release and/or custom location
sh scripts/install-release.sh --version v0.2.0 --bin-dir ~/.local/bin
```

The script selects the asset for the current platform (Linux x86_64 static
musl, macOS Intel, macOS Apple Silicon), downloads `SHA256SUMS` and its
detached RSA/SHA-256 signature from the same release, verifies the signature
with the embedded public key, verifies the asset's checksum entry, and only
then replaces the binary — atomically, in the same directory. Verification,
download, and staging failures before the final atomic rename leave the
previous installation untouched. See [RELEASING.md](RELEASING.md) for the
full trust model and manual verification commands.

> Until a signed release is actually published, the bootstrap fails closed and
> installs nothing — check the [releases page](https://github.com/digi4care/omp-topologies/releases)
> first, and run from a checkout (`cargo run -- ...`) in the meantime.

**Self-update** (from an installed CLI; same signature verification):

```sh
omp-topology self-update            # latest release
omp-topology self-update --version 0.2.0 --dry-run   # plan a pinned update
```

**From source** (any checkout; requires a Rust toolchain):

```sh
cargo build --release   # binary at target/release/omp-topology
cargo test --locked     # full local test run
```

The pinned quality gates (`fmt`, `clippy -D warnings`, `cargo test --locked`
in a Docker toolchain) live in [scripts/rust-gates.sh](scripts/rust-gates.sh);
CI runs them on pull requests that change Rust inputs
([.github/workflows/rust-gates.yml](.github/workflows/rust-gates.yml)).

**Persistent dev container (Docker Compose).** For local Rust work, a
long-lived Compose service keeps a pinned Rust 1.100.0 container running with
warm Cargo caches (named volumes), so repeated gate runs skip toolchain setup
and recompilation:

```sh
docker compose up -d --build                       # build and start the container
docker compose exec rust ./scripts/rust-gates.sh   # run the gates inside it
docker compose down                                # stop; caches are kept
```

The repository is bind-mounted at `/workspace`; `CARGO_HOME=/cache/cargo` and
`CARGO_TARGET_DIR=/cache/target` live on the named volumes
`omp-topology-cargo-home` and `omp-topology-cargo-target`, and
`OMP_TOPOLOGY_RUST_CONTAINER=1` makes `scripts/rust-gates.sh` run its
in-container path instead of starting nested Docker. `docker compose down`
preserves the caches — only `docker compose down -v` deletes them. This warms
local Docker runs only; GitHub-hosted CI caches separately and is unchanged.

---

## 🧱 Project Structure

```
├── Cargo.toml               # crate manifest: name, version, edition, license
├── src/
│   ├── main.rs              # CLI entry point: subcommands, output, exit codes
│   ├── install.rs           # install transaction: plan, apply, backups, rollback
│   ├── remove.rs            # edit-aware, retryable uninstallation
│   ├── update.rs            # per-install refresh against the current source pin
│   ├── status.rs            # read-only install status and freshness
│   ├── profiles.rs          # OMP profile discovery and CLI target resolution
│   ├── registry.rs          # install ID → install metadata registry (locked, versioned)
│   ├── state.rs             # XDG state root, ownership manifest, snapshots
│   ├── source.rs            # github:/local source resolution + bounded cache
│   ├── topology.rs          # topology.yml parsing and overlay discovery
│   ├── symlinks.rs          # supported main→profile symlink handling
│   ├── self_update.rs       # signature-verified in-place self-update
│   ├── config_mode.rs       # candidate / merge / replace config handling
│   └── anchored/            # descriptor-anchored FS (the only unsafe boundary)
├── topologies/
│   ├── README.md            # payload layout and discovery contract
│   └── digital-product-studio/   # the installable topology (agent/ + topology.yml)
├── scripts/
│   ├── install-release.sh   # fail-closed signed-release bootstrap
│   └── rust-gates.sh        # pinned fmt/clippy/test gate entry point
├── tests/
│   ├── installer.rs         # installer contract suite
│   ├── cli.rs               # CLI surface tests
│   └── regressions.rs       # regression tests
├── keys/omp-topology-release-public.pem   # public half of the release signing key
├── .github/workflows/       # release.yml (signed release), rust-gates.yml (Rust PR gates),
│                            # topology.yml (changed-topology PR gates)
└── docs/                    # contributor templates and project-wide notes
```

A topology payload is `topologies/<name>/` with a `topology.yml` manifest
(`name`, `version`, `description`, `installable: true`, optional `profile`)
plus an `agent/` overlay; the selected config mode handles `agent/config.yml`
separately. Candidate may publish raw bytes as `config.yml.new`; replacement
installs raw bytes as the active config; merge performs structural merging.
`agent/skills/*` can be routed to a
separate root with `--skills-dir`. The installer rejects symlinks and unsafe
relative paths inside the overlay. The source tree itself is the catalog:
there is no hand-maintained index file.

---

## 🔧 Configuration Matrix

The CLI's configuration surface is its flags plus a few environment variables.
Defaults below are what the code actually applies when the option is absent.

| Variable / flag | Applies to | What it does | Default |
|---|---|---|---|
| `--source` | `list`, `install`, `update` | Topology source: `github:OWNER/REPO`, an HTTPS GitHub URL, or a local path containing `topologies/` | current checkout root (installed binaries must pass it) |
| `--ref` | `list`, `install`, `update` | Selects which Git ref of the source is read — a branch, tag, or full 40-hex SHA; it picks the checkout only and never resolves a topology version. For list/install it requires a GitHub `--source`. For update, optionally overrides the recorded GitHub source's ref without a new `--source`; invalid with a local source. | the remote's default `HEAD` |
| `--profile` | `install`, `remove`, `status` | Target profile, resolved via `omp --profile NAME config path` (mutually exclusive with `--agent-dir`) | ambient profile from `omp config path` |
| `--agent-dir` | `install`, `remove`, `status` | Explicit target agent directory | — |
| `--install-id` | `install`, `remove`, `status`, `update` | Stable registry ID selecting the install | profile name, else lowercased agent-dir basename; `update` requires it |
| `--alias` | `install` | OMP shell shortcut created after a successful install by delegating to `omp --profile <profile> --alias <alias>`; requires `--profile`, incompatible with `--agent-dir`, preflight-checked against `PATH` executables | no shell shortcut |
| `--skills-dir` | `install`, `remove` | Explicit root that receives the overlay's `skills/*` | skills install under the agent directory |
| `--config-merge` + `--config-precedence` | `install`, `update` | Structural merge into the active `config.yml`; `profile` (default) or `topology` wins | candidate mode: create an absent active config; otherwise preserve a pre-existing/user-edited active config and publish `config.yml.new` |
| `--config-replace` | `install`, `update` | Replace the active config; the pre-replacement config is restored on `remove` | candidate mode |
| `--dry-run` | `install`, `remove`, `update`, `self-update` | Print the plan without topology, registry, or persistent source-cache writes; named-profile resolution may invoke OMP and create missing profile directories | off |
| `--json` | all commands | Exactly one JSON document on stdout instead of human lines | human mode |
| `--version` | `install`, `update`, `self-update` | On `install`/`update`: an exact SemVer assertion against the selected source's `topology.yml` `version` — the command fails on mismatch; it is not a tag resolver. On `self-update`: the CLI release to install | no assertion; `self-update` uses the latest published release |
| `OMP_PROFILE`, `PI_PROFILE` | target resolution | Active profile name for the ambient target | `default` |
| `XDG_STATE_HOME` | state location | Root for the registry, manifests, snapshots, and source cache | `~/.local/state/omp-topology` |
| `OMP_TOPOLOGY_BIN_DIR` / `--bin-dir` | `install-release.sh` | Install directory for the CLI binary | `~/.local/bin` |
| `OMP_TOPOLOGY_VERSION` / `--version` | `install-release.sh` | Release tag to install | `latest` |
| `OMP_TOPOLOGY_REPO` | `install-release.sh` | `owner/repo` override for the release source | `digi4care/omp-topologies` |

---

## 💡 Design Decisions / Reliability

- **Fail-closed release trust** — bootstrap and `self-update` verify the
  RSA/SHA-256 signature of `SHA256SUMS`, then the asset's SHA-256 entry;
  verification, download, and staging failures before the final atomic rename
  leave the previous binary untouched. The release workflow signs and
  then re-verifies before publishing, and aborts if the signing secret is
  missing or mismatched. There is no unsigned fallback anywhere.
- **Plan, then apply in one transaction** — planning reads every target exactly
  once and refuses all collisions before anything is mutated; the apply phase
  re-verifies live bytes just before each replacement, with timestamped backups
  and rollback that undoes only what the run changed.
- **Edit-aware removal** — files edited after install and config keys changed
  by the user are preserved and reported, not clobbered; the ownership manifest
  keeps the unresolved state so a later retry finishes the job.
- **One topology per install ID, independent ownership** — multiple
  install IDs may target the same agent directory; each install owns only
  the files and config keys it installed, tracked per install in the
  ownership manifest. Any file or config key already owned by another
  install is refused before mutation; no install overwrites, deletes, or
  depends on another install's files. Ownership manifests written before
  per-install ownership (single-install) remain supported.
  Reinstalling under the same install ID and target is an
  upsert; reusing the ID for another target is refused.
- **Shell shortcuts belong to OMP** — `--alias` never reimplements
  shell-rc editing: the installer delegates to
  `omp --profile <profile> --alias <alias>` after the install commits and
  captures OMP's output. A failed shortcut is an explicit partial-completion
  error naming the manual recovery command, and the committed install stays
  registered rather than being rolled back unsafely. `update` and `remove`
  never create or delete shortcuts, and `--dry-run` never invokes the
  delegation.
- **Config modes are never remembered** — `update` and `remove` derive their
  behavior from what the ownership manifest recorded, not from flags a previous
  command happened to pass.
- **Bounded caches and snapshots** — per GitHub source, the 10 most recent
  unreferenced checkouts are retained; per agent, the 10 most recent snapshots
  are normally retained, with recovery-pinned snapshots retained additionally
  until reconciliation succeeds.
  A checkout a running command is using holds a lease that pruning respects.
- **Honest partial state** — a registry entry is staged as `Partial` before the
  install and flipped to `Installed` only after it lands, so an interrupted run
  never claims more than what exists.

---

## ⚠️ Known Limitations

- **No signed release published yet** — the bootstrap and `self-update` require
  a signed GitHub Release with assets. Check the
  [releases page](https://github.com/digi4care/omp-topologies/releases) first;
  until one exists, run the CLI from a checkout (`cargo run -- ...`) and use
  `scripts/install-release.sh` only once a release is available.
- **Platform coverage** — Linux x86_64 only, as a static musl build (no Linux
  aarch64/armv7/glibc-specific builds); macOS Intel and Apple Silicon. No
  Windows builds.
- **External tools** — profile targeting invokes `omp` (`config path`), and
  shell-shortcut installs invoke `omp --profile <name> --alias <alias>`;
  GitHub sources invoke `git`; release installs and self-update need `curl`,
  `openssl`, and `tar`; the bootstrap needs `sha256sum` (Linux) or `shasum`
  (macOS).
- **Cross-install ownership collisions are refused** — a second install ID
  may install into the same agent directory, but differing file bytes or
  config values already owned by another install are a fail-closed
  collision; remove the owning install first if the overlap must go away.
- **Freshness can be `unknown`** — when a source cannot be resolved (network
  failure, moved or deleted local checkout, removed topology), `status` says so
  instead of guessing.
- **Installed binaries carry no checkout** — outside this repository, pass
  `--source github:digi4care/omp-topologies` explicitly (or run from a checkout).

---

## 📜 License & Credits

Apache License 2.0. See [LICENSE](LICENSE); attribution notes are in
[NOTICE](NOTICE) (Copyright 2026 Chris Engelhard). Release signing and the
trust model are documented in [RELEASING.md](RELEASING.md); the public half of
the release key is committed at
[keys/omp-topology-release-public.pem](keys/omp-topology-release-public.pem).
Built for [OMP](https://github.com/can1357/oh-my-pi) agent configurations.
