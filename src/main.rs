//! `omp-profile` command-line entry point.
//!
//! Subcommands:
//!
//! * `list` — list installable topologies from an explicit `--source`.
//! * `install` — merge a topology into an OMP agent directory and register
//!   the install under a stable install ID.
//! * `remove` — remove a topology, restoring prior config values, and
//!   reconcile the install registry.
//! * `profiles list` — filesystem discovery of OMP profiles and
//!   registry-managed installs (never creates profiles).
//! * `status` — read-only status of a managed install ID or explicit target.
//! * `update` — refresh one registry install ID to the current source pin.
//! * `self-update` — install a signed GitHub Release of this CLI.
//!
//! JSON mode (`--json`) emits exactly one JSON document on stdout;
//! human mode prints the captured command lines unchanged. Errors are
//! printed as `error: <message>` on stderr with exit code 1; argument
//! parsing is handled by clap (usage errors exit 2).

// `deny` (not `forbid`) so the descriptor-anchored filesystem layer in
// `anchored` — the single audited `unsafe` boundary, a direct libc
// openat-family binding — can carry a scoped exemption. Every other
// module stays `unsafe`-free.
#![deny(unsafe_code)]

// SAFETY SCOPE: this module is the crate's only `unsafe` code — the
// direct libc openat-family binding implementing descriptor-anchored
// filesystem operations. All invariants are documented at each call
// site; no other module may use `unsafe`.
#[allow(unsafe_code)]
mod anchored;
mod completions;
mod config_mode;
mod doctor;
mod error;
mod fsx;
mod install;
mod profiles;
mod registry;
mod remove;
mod self_update;
mod source;
mod state;
mod status;
mod symlinks;
mod topology;
mod update;
mod value;
mod yamlwork;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use serde_json::{Value as Json, json};

use crate::error::CliError;
use crate::registry::{InstallState, RegistryEntry, SourceSpec};
use crate::source::{SourceCheckout, TopologySource};

/// GitHub cache checkouts retained per source beyond those still
/// referenced by the registry (the same bound `update.rs` uses).
const CACHE_KEEP_UNREFERENCED: usize = 10;

/// Install and manage OMP topology profiles.
#[derive(Parser)]
#[command(name = "omp-profile", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List installable topologies
    List(ListArgs),
    /// Install a topology into an OMP agent dir and register the install
    Install(InstallArgs),
    /// Remove a topology, restoring prior config values
    Remove(RemoveArgs),
    /// List OMP profiles and registry-managed installs
    Profiles(ProfilesArgs),
    /// Show status of a managed install ID or explicit target
    Status(StatusArgs),
    /// Update one registry install ID to the current source pin
    Update(UpdateArgs),
    /// Update this CLI from a signed GitHub Release
    SelfUpdate(SelfUpdateArgs),
    /// Diagnose local OMP profiles and topology installer state
    Doctor(DoctorArgs),
    /// Generate shell completion scripts
    Completions(CompletionsArgs),
    #[command(name = "__complete", hide = true)]
    Complete(InternalCompletionArgs),
}

#[derive(Args)]
struct ListArgs {
    /// Topology source: 'github:OWNER/REPO' or a local path containing
    /// a root topology.yml package or legacy topologies/ catalog
    #[arg(long, required = true)]
    source: Option<String>,
    /// Git ref (branch, tag, or full 40-hex SHA) for a GitHub source;
    /// requires --source
    #[arg(long = "ref", requires = "source")]
    reference: Option<String>,
    /// Emit exactly one JSON document on stdout
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct InstallArgs {
    /// Topology name
    #[arg(short, long)]
    topology: String,
    /// Topology source: 'github:OWNER/REPO' or a local path containing
    /// a root topology.yml package or legacy topologies/ catalog
    #[arg(long, required = true)]
    source: Option<String>,
    /// Git ref for a GitHub source; requires --source
    #[arg(long = "ref", requires = "source")]
    reference: Option<String>,
    /// OMP profile to install into (mutually exclusive with --agent-dir)
    #[arg(long, conflicts_with = "agent_dir")]
    profile: Option<String>,
    /// Target agent directory (default: 'omp config path' for the profile)
    #[arg(long)]
    agent_dir: Option<String>,
    /// Stable install ID for this install (default: the profile name, or
    /// the lowercased basename of the agent directory). The install ID is
    /// the registry key only: it never selects a profile, agent
    /// directory, or PATH executable.
    #[arg(long = "install-id")]
    install_id: Option<String>,
    /// OMP shell shortcut to create in the shell configuration
    /// (requires --profile). The shell alias is independent of the
    /// registry install ID and never influences install selection.
    #[arg(long, requires = "profile", conflicts_with = "agent_dir")]
    alias: Option<String>,
    /// Explicit skills root: only skills/* install there
    #[arg(long)]
    skills_dir: Option<String>,
    /// Merge the topology config into the active config.yml, preserving
    /// profile values (this command only; never remembered)
    #[arg(long, conflicts_with = "config_replace")]
    config_merge: bool,
    /// Replace the active config.yml with the topology config, keeping a
    /// backup for remove (this command only; never remembered)
    #[arg(long)]
    config_replace: bool,
    /// Merge precedence when the profile and topology set the same key:
    /// profile (default) or topology; requires --config-merge
    #[arg(long, value_name = "WHEN", requires = "config_merge")]
    config_precedence: Option<String>,
    /// Require the resolved topology's declared version to match exactly;
    /// never a range, and never compared against the recorded version.
    /// Parsed as strict SemVer, so an invalid value is rejected before
    /// any source resolution or state/target access.
    #[arg(long, value_name = "VERSION", value_parser = parse_semver)]
    version: Option<semver::Version>,
    /// Print the plan, change nothing
    #[arg(long)]
    dry_run: bool,
    /// Emit exactly one JSON document on stdout
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct RemoveArgs {
    /// Topology name; required without --install-id and must match the
    /// install ID's recorded topology
    #[arg(short, long)]
    topology: Option<String>,
    /// Install ID selecting the target and topology; mutually exclusive
    /// with --profile and --agent-dir
    #[arg(long = "install-id")]
    install_id: Option<String>,
    /// Target profile (default: the ambient profile)
    #[arg(long)]
    profile: Option<String>,
    /// Target agent directory
    #[arg(long)]
    agent_dir: Option<String>,
    /// Explicit skills root override; must match the recorded root
    #[arg(long)]
    skills_dir: Option<String>,
    /// Print the plan, change nothing
    #[arg(long)]
    dry_run: bool,
    /// Emit exactly one JSON document on stdout
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ProfilesArgs {
    #[command(subcommand)]
    command: ProfilesCommand,
}

#[derive(Subcommand)]
enum ProfilesCommand {
    /// List known profiles and managed installs (read-only)
    List(ProfilesListArgs),
}

#[derive(Args)]
struct ProfilesListArgs {
    /// Emit exactly one JSON document on stdout
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct StatusArgs {
    /// Install ID to report on; mutually exclusive with --profile and
    /// --agent-dir
    #[arg(long = "install-id")]
    install_id: Option<String>,
    /// Report on this profile's agent directory
    #[arg(long)]
    profile: Option<String>,
    /// Report on this agent directory
    #[arg(long)]
    agent_dir: Option<String>,
    /// Emit exactly one JSON document on stdout
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct UpdateArgs {
    /// Stable install ID to update
    #[arg(long = "install-id")]
    install_id: String,
    /// Ref override (branch, tag, or full 40-hex SHA); GitHub sources only
    #[arg(long = "ref")]
    reference: Option<String>,
    /// Optional replacement source (`github:OWNER/REPO`, a full GitHub
    /// HTTPS URL, or a local path); committed to the registry only after
    /// a successful update
    #[arg(long)]
    source: Option<String>,
    /// Require the resolved topology's declared version to match exactly;
    /// never a range, and never compared against the recorded version.
    /// Parsed as strict SemVer, so an invalid value is rejected before
    /// any source resolution or state access.
    #[arg(long, value_name = "VERSION", value_parser = parse_semver)]
    version: Option<semver::Version>,
    /// Merge the topology config into the active config.yml for this
    /// update only (never remembered)
    #[arg(long, conflicts_with = "config_replace")]
    config_merge: bool,
    /// Replace the active config.yml with the topology config for this
    /// update only (never remembered)
    #[arg(long)]
    config_replace: bool,
    /// Merge precedence when the profile and topology set the same key:
    /// profile (default) or topology; requires --config-merge
    #[arg(long, value_name = "WHEN", requires = "config_merge")]
    config_precedence: Option<String>,
    /// Plan only: ephemeral checkout, no registry/cache/target writes
    #[arg(long)]
    dry_run: bool,
    /// Emit exactly one JSON document on stdout
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct SelfUpdateArgs {
    /// Release version to install (default: latest)
    #[arg(long, value_name = "VERSION")]
    version: Option<String>,
    /// Resolve the release and print the plan without downloading or changing files
    #[arg(long)]
    dry_run: bool,
    /// Emit exactly one JSON document on stdout
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct DoctorArgs {
    /// Diagnose one registered install ID (mutually exclusive with profile and agent directory)
    #[arg(
        long = "install-id",
        conflicts_with_all = ["profile", "agent_dir"],
        add = clap_complete::engine::ArgValueCompleter::new(completions::complete_install_ids)
    )]
    install_id: Option<String>,
    /// Diagnose one OMP profile (mutually exclusive with install ID and agent directory)
    #[arg(
        long,
        conflicts_with_all = ["install_id", "agent_dir"],
        add = clap_complete::engine::ArgValueCompleter::new(completions::complete_profiles)
    )]
    profile: Option<String>,
    /// Diagnose one explicit agent directory
    #[arg(long, conflicts_with_all = ["install_id", "profile"], value_hint = clap::ValueHint::DirPath)]
    agent_dir: Option<String>,
    /// Emit exactly one JSON document on stdout
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct CompletionsArgs {
    /// Shell to generate for
    #[arg(value_parser = ["bash", "fish", "zsh"])]
    shell: String,
}

#[derive(Args)]
struct InternalCompletionArgs {
    /// Current command line words, after `--`
    #[arg(last = true, num_args = 0.., allow_hyphen_values = true)]
    words: Vec<OsString>,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::List(args) => cmd_list(&args),
        Command::Install(args) => cmd_install(&args),
        Command::Remove(args) => cmd_remove(&args),
        Command::Profiles(args) => cmd_profiles(&args),
        Command::Status(args) => cmd_status(&args),
        Command::Update(args) => cmd_update(&args),
        Command::SelfUpdate(args) => cmd_self_update(&args),
        Command::Doctor(args) => cmd_doctor(&args),
        Command::Completions(args) => cmd_completions(&args),
        Command::Complete(args) => cmd_complete(args),
    };
    match result {
        Ok(code) => std::process::ExitCode::from(code as u8),
        Err(err) => {
            eprintln!("error: {err}");
            std::process::ExitCode::from(1)
        }
    }
}

fn cmd_doctor(args: &DoctorArgs) -> Result<i32, CliError> {
    let anchor = state_anchor()?;
    doctor::run(
        args.install_id.as_deref(),
        args.profile.as_deref(),
        args.agent_dir.as_deref(),
        args.json,
        &anchor,
    )
}

fn cmd_completions(args: &CompletionsArgs) -> Result<i32, CliError> {
    let script = completions::script(&args.shell)
        .ok_or_else(|| CliError::Usage(format!("unsupported shell '{}'", args.shell)))?;
    print!("{script}");
    Ok(0)
}

fn cmd_complete(args: InternalCompletionArgs) -> Result<i32, CliError> {
    completions::complete(args.words);
    Ok(0)
}

/// Build the per-command config mode from the CLI flag combination.
/// Modes are never remembered: update and remove derive their behavior
/// from the ownership manifest instead.
fn config_mode_from_flags(
    merge: bool,
    replace: bool,
    precedence: Option<&str>,
) -> Result<config_mode::ConfigMode, CliError> {
    if replace {
        return Ok(config_mode::ConfigMode::Replace);
    }
    if merge {
        let precedence = match precedence {
            Some(text) => text
                .parse::<config_mode::MergePrecedence>()
                .map_err(CliError::Usage)?,
            None => config_mode::MergePrecedence::Profile,
        };
        return Ok(config_mode::ConfigMode::Merge { precedence });
    }
    if precedence.is_some() {
        return Err(CliError::Usage(
            "--config-precedence requires --config-merge".to_string(),
        ));
    }
    Ok(config_mode::ConfigMode::Candidate)
}

/// The stable state anchor for registry and manifest paths: the installer
/// checkout when available, otherwise the executable's parent directory
/// (an installed binary has no checkout).
pub(crate) fn state_anchor() -> Result<PathBuf, CliError> {
    let manifest_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    if manifest_root.join("Cargo.toml").is_file() {
        return Ok(manifest_root.to_path_buf());
    }

    let exe = std::env::current_exe()
        .map_err(|err| crate::error::io_err(Path::new("current executable"), err))?;
    if let Some(root) = exe.ancestors().find(|candidate| candidate.join("Cargo.toml").is_file()) {
        return Ok(root.to_path_buf());
    }

    let parent = exe.parent().ok_or_else(|| {
        CliError::Usage(
            "could not determine the executable directory for installer state; \
             set XDG_STATE_HOME to a private location"
                .to_string(),
        )
    })?;
    Ok(parent.to_path_buf())
}

/// A topology source for a command. Explicit checkouts stay alive in this
/// value until the command returns (ephemeral staging is removed on drop).
enum ResolvedSource {
    Explicit(SourceCheckout),
}

impl ResolvedSource {
    /// Root directory handed to `topology::list_topologies` / `install::run`.
    fn tree_root(&self) -> &Path {
        match self {
            ResolvedSource::Explicit(checkout) => &checkout.root,
        }
    }

    /// What the registry records as the install source.
    fn source_spec(&self) -> SourceSpec {
        match self {
            ResolvedSource::Explicit(checkout) => match &checkout.source {
                TopologySource::GitHub { owner, repo } => SourceSpec::GitHub {
                    owner: owner.clone(),
                    repo: repo.clone(),
                },
                TopologySource::Local { path } => SourceSpec::Local { path: path.clone() },
            },
        }
    }

    /// Pinned commit for GitHub sources; `None` for local sources.
    fn commit(&self) -> Option<&str> {
        match self {
            ResolvedSource::Explicit(checkout) => checkout.commit.as_deref(),
        }
    }
}

/// Map the registry's recorded source spec onto the resolver's source type;
/// both are the same `github:`/local pair of shapes.
fn topology_source(spec: &SourceSpec) -> TopologySource {
    match spec {
        SourceSpec::GitHub { owner, repo } => TopologySource::GitHub {
            owner: owner.clone(),
            repo: repo.clone(),
        },
        SourceSpec::Local { path } => TopologySource::Local { path: path.clone() },
    }
}

/// Resolve the explicitly selected topology source.
fn resolve_source(
    source: &Option<String>,
    reference: Option<&str>,
    cache_root: Option<&Path>,
) -> Result<ResolvedSource, CliError> {
    let spec = source.as_deref().ok_or_else(|| {
        CliError::Usage("--source is required; provide a local path or github:OWNER/REPO".into())
    })?;
    Ok(ResolvedSource::Explicit(source::resolve(
        spec, reference, cache_root,
    )?))
}

/// Print exactly one JSON document on stdout.
fn print_json(doc: &Json) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(doc)
        .map_err(|err| CliError::Usage(format!("could not serialize JSON output: {err}")))?;
    println!("{text}");
    Ok(())
}

/// Finish an install/remove/update command: one JSON document in JSON mode,
/// the captured lines unchanged in human mode.
fn finish(command: &str, json: bool, out: Vec<String>, code: i32) -> Result<i32, CliError> {
    if json {
        print_json(&json!({ "command": command, "lines": out }))?;
    } else {
        for line in &out {
            println!("{line}");
        }
    }
    Ok(code)
}

/// Preserve captured command lines when a command fails after doing work:
/// stdout in human mode, stderr in JSON mode so stdout stays free of stray
/// output (JSON mode may have emitted nothing yet or must stay clean).
fn preserve_lines(json: bool, out: &[String]) {
    for line in out {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
}

/// Build the error for a registry-metadata failure that happens *after*
/// the target files were already mutated: the captured success lines are
/// preserved, and the message states plainly that the target changed, the
/// registry did not, and how to recover — never implying a rollback
/// happened.
fn registry_commit_failed(
    command: &str,
    json: bool,
    out: &[String],
    exc: CliError,
    anchor: &Path,
    recovery: &str,
) -> CliError {
    preserve_lines(json, out);
    let registry_path = registry::registry_path(anchor)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "the state root".to_string());
    CliError::Registry(format!(
        "{command} changed the target, but registry reconciliation failed: \
         {exc}\nthe registry at {registry_path} is now out of date; {recovery}"
    ))
}

/// The default install ID: the profile name, or the lowercased basename
/// of the agent directory for explicit/ambient agent-dir targets.
fn default_install_id(target: &profiles::ResolvedTarget) -> Result<String, CliError> {
    if let Some(profile) = &target.profile {
        // Registry install IDs accept only the lowercase slug grammar.
        return Ok(profile.to_ascii_lowercase());
    }
    let basename = target
        .agent_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            CliError::Usage(format!(
                "cannot derive a default install ID from agent directory '{}'; \
                 pass --install-id NAME explicitly",
                target.agent_dir.display()
            ))
        })?;
    Ok(basename.to_ascii_lowercase())
}

/// Which ownership manifest an install under `install_id` owns at
/// `agent_dir`: an already-registered install ID keeps its recorded
/// slot; a new install ID owns the legacy per-agent manifest only when
/// no other install targets the directory, else its own per-install
/// manifest, so two installs never share one ownership record.
fn manifest_slot_for(
    registry_doc: &registry::Registry,
    install_id: &str,
    agent_dir: &Path,
) -> state::ManifestSlot {
    if let Some(existing) = registry_doc.get(install_id) {
        return existing.manifest_slot;
    }
    if registry_doc
        .aliases
        .values()
        .any(|entry| entry.agent_dir == agent_dir)
    {
        state::ManifestSlot::PerAlias
    } else {
        state::ManifestSlot::Legacy
    }
}

fn cmd_list(args: &ListArgs) -> Result<i32, CliError> {
    // An explicit source is resolved ephemerally: no persistent cache.
    let resolved = resolve_source(&args.source, args.reference.as_deref(), None)?;
    let topologies = topology::list_topologies(resolved.tree_root())?;
    if args.json {
        let topologies = Json::Array(
            topologies
                .iter()
                .map(|(name, version, description)| {
                    json!({ "name": name, "version": version, "description": description })
                })
                .collect(),
        );
        print_json(&json!({ "command": "list", "topologies": topologies }))?;
    } else {
        for (name, version, description) in &topologies {
            println!("{name} {version}");
            if !description.is_empty() {
                println!("    {description}");
            }
        }
    }
    Ok(0)
}

/// Bounded cache cleanup for one GitHub source involved in an install,
/// retaining every commit still referenced by the registry plus any
/// commits in `extra_retained` — e.g. the just-resolved checkout when the
/// registry mutation failed or never ran, so cleanup never deletes the
/// checkout needed for recovery. The unreferenced allowance shrinks by
/// the extra commits (saturating at zero) so the cache never retains
/// more than CACHE_KEEP_UNREFERENCED unreferenced checkouts in total.
/// Failures are reported as warnings in
/// `out`, through the same human/JSON output path as the other captured
/// lines, never as a command failure.
fn prune_install_source(
    anchor: &Path,
    source: &SourceSpec,
    extra_retained: &BTreeSet<String>,
    out: &mut Vec<String>,
) {
    let registry_referenced: BTreeSet<String> = match registry::load_registry(anchor) {
        Ok(doc) => doc
            .aliases
            .values()
            .filter(|other| other.source == *source)
            .filter_map(|other| other.resolved_commit.clone())
            .collect(),
        Err(exc) => {
            out.push(format!(
                "warning: could not reload the registry to prune the source cache: {exc}"
            ));
            return;
        }
    };
    let mut referenced = registry_referenced.clone();
    referenced.extend(extra_retained.iter().cloned());
    // `extra_retained` checkouts are unreferenced by the registry, so they
    // count against the unreferenced bound: shrink the allowance by the
    // ones not already registry-referenced (saturating at zero) so the
    // cache never retains more than CACHE_KEEP_UNREFERENCED unreferenced
    // checkouts in total.
    let extra_unreferenced = extra_retained
        .iter()
        .filter(|commit| !registry_referenced.contains(*commit))
        .count();
    let keep_unreferenced = CACHE_KEEP_UNREFERENCED.saturating_sub(extra_unreferenced);
    let cache_root = match state::state_root(anchor) {
        Ok(root) => root.join("sources"),
        Err(exc) => {
            out.push(format!(
                "warning: could not resolve source cache root: {exc}"
            ));
            return;
        }
    };
    if let Err(exc) = source::prune_cache(
        &cache_root,
        &topology_source(source),
        &referenced,
        keep_unreferenced,
    ) {
        out.push(format!("warning: could not prune source cache: {exc}"));
    }
}

/// Bound the source cache after an install whose registry entry did not
/// land (an `install::run` failure or a registry-commit failure): the
/// persistent GitHub checkout is already on disk, so repeated failures
/// would otherwise grow the cache without bound. The resolved checkout
/// commit is retained even though the registry did not record it, so the
/// checkout needed for recovery is never pruned. No-op for dry runs
/// (ephemeral checkout) and local sources (no cache).
fn prune_after_failed_install(
    anchor: &Path,
    source_spec: &SourceSpec,
    commit: Option<&str>,
    dry_run: bool,
    out: &mut Vec<String>,
) {
    if dry_run || !matches!(source_spec, SourceSpec::GitHub { .. }) {
        return;
    }
    let mut extra_retained = BTreeSet::new();
    if let Some(commit) = commit {
        extra_retained.insert(commit.to_string());
    }
    prune_install_source(anchor, source_spec, &extra_retained, out);
}

/// Parse a `--version` flag as strict SemVer so an invalid value is
/// rejected by clap, before any source resolution or state/target access,
/// with a diagnostic that names the bad value.
fn parse_semver(raw: &str) -> Result<semver::Version, String> {
    semver::Version::parse(raw).map_err(|err| {
        format!("'{raw}' is not valid SemVer ({err}); pass an exact version like '1.2.3'")
    })
}

/// Find an executable regular file named exactly like the requested shell
/// `alias` on PATH without spawning a process: a launcher with the shell
/// alias's name would shadow (or be shadowed by) the alias the user just
/// chose. Symlinks resolving to an executable regular file count;
/// directories, non-executable files, unreadable candidates, and an
/// absent PATH do not. The first match in PATH order is returned.
#[cfg(unix)]
fn path_executable_collision(alias: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(alias);
        // `metadata` follows symlinks, so a link to an executable regular
        // file collides too; dangling links and unreadable entries fail and
        // are ignored. Executability is decided by permission bits, the
        // supported targets being Linux and macOS.
        let Ok(metadata) = std::fs::metadata(&candidate) else {
            continue;
        };
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(unix))]
fn path_executable_collision(_alias: &str) -> Option<PathBuf> {
    None
}

/// The refusal for a requested shell alias whose name is already an
/// executable on PATH.
fn path_collision_error(alias: &str, colliding: &Path) -> CliError {
    CliError::Usage(format!(
        "alias '{alias}' collides with the executable '{}' already on \
         PATH; choose another --alias (for example 'omp-{alias}')",
        colliding.display()
    ))
}

/// Enrich a refused install with recovery guidance when a collision
/// diagnostic names an owner: the collision error carries the install IDs
/// recorded as owning the colliding files, and the guidance names the
/// first listed owner whose registry entry targets the same agent
/// directory under a different topology. It is a no-op for every other
/// error — unrelated usage failures (symlinked skills roots, config
/// refusals) and unowned collisions pass through untouched — and when
/// no listed owner is registered at the target under a different
/// topology.
fn cross_topology_guidance(
    exc: CliError,
    registry_doc: &registry::Registry,
    install_id: &str,
    topology: &str,
    agent_dir: &Path,
) -> CliError {
    let (message, owners) = match exc {
        CliError::InstallCollision { message, owners } => (message, owners),
        other => return other,
    };
    // The owners come from the collision diagnostics themselves, so the
    // guidance can never name an install ID that owns none of the
    // colliding files. An owner whose registration changed (different
    // target, or the same topology the user requested) is not a recovery
    // anchor.
    let owner = owners.iter().find_map(|owner_install_id| {
        let entry = registry_doc.get(owner_install_id)?;
        (owner_install_id.as_str() != install_id
            && entry.agent_dir == agent_dir
            && entry.topology != topology)
            .then_some((owner_install_id, entry))
    });
    let Some((owner_install_id, owner_entry)) = owner else {
        return CliError::Usage(message);
    };
    CliError::Usage(format!(
        "{message}\n\
         guidance: '{owner_topology}' is already installed at {target} \
         under install ID '{owner_install_id}'; the requested topology is \
         '{topology}'.\n\
         - To keep both topologies, install '{topology}' into a \
         separate target instead: pass --profile {topology} (or \
         --agent-dir PATH) so the two installs do not share files.\n\
         - To update the existing '{owner_topology}' installation, \
         run: omp-profile update --install-id {owner_install_id}. This \
         keeps '{owner_topology}'; it does not install '{topology}'.",
        owner_topology = owner_entry.topology,
        target = agent_dir.display(),
        owner_install_id = owner_install_id,
        topology = topology,
    ))
}

fn cmd_install(args: &InstallArgs) -> Result<i32, CliError> {
    let anchor = state_anchor()?;
    // Validate values derivable without resolving an OMP profile first:
    // `omp --profile NAME config path` can create a missing profile dir.
    if let Some(install_id) = args.install_id.as_deref() {
        registry::validate_alias(install_id)?;
    } else if let Some(profile) = args.profile.as_deref() {
        registry::validate_alias(&profile.to_ascii_lowercase())?;
    }
    let registry_doc = registry::load_registry(&anchor)?;
    // --profile and --agent-dir are mutually exclusive (checked inside);
    // a profile-less invocation targets the ambient profile. A dry run is
    // read-only, so its target resolution must not create a missing
    // profile directory the way `omp --profile NAME config path` does.
    let target_opts = profiles::TargetOptions {
        profile: args.profile.as_deref(),
        agent_dir: args.agent_dir.as_deref(),
    };
    // The requested shell alias must not collide with an executable
    // already on PATH: refuse before source resolution, cache creation,
    // or any state or target mutation. The registry install ID is never
    // PATH-preflighted; only the executable name the shell alias would
    // shadow is checked. The single allowed collision is a
    // same-install-ID, same-target, same-topology upsert reusing its own
    // shell alias: the executable on PATH is the launcher that install
    // created, so the reinstall must not refuse itself. --alias requires
    // --profile, so the candidate install ID is derivable without
    // resolving the target, and a registry miss or topology mismatch
    // refuses without invoking omp at all; only a registered
    // same-topology candidate probes the target, and the probe is the
    // read-only resolution — never the mutating `omp --profile NAME
    // config path` — so every refusal here stays side-effect free. A dry
    // run reuses the probed target instead of resolving twice.
    let mut alias_probe = None;
    if let Some(shell_alias) = args.alias.as_deref()
        && let Some(colliding) = path_executable_collision(shell_alias)
    {
        let candidate_install_id = match &args.install_id {
            Some(install_id) => Some(install_id.clone()),
            None => args
                .profile
                .as_deref()
                .map(|profile| profile.to_ascii_lowercase()),
        };
        let is_upsert = match candidate_install_id
            .as_deref()
            .and_then(|install_id| registry_doc.get(install_id))
        {
            Some(existing) if existing.topology == args.topology => {
                let probe = profiles::resolve_target_read_only(&target_opts)?;
                if probe.agent_dir == existing.agent_dir {
                    alias_probe = Some(probe);
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if !is_upsert {
            return Err(path_collision_error(shell_alias, &colliding));
        }
    }
    let target = if args.dry_run {
        match alias_probe {
            Some(probe) => probe,
            None => profiles::resolve_target_read_only(&target_opts)?,
        }
    } else {
        profiles::resolve_target(&target_opts)?
    };
    let install_id = match &args.install_id {
        Some(install_id) => install_id.clone(),
        None => default_install_id(&target)?,
    };
    // Validate the install ID and settle conflicts before any side
    // effect: a reinstall under the same install ID for the same target
    // is an upsert; an install ID that maps elsewhere is refused. Several
    // install IDs may share one agent directory: the first install ID
    // owns the legacy per-agent manifest.json, every additional install
    // owns its per-install manifests/<id>.json, so ownership state is
    // never shared between installs.
    registry::validate_alias(&install_id)?;
    if let Some(existing) = registry_doc.get(&install_id)
        && (existing.agent_dir != target.agent_dir || existing.topology != args.topology)
    {
        return Err(CliError::Usage(format!(
            "install ID '{install_id}' is already registered for '{}' in {}; it \
             cannot be reused for '{}' in {}. To update the existing \
             install, rerun install with its registered topology and \
             target; otherwise choose another --install-id",
            existing.topology,
            existing.agent_dir.display(),
            args.topology,
            target.agent_dir.display()
        )));
    }
    let manifest_slot = manifest_slot_for(&registry_doc, &install_id, &target.agent_dir);

    // Real installs from an explicit source cache persistently; dry runs
    // clone into a temporary directory that SourceCheckout removes on drop.
    let cache_root = if args.dry_run {
        None
    } else {
        Some(state::state_root(&anchor)?.join("sources"))
    };
    let resolved = resolve_source(
        &args.source,
        args.reference.as_deref(),
        cache_root.as_deref(),
    )?;

    // Record what the registry will persist, and read the local overlay
    // digest *before* mutating the target: a source read failure must not
    // surface only after install::run has already changed target files
    // and leave an unregistered install behind.
    let source_spec = resolved.source_spec();
    let overlay_digest = if matches!(source_spec, SourceSpec::Local { .. }) {
        let topology_dir = topology::find_topology(resolved.tree_root(), &args.topology, true)?;
        Some(source::overlay_digest(&topology_dir)?)
    } else {
        None
    };

    let agent_dir_text = target.agent_dir.to_string_lossy().into_owned();
    let manifest_path =
        state::manifest_path_for_slot(&target.agent_dir, &anchor, &install_id, manifest_slot)?;
    // Config handling for this command only; never remembered.
    let config_mode = config_mode_from_flags(
        args.config_merge,
        args.config_replace,
        args.config_precedence.as_deref(),
    )?;
    // Refuse incompatible config types before taking the registry lock:
    // acquiring the lock creates the state home, and a refused install
    // must leave no side effects behind. `install::run` repeats this
    // check under the lock before mutating, so a config edited in
    // between still fails closed.
    if let Err(exc) =
        install::preflight_config_types(resolved.tree_root(), &args.topology, Some(&agent_dir_text))
    {
        // Same cache bounding as the `install::run` error path below: a
        // GitHub checkout may already be persistent in the source cache.
        let mut out = Vec::new();
        prune_after_failed_install(
            &anchor,
            &source_spec,
            resolved.commit(),
            args.dry_run,
            &mut out,
        );
        preserve_lines(args.json, &out);
        return Err(exc);
    }
    // Refuse unsupported symlink topology on the install targets before
    // taking the registry lock, for the same no-side-effects reason as
    // the config preflight above. `install::run` re-resolves the links
    // under the lock before mutating.
    if let Err(exc) = install::preflight_target_links(
        resolved.tree_root(),
        &args.topology,
        Some(&agent_dir_text),
        args.skills_dir.as_deref(),
        &anchor,
        Some(&manifest_path),
    ) {
        let mut out = Vec::new();
        prune_after_failed_install(
            &anchor,
            &source_spec,
            resolved.commit(),
            args.dry_run,
            &mut out,
        );
        preserve_lines(args.json, &out);
        return Err(cross_topology_guidance(
            exc,
            &registry_doc,
            &install_id,
            &args.topology,
            &target.agent_dir,
        ));
    }
    // Refuse an invalid declared SemVer and a requested-version mismatch
    // before taking the registry lock: acquiring the lock creates the
    // state home, and a refused install must leave no side effects
    // behind. `install::run` repeats this check under the lock before
    // mutating, so a source edited in between still fails closed.
    let topology_dir = topology::find_topology(resolved.tree_root(), &args.topology, true)?;
    let topology_meta = topology::topology_metadata(&topology_dir)?;
    if let Err(exc) = install::validate_declared_version(
        &args.topology,
        &topology_dir.join("topology.yml"),
        &topology::meta_version(&topology_meta),
        args.version.as_ref(),
    ) {
        let mut out = Vec::new();
        prune_after_failed_install(
            &anchor,
            &source_spec,
            resolved.commit(),
            args.dry_run,
            &mut out,
        );
        preserve_lines(args.json, &out);
        return Err(exc);
    }
    // Hold the registry lock across the target mutation and the registry
    // commit so a concurrent update/remove cannot interleave with this
    // install — the same serialization update uses.
    let _lock = if args.dry_run {
        None
    } else {
        Some(registry::RegistryLock::acquire(&anchor)?)
    };
    // Re-decide the ownership slot under the lock: the preflight snapshot
    // may predate a concurrent registration, and the slot decides every
    // ownership-manifest read/write below. The locked registry document
    // is retained through `install::run` so run-error recovery guidance
    // resolves collision owners against the same snapshot the install
    // ran against; dry runs hold no lock and fall back to the pre-lock
    // registry.
    let (locked_doc, manifest_slot, manifest_path) = if _lock.is_some() {
        let locked_doc = registry::load_registry(&anchor)?;
        if let Some(existing) = locked_doc.get(&install_id)
            && (existing.agent_dir != target.agent_dir || existing.topology != args.topology)
        {
            let exc = CliError::Usage(format!(
                "install ID '{install_id}' is already registered for '{}' in {}; it \
                 cannot be reused for '{}' in {}. To update the existing \
                 install, rerun install with its registered topology and \
                 target; otherwise choose another --install-id",
                existing.topology,
                existing.agent_dir.display(),
                args.topology,
                target.agent_dir.display()
            ));
            let mut out = Vec::new();
            prune_after_failed_install(
                &anchor,
                &source_spec,
                resolved.commit(),
                args.dry_run,
                &mut out,
            );
            preserve_lines(args.json, &out);
            return Err(exc);
        }
        let slot = manifest_slot_for(&locked_doc, &install_id, &target.agent_dir);
        let path = state::manifest_path_for_slot(&target.agent_dir, &anchor, &install_id, slot)?;
        (Some(locked_doc), slot, path)
    } else {
        (None, manifest_slot, manifest_path)
    };
    let mut out = Vec::new();
    let code = match install::run(
        &install::InstallOptions {
            topology: &args.topology,
            topology_root: resolved.tree_root(),
            state_anchor: &anchor,
            agent_dir: Some(&agent_dir_text),
            skills_dir: args.skills_dir.as_deref(),
            dry_run: args.dry_run,
            config_mode,
            manifest_path: Some(&manifest_path),
            install_id: Some(&install_id),
            expected_version: args.version.as_ref(),
        },
        &mut out,
    ) {
        Ok(code) => code,
        Err(exc) => {
            // The persistent GitHub checkout is already on disk; bound the
            // cache even though the install failed, retaining the resolved
            // commit so the checkout needed for recovery is not pruned.
            prune_after_failed_install(
                &anchor,
                &source_spec,
                resolved.commit(),
                args.dry_run,
                &mut out,
            );
            preserve_lines(args.json, &out);
            // `run` repeats the collision detection under the lock, so a
            // target changed in between still fails closed here; enrich
            // that race refusal with recovery guidance resolved against
            // the locked registry snapshot the install ran against.
            return Err(cross_topology_guidance(
                exc,
                locked_doc.as_ref().unwrap_or(&registry_doc),
                &install_id,
                &args.topology,
                &target.agent_dir,
            ));
        }
    };
    if code != 0 {
        // Same cleanup as the error path above: a failed install can still
        // leave the freshly fetched persistent checkout behind.
        prune_after_failed_install(
            &anchor,
            &source_spec,
            resolved.commit(),
            args.dry_run,
            &mut out,
        );
        return finish("install", args.json, out, code);
    }
    if args.dry_run {
        out.push(format!(
            "dry-run: would install '{}' into {} as install ID '{install_id}'",
            args.topology,
            target.agent_dir.display()
        ));
        // The shell alias is handed to the real omp only on a real
        // install; the dry-run plan reports it without invoking omp or
        // touching any shell configuration.
        if let Some(profile) = args.profile.as_deref()
            && let Some(shell_alias) = args.alias.as_deref()
        {
            out.push(format!(
                "Would create OMP shell alias '{shell_alias}' for profile '{profile}'"
            ));
        }
        return finish("install", args.json, out, 0);
    }

    // The install landed: persist the source pin (GitHub commit) or the
    // precomputed local overlay digest, the profile, and the absolute
    // target. A failure inside `with_registry` leaves the target mutated
    // but the registry unrecorded, so it routes through
    // `registry_commit_failed`.
    let mut entry = RegistryEntry::new(
        target.agent_dir.clone(),
        target.profile.clone(),
        args.topology.clone(),
        source_spec,
        args.reference.clone(),
        resolved.commit().map(str::to_string),
        overlay_digest,
    );
    entry.manifest_slot = manifest_slot;

    // Re-registering an unchanged managed target must not churn installer
    // state: the entry inserted below starts as a fresh stamp, but the
    // original registration time survives when the install ID already maps
    // to this same agent directory and topology, so an unchanged reinstall
    // serializes identical registry bytes (the state snapshot the installer
    // keeps is byte-compared across reinstalls).
    let mut entry_for_merge = entry.clone();
    // A reinstall may move the install ID from source A to source B; both
    // caches are pruned after the commit, each retaining the commits the
    // post-insert registry still references.
    let mut sources_to_prune: Vec<SourceSpec> = Vec::new();
    // The registry lock is held from before install::run, so this merge is
    // a plain load/insert/write (write_registry documents the lock as a
    // precondition) instead of with_registry acquiring a second one.
    let merge_result = (|| -> Result<(), CliError> {
        let mut doc = registry::load_registry(&anchor)?;
        let old_source = doc.get(&install_id).map(|existing| existing.source.clone());
        if let Some(existing) = doc.get(&install_id) {
            if existing.agent_dir != target.agent_dir || existing.topology != args.topology {
                return Err(CliError::Registry(format!(
                    "install ID '{install_id}' changed concurrently (now '{}' \
                     in {}); refusing to overwrite it",
                    existing.topology,
                    existing.agent_dir.display()
                )));
            }
            entry_for_merge.installed_at = existing.installed_at.clone();
        }
        doc.insert(install_id.clone(), entry_for_merge)?;
        for source in old_source.into_iter().chain([entry.source.clone()]) {
            if matches!(source, SourceSpec::GitHub { .. }) && !sources_to_prune.contains(&source) {
                sources_to_prune.push(source);
            }
        }
        registry::write_registry(&anchor, &doc)
    })();
    if let Err(exc) = merge_result {
        // The target changed but the registry did not record the new pin;
        // bound the cache for the new source, retaining the resolved
        // checkout commit so recovery is not pruned away. The registry
        // still references every commit it recorded, so those stay too.
        if matches!(entry.source, SourceSpec::GitHub { .. }) {
            let mut retain = BTreeSet::new();
            if let Some(commit) = resolved.commit() {
                retain.insert(commit.to_string());
            }
            prune_install_source(&anchor, &entry.source, &retain, &mut out);
        }
        let recovery = format!(
            "re-run 'install -t {} --agent-dir {}' once the registry is \
             writable; the reinstall is recorded as an upsert",
            args.topology,
            target.agent_dir.display()
        );
        return Err(registry_commit_failed(
            "install", args.json, &out, exc, &anchor, &recovery,
        ));
    }

    out.push(format!(
        "recorded install ID '{install_id}' for '{}' in {}",
        args.topology,
        target.agent_dir.display()
    ));

    // Bounded cache cleanup after the registry mutation, per GitHub
    // source involved in the install, retaining every commit still
    // referenced by the post-insert registry. Run this before optional
    // shell-alias delegation so an OMP alias failure cannot skip cleanup;
    // a prune failure is a warning, never an install failure.
    for source in sources_to_prune {
        prune_install_source(&anchor, &source, &BTreeSet::new(), &mut out);
    }

    // The topology is installed and the registry commit succeeded, so
    // the shell alias is handed to the real omp — it owns its own rc-file
    // syntax, which this installer never writes. Only an explicit
    // --profile together with an explicit --alias delegates: the shell
    // alias is independent of the registry install ID, and installs
    // without --alias keep their registry-only semantics. Unreachable on
    // dry runs, which returned before the registry commit above.
    if let Some(profile) = args.profile.as_deref()
        && let Some(shell_alias) = args.alias.as_deref()
        && let Err(exc) = profiles::create_profile_alias(profile, shell_alias)
    {
        // The install and the registry commit succeeded and are not
        // rolled back; preserve the accumulated human lines (stderr in
        // JSON mode, so no OMP stdout can pollute the JSON document) and
        // name the exact manual recovery command.
        preserve_lines(args.json, &out);
        return Err(CliError::Usage(format!(
            "installed '{}' into {} as install ID '{install_id}' and recorded it \
             in the registry, but the shell alias could not be created: {exc}\n\
             the topology install is complete; recover by running \
             'omp --profile {profile} --alias {shell_alias}' manually",
            args.topology,
            target.agent_dir.display(),
        )));
    }

    finish("install", args.json, out, 0)
}

fn cmd_remove(args: &RemoveArgs) -> Result<i32, CliError> {
    let anchor = state_anchor()?;
    if args.install_id.is_some() && (args.profile.is_some() || args.agent_dir.is_some()) {
        return Err(CliError::Usage(
            "--install-id cannot be combined with --profile or --agent-dir: \
             the install ID already selects the target and topology"
                .to_string(),
        ));
    }

    // Read-only registry lookup to select the removal target.
    let registry_doc = registry::load_registry(&anchor)?;
    let (topology, agent_dir, matching_install_ids): (String, PathBuf, Vec<String>) =
        if let Some(install_id) = &args.install_id {
            let entry = registry_doc.get(install_id).ok_or_else(|| {
                CliError::Usage(format!(
                    "unknown install ID '{install_id}' (see the registry under {})",
                    registry::registry_path(&anchor)
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|_| "the state root".to_string())
                ))
            })?;
            if let Some(topology) = &args.topology
                && topology != &entry.topology
            {
                return Err(CliError::Usage(format!(
                    "topology '{topology}' does not match install ID \
                     '{install_id}' (installed topology '{}'); pass -t {} or \
                     drop --install-id",
                    entry.topology, entry.topology
                )));
            }
            (
                entry.topology.clone(),
                entry.agent_dir.clone(),
                vec![install_id.clone()],
            )
        } else {
            let Some(topology) = &args.topology else {
                return Err(CliError::Usage(
                    "remove needs a topology: pass -t NAME, or select an \
                     install with --install-id NAME"
                        .to_string(),
                ));
            };
            // A dry run is read-only, so its target resolution must not
            // create a missing profile directory the way `omp --profile
            // NAME config path` does.
            let target_opts = profiles::TargetOptions {
                profile: args.profile.as_deref(),
                agent_dir: args.agent_dir.as_deref(),
            };
            let target = if args.dry_run {
                profiles::resolve_target_read_only(&target_opts)?
            } else {
                profiles::resolve_target(&target_opts)?
            };
            // Direct target removal reconciles any matching registry entry.
            let matching: Vec<String> = registry_doc
                .aliases
                .iter()
                .filter(|(_, entry)| {
                    entry.agent_dir == target.agent_dir && entry.topology == *topology
                })
                .map(|(install_id, _)| install_id.clone())
                .collect();
            (topology.clone(), target.agent_dir.clone(), matching)
        };

    // Resolve which ownership manifest this removal operates on: an
    // explicit install ID uses its recorded slot; a direct target with
    // exactly one matching install ID uses that install's slot; a direct
    // target with several matching install IDs is ambiguous and must name
    // one; no matching install falls back to the legacy per-agent
    // manifest. The resolved install ID also scopes recovery-marker
    // cleanup so a sibling install's retained rollback snapshot is never
    // cleared.
    let (manifest_path, scope_install_id): (PathBuf, Option<&str>) = if let Some(install_id) =
        &args.install_id
    {
        let entry = registry_doc.get(install_id).expect("validated above");
        (
            state::manifest_path_for_slot(&agent_dir, &anchor, install_id, entry.manifest_slot)?,
            Some(install_id.as_str()),
        )
    } else {
        match matching_install_ids.len() {
            0 => (state::manifest_path(&agent_dir, &anchor)?, None),
            1 => {
                let install_id = &matching_install_ids[0];
                let entry = registry_doc.get(install_id).expect("collected above");
                (
                    state::manifest_path_for_slot(
                        &agent_dir,
                        &anchor,
                        install_id,
                        entry.manifest_slot,
                    )?,
                    Some(install_id.as_str()),
                )
            }
            _ => {
                return Err(CliError::Usage(format!(
                    "multiple install IDs ({}) target {} for topology \
                         '{topology}'; pass --install-id NAME to select one",
                    matching_install_ids.join(", "),
                    agent_dir.display()
                )));
            }
        }
    };

    // Stale install ID: the registry still records the install but the
    // target has no ownership manifest (e.g. a previous removal succeeded
    // while its registry write failed). Reconcile the registry directly
    // and never touch target files; remove::run would refuse here, and
    // dry-run stays read-only.
    if args.install_id.is_some() {
        match state::load_manifest_at(&manifest_path, &anchor) {
            Ok(None) => {
                let mut out = Vec::new();
                if args.dry_run {
                    out.push(format!(
                        "dry-run: install ID '{}' has no ownership manifest in {}; \
                         would remove the stale registry entry only",
                        args.install_id.as_deref().expect("checked above"),
                        agent_dir.display()
                    ));
                    return finish("remove", args.json, out, 0);
                }
                out.push(format!(
                    "install ID '{}' has no ownership manifest in {}; removing \
                     the stale registry entry only",
                    args.install_id.as_deref().expect("checked above"),
                    agent_dir.display()
                ));
                // Reconcile under the registry lock: the manifest absence
                // observed above is re-checked there, so a concurrent
                // reinstall that recreated the manifest keeps the entry
                // (marked Partial) instead of being unregistered.
                let reconciliation = reconcile_removal(
                    &anchor,
                    &matching_install_ids,
                    false,
                    RemovalScope {
                        topology: &topology,
                        agent_dir: &agent_dir,
                        manifest_path: &manifest_path,
                    },
                    args.json,
                    &out,
                )?;
                if !reconciliation.retained_aliases.is_empty() {
                    out.push(format!(
                        "note: a concurrent install recreated the ownership \
                         manifest in {}; install ID '{}' was retained as \
                         partial instead of unregistered",
                        agent_dir.display(),
                        args.install_id.as_deref().expect("checked above"),
                    ));
                }
                prune_removed_sources(&anchor, reconciliation.pruned_sources, &mut out)?;
                return finish("remove", args.json, out, 0);
            }
            Ok(Some(_)) => {}
            Err(exc) => return Err(exc),
        }
    }

    let agent_dir_text = agent_dir.to_string_lossy().into_owned();
    let mut out = Vec::new();
    let code = match remove::run(
        &remove::RemoveOptions {
            topology: &topology,
            state_anchor: &anchor,
            agent_dir: Some(&agent_dir_text),
            skills_dir: args.skills_dir.as_deref(),
            dry_run: args.dry_run,
            manifest_path: Some(&manifest_path),
            install_id: scope_install_id,
        },
        &mut out,
    ) {
        Ok(code) => code,
        Err(exc) => {
            preserve_lines(args.json, &out);
            return Err(exc);
        }
    };
    if code != 0 {
        return finish("remove", args.json, out, code);
    }
    if args.dry_run {
        out.push(format!(
            "dry-run: would remove '{topology}' from {}",
            agent_dir.display()
        ));
        return finish("remove", args.json, out, 0);
    }

    // The target is already mutated here: the manifest decides drop vs.
    // Partial, and any reconciliation failure is reported with the target
    // state and a recovery path, not a bare `?`.
    let manifest_present = match state::load_manifest_at(&manifest_path, &anchor) {
        Ok(manifest) => manifest.is_some(),
        Err(exc) => {
            let recovery = format!(
                "re-run 'remove -t {topology} --agent-dir {}' — removal is \
                 retryable and will finish reconciling the registry",
                agent_dir.display()
            );
            return Err(registry_commit_failed(
                "remove", args.json, &out, exc, &anchor, &recovery,
            ));
        }
    };
    let reconciliation = reconcile_removal(
        &anchor,
        &matching_install_ids,
        manifest_present,
        RemovalScope {
            topology: &topology,
            agent_dir: &agent_dir,
            manifest_path: &manifest_path,
        },
        args.json,
        &out,
    )?;
    prune_removed_sources(&anchor, reconciliation.pruned_sources, &mut out)?;
    finish("remove", args.json, out, 0)
}

/// The outcome of a post-removal registry reconciliation.
struct ReconciledRemoval {
    /// Sources whose last referencing install ID was unregistered; their
    /// caches may be pruned.
    pruned_sources: Vec<SourceSpec>,
    /// Install IDs removed from the registry.
    removed_aliases: Vec<String>,
    /// Install IDs kept and marked `Partial` because an ownership
    /// manifest was (re-)observed present — for stale installs this
    /// includes the manifest a concurrent reinstall recreated before the
    /// registry lock was taken.
    retained_aliases: Vec<String>,
}

/// Reconcile the registry after a removal: drop install IDs whose
/// ownership manifest is gone, retain and mark `Partial` those whose
/// manifest remains. Callers must only invoke this after the target
/// question is settled (a successful `remove::run`, or a proven-absent
/// manifest for a stale install ID). When the caller observed the
/// manifest *missing* (stale install ID), the manifest presence is
/// re-checked under the registry lock, immediately before the install ID
/// would be unregistered: a concurrent reinstall that recreated the
/// manifest keeps the entry alive as `Partial` — a live installation is
/// never unregistered. An unreadable manifest is treated as present, so
/// uncertainty always retains the install ID. On a registry failure the
/// captured `out` lines are preserved and the error states the target
/// state and recovery path.
struct RemovalScope<'a> {
    topology: &'a str,
    agent_dir: &'a Path,
    manifest_path: &'a Path,
}

fn reconcile_removal(
    anchor: &Path,
    aliases: &[String],
    manifest_present: bool,
    scope: RemovalScope<'_>,
    json: bool,
    out: &[String],
) -> Result<ReconciledRemoval, CliError> {
    let RemovalScope {
        topology,
        agent_dir,
        manifest_path,
    } = scope;
    let mut result = ReconciledRemoval {
        pruned_sources: Vec::new(),
        removed_aliases: Vec::new(),
        retained_aliases: Vec::new(),
    };
    let reconcile_result = registry::with_registry(anchor, |doc| {
        for alias in aliases {
            let Some(entry) = doc.get(alias) else {
                continue;
            };
            if entry.agent_dir.as_path() != agent_dir || entry.topology != topology {
                return Err(CliError::Registry(format!(
                    "install ID '{alias}' changed concurrently (now '{}' in \
                     {}); refusing to reconcile it",
                    entry.topology,
                    entry.agent_dir.display()
                )));
            }
            // Stale install ID: the manifest was observed missing before
            // the registry lock existed. Re-check under the lock, right
            // before unregistering: a concurrent reinstall may have
            // recreated the manifest, and dropping the install ID would
            // orphan that live installation. An unreadable manifest also
            // retains the install ID (fail closed) but is not reported as
            // a reinstall.
            let present = if manifest_present {
                true
            } else {
                match state::load_manifest_at(manifest_path, anchor) {
                    Ok(Some(_)) => {
                        result.retained_aliases.push(alias.clone());
                        true
                    }
                    Ok(None) => false,
                    Err(_) => true,
                }
            };
            if present {
                let mut partial = entry.clone();
                partial.state = InstallState::Partial;
                doc.insert(alias.clone(), partial)?;
                result.retained_aliases.push(alias.clone());
            } else {
                result.removed_aliases.push(alias.clone());
                result.pruned_sources.push(entry.source.clone());
                doc.remove(alias);
            }
        }
        Ok(())
    });
    match reconcile_result {
        Ok(()) => Ok(result),
        Err(exc) => {
            let recovery = format!(
                "re-run 'remove -t {topology} --agent-dir {dir}' — removal is \
                 retryable; if the ownership manifest is already gone, run \
                 'install -t {topology} --agent-dir {dir}' first, then remove \
                 again",
                dir = agent_dir.display()
            );
            Err(registry_commit_failed(
                "remove", json, out, exc, anchor, &recovery,
            ))
        }
    }
}

/// Prune source caches for the reconciled GitHub sources, per source,
/// retaining every commit still referenced by the post-mutation registry.
/// The reconciliation already landed, so anything failing here can only be
/// a warning, never a command failure.
fn prune_removed_sources(
    anchor: &Path,
    sources: Vec<SourceSpec>,
    out: &mut Vec<String>,
) -> Result<(), CliError> {
    for source in sources {
        if let SourceSpec::GitHub { .. } = source {
            let referenced: BTreeSet<String> = match registry::load_registry(anchor) {
                Ok(doc) => doc
                    .aliases
                    .values()
                    .filter(|entry| entry.source == source)
                    .filter_map(|entry| entry.resolved_commit.clone())
                    .collect(),
                Err(exc) => {
                    out.push(format!(
                        "warning: could not reload the registry to prune the source cache: {exc}"
                    ));
                    continue;
                }
            };
            let cache_root = match state::state_root(anchor) {
                Ok(root) => root.join("sources"),
                Err(exc) => {
                    out.push(format!(
                        "warning: could not resolve source cache root: {exc}"
                    ));
                    continue;
                }
            };
            if let Err(exc) = source::prune_cache(
                &cache_root,
                &topology_source(&source),
                &referenced,
                CACHE_KEEP_UNREFERENCED,
            ) {
                out.push(format!("warning: could not prune source cache: {exc}"));
            }
        }
    }
    Ok(())
}

fn cmd_profiles(args: &ProfilesArgs) -> Result<i32, CliError> {
    match &args.command {
        ProfilesCommand::List(list) => {
            let anchor = state_anchor()?;
            // Pure filesystem discovery plus the registry; never creates
            // profile directories.
            let discovered = profiles::discover_profiles(&anchor)?;
            if list.json {
                let discovered = Json::Array(
                    discovered
                        .iter()
                        .map(|profile| {
                            json!({
                                "name": profile.name,
                                "agent_dir": profile.agent_dir.display().to_string(),
                                "exists": profile.exists,
                                "managed": profile.managed.iter().map(|install| {
                                    json!({
                                        "install_id": install.install_id,
                                        "topology": install.topology,
                                        "source": install.source.describe(),
                                    })
                                }).collect::<Vec<_>>(),
                            })
                        })
                        .collect(),
                );
                print_json(&json!({ "command": "profiles list", "profiles": discovered }))?;
            } else {
                for profile in &discovered {
                    let label = profile.name.as_deref().unwrap_or("(custom)");
                    if profile.exists {
                        println!("{label}  {}", profile.agent_dir.display());
                    } else {
                        println!("{label}  {}  [missing]", profile.agent_dir.display());
                    }
                    for install in &profile.managed {
                        println!(
                            "    install ID {}: {} (via {})",
                            install.install_id,
                            install.topology,
                            install.source.describe()
                        );
                    }
                }
            }
            Ok(0)
        }
    }
}

fn cmd_status(args: &StatusArgs) -> Result<i32, CliError> {
    let anchor = state_anchor()?;
    if args.install_id.is_some() && (args.profile.is_some() || args.agent_dir.is_some()) {
        return Err(CliError::Usage(
            "--install-id cannot be combined with --profile or --agent-dir: \
             the install ID already selects the target"
                .to_string(),
        ));
    }

    if let Some(install_id) = &args.install_id {
        // Read-only install ID lookup.
        let report = status::alias_status(install_id, &anchor)?;
        return emit_status(&report, args.json);
    }

    // Status is read-only: resolving the target must not create a missing
    // profile directory the way `omp --profile NAME config path` does.
    let target = profiles::resolve_target_read_only(&profiles::TargetOptions {
        profile: args.profile.as_deref(),
        agent_dir: args.agent_dir.as_deref(),
    })?;
    let registry_doc = registry::load_registry(&anchor)?;
    let matching: Vec<(&String, &RegistryEntry)> = registry_doc
        .aliases
        .iter()
        .filter(|(_, entry)| entry.agent_dir == target.agent_dir)
        .collect();
    match matching.len() {
        0 => emit_unmanaged(&target, &anchor, args.json),
        1 => {
            let (install_id, entry) = matching[0];
            let report = status::status_for_entry(install_id, entry, &anchor)?;
            emit_status(&report, args.json)
        }
        _ => Err(CliError::Usage(format!(
            "multiple install IDs ({}) target {}; pass --install-id NAME explicitly",
            matching
                .iter()
                .map(|(install_id, _)| install_id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            target.agent_dir.display()
        ))),
    }
}

/// Emit the status of an explicitly unmanaged target: the result states
/// the target is unmanaged and includes the manifest's topology when a
/// manifest exists.
fn emit_unmanaged(
    target: &profiles::ResolvedTarget,
    anchor: &Path,
    json: bool,
) -> Result<i32, CliError> {
    let manifest_topology = state::load_manifest(&target.agent_dir, anchor)?
        .and_then(|manifest| topology::manifest_topology(&manifest).map(str::to_string));
    if json {
        print_json(&json!({
            "command": "status",
            "managed": false,
            "agent_dir": target.agent_dir.display().to_string(),
            "profile": target.profile,
            "topology": manifest_topology,
        }))?;
    } else {
        println!("unmanaged target: {}", target.agent_dir.display());
        if let Some(profile) = &target.profile {
            println!("profile: {profile}");
        }
        match &manifest_topology {
            Some(topology) => {
                println!("topology manifest: {topology}");
                println!("registry install ID: none");
            }
            None => println!(
                "no install ID manages this target and no ownership manifest \
                 was found"
            ),
        }
    }
    Ok(0)
}

fn emit_status(report: &status::AliasStatus, json: bool) -> Result<i32, CliError> {
    if json {
        let manifest = match &report.manifest {
            status::ManifestStatus::Healthy => json!("healthy"),
            status::ManifestStatus::Missing => json!("missing"),
            status::ManifestStatus::Mismatched { .. } => json!("mismatched"),
        };
        let manifest_topology = match &report.manifest {
            status::ManifestStatus::Mismatched { manifest_topology } => json!(manifest_topology),
            _ => Json::Null,
        };
        let freshness = match &report.freshness {
            status::Freshness::UpToDate { current } => {
                json!({ "status": "up-to-date", "current": current })
            }
            status::Freshness::UpdateAvailable { current } => {
                json!({ "status": "update-available", "current": current })
            }
            status::Freshness::Unknown { reason } => {
                json!({ "status": "unknown", "reason": reason })
            }
        };
        print_json(&json!({
            "command": "status",
            "managed": true,
            "install_id": report.install_id,
            "state": if report.state == InstallState::Installed { "installed" } else { "partial" },
            "agent_dir": report.agent_dir.display().to_string(),
            "profile": report.profile,
            "topology": report.topology,
            "source": report.source.describe(),
            "git_ref": report.git_ref,
            "installed_at": report.installed_at,
            "installed_commit": report.installed_commit,
            "installed_digest": report.installed_digest,
            "manifest": manifest,
            "manifest_topology": manifest_topology,
            "freshness": freshness,
        }))?;
    } else {
        println!("install ID: {}", report.install_id);
        println!(
            "state: {}",
            if report.state == InstallState::Installed {
                "installed"
            } else {
                "partial"
            }
        );
        println!("topology: {}", report.topology);
        println!("agent dir: {}", report.agent_dir.display());
        if let Some(profile) = &report.profile {
            println!("profile: {profile}");
        }
        match &report.git_ref {
            Some(reference) => {
                println!("source: {} (ref {reference})", report.source.describe())
            }
            None => println!("source: {}", report.source.describe()),
        }
        println!("installed at: {}", report.installed_at);
        if let Some(commit) = &report.installed_commit {
            println!("installed commit: {commit}");
        }
        if let Some(digest) = &report.installed_digest {
            println!("installed digest: {digest}");
        }
        match &report.manifest {
            status::ManifestStatus::Healthy => println!("manifest: healthy"),
            status::ManifestStatus::Missing => println!("manifest: missing"),
            status::ManifestStatus::Mismatched { manifest_topology } => {
                let found = manifest_topology
                    .as_deref()
                    .unwrap_or("none (manifest declares no topology)");
                println!("manifest: mismatched (manifest records '{found}')");
            }
        }
        match &report.freshness {
            status::Freshness::UpToDate { current } => {
                println!("freshness: up to date (current {current})")
            }
            status::Freshness::UpdateAvailable { current } => {
                println!("update available (current {current})")
            }
            status::Freshness::Unknown { reason } => {
                println!("freshness: unknown ({reason})")
            }
        }
    }
    Ok(0)
}

fn cmd_update(args: &UpdateArgs) -> Result<i32, CliError> {
    let anchor = state_anchor()?;
    // Config handling for this update only; never remembered. The source
    // override commits to the registry only if the update succeeds
    // (update::run stages the new pin as Partial first).
    let config_mode = config_mode_from_flags(
        args.config_merge,
        args.config_replace,
        args.config_precedence.as_deref(),
    )?;
    let mut out = Vec::new();
    let code = match update::run(
        &update::UpdateOptions {
            install_id: &args.install_id,
            reference: args.reference.as_deref(),
            source: args.source.as_deref(),
            version: args.version.as_ref(),
            state_anchor: &anchor,
            dry_run: args.dry_run,
            config_mode,
        },
        &mut out,
    ) {
        Ok(code) => code,
        Err(exc) => {
            // update::run records the intended pin as Partial while holding
            // the registry lock before target mutation. An error leaves that
            // recovery marker in place; rerunning reconciles it.
            preserve_lines(args.json, &out);
            let registry_path = registry::registry_path(&anchor)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "the state root".to_string());
            return Err(CliError::Registry(format!(
                "update of install ID '{}' failed: {exc}; if target mutation \
                 started, the registry at {registry_path} remains marked \
                 partial; re-run 'update --install-id {}' to reconcile it",
                args.install_id, args.install_id
            )));
        }
    };
    finish("update", args.json, out, code)
}

fn cmd_self_update(args: &SelfUpdateArgs) -> Result<i32, CliError> {
    let mut out = Vec::new();
    let code = match self_update::run(args.version.as_deref(), args.dry_run, &mut out) {
        Ok(code) => code,
        Err(exc) => {
            preserve_lines(args.json, &out);
            return Err(exc);
        }
    };
    finish("self-update", args.json, out, code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "omp-topology-reconcile-test-{}-{}",
                std::process::id(),
                TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).expect("failed to create isolated reconciliation test dir");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reconcile_stale_alias_retains_manifest_recreated_before_locked_check() {
        let temp = TestDir::new();
        let state_home = temp.0.join("state-home");
        let anchor = temp.0.join("repo");
        let agent_dir = temp.0.join("agent");
        std::fs::create_dir(&anchor).expect("failed to create isolated state anchor");
        std::fs::create_dir(&agent_dir).expect("failed to create isolated agent directory");
        let alias = "reinstalled-alias";
        let topology = "reinstalled-topology";

        state::with_state_home_for_test(&state_home, || {
            assert!(
                state::load_manifest(&agent_dir, &anchor)
                    .expect("failed to inspect initial manifest")
                    .is_none(),
                "fixture must begin with the manifest missing"
            );
            let entry = RegistryEntry::new(
                agent_dir.clone(),
                None,
                topology.to_string(),
                SourceSpec::Local {
                    path: anchor.clone(),
                },
                None,
                None,
                None,
            );
            registry::with_registry(&anchor, |doc| doc.insert(alias, entry))
                .expect("failed to seed registry entry");

            state::write_manifest(
                &agent_dir,
                &anchor,
                &serde_json::json!({"version": state::MANIFEST_VERSION, "files": {}, "config": {}}),
            )
            .expect("failed to recreate ownership manifest");

            let result = reconcile_removal(
                &anchor,
                &[alias.to_string()],
                false,
                RemovalScope {
                    topology,
                    agent_dir: &agent_dir,
                    manifest_path: &state::manifest_path(&agent_dir, &anchor)
                        .expect("failed to resolve manifest path"),
                },
                false,
                &[],
            )
            .expect("stale alias reconciliation failed");

            assert!(result.retained_aliases.iter().any(|item| item == alias));
            assert!(result.removed_aliases.is_empty());
            assert!(result.pruned_sources.is_empty());
            let registry = registry::load_registry(&anchor).expect("failed to reload registry");
            assert_eq!(
                registry.get(alias).map(|entry| entry.state),
                Some(InstallState::Partial),
                "recreated manifest must keep its install ID marked partial"
            );
        });
    }
}
