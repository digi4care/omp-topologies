//! Cross-agent install registry: a versioned JSON document under the XDG
//! state root that maps stable aliases to install metadata.
//!
//! The registry records *metadata only* — agent directory, topology, source
//! spec, resolved commit, overlay digest, timestamp, and install state. It
//! never stores credentials, config contents, or file payloads; the
//! per-agent ownership manifest (`state.rs`) stays authoritative for safe
//! removal.
//!
//! Guarantees:
//!
//! * Versioned schema (`REGISTRY_VERSION`); missing registry loads empty,
//!   malformed or unsupported registries fail closed with an error.
//! * Aliases live in a `BTreeMap`, so serialization is deterministically
//!   sorted and canonical bytes can be compared/audited.
//! * All writes are descriptor-relative below a pinned state-root
//!   descriptor ([`RootFd`]: atomic temp-file + rename, owner-only
//!   permissions) inside a create-new lock ([`RegistryLock`]) with a
//!   bounded wait; stale locks are never stolen.
//! * [`with_registry`] performs locked read/merge/write so concurrent
//!   processes cannot lose each other's updates.
//! * Agent and local-source paths are persisted as UTF-8 JSON strings; a
//!   path that is not valid UTF-8 is rejected with an error before any
//!   write rather than lossily rewritten (see [`utf8_path`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value as Json};

use crate::anchored::{EntryKind, RootFd};
use crate::error::{CliError, io_err};
use crate::fsx;
use crate::state;

/// Registry format version written by this installer.
pub const REGISTRY_VERSION: u64 = 1;

/// Snapshots retained per agent state directory by [`prune_snapshots`].
pub const SNAPSHOT_KEEP: usize = 10;

/// Marker file inside a snapshot directory that pins that snapshot against
/// ordinary retention pruning: written when an install/remove rollback
/// retains the snapshot as the only pre-mutation recovery data, cleared by
/// [`clear_recovery_snapshots`] once the operation reconciles successfully.
pub const RECOVERY_MARKER: &str = ".recovery-required";

/// Total time [`RegistryLock::acquire`] waits for a competing holder.
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll interval while waiting for the registry lock.
const LOCK_POLL: Duration = Duration::from_millis(50);

/// Where the registry document lives, under the XDG state root.
pub fn registry_path(repo_root: &Path) -> Result<PathBuf, CliError> {
    Ok(state::state_root(repo_root)?.join("registry.json"))
}

/// The create-new lock sibling of the registry document.
fn lock_path(repo_root: &Path) -> Result<PathBuf, CliError> {
    Ok(state::state_root(repo_root)?.join(".registry.lock"))
}

/// Pin a state/agent-state directory with the no-symlink-root policy:
/// the agent directory is a configured root, so a symlink swapped in at
/// it (or at the state directory itself) fails closed instead of
/// redirecting registry reads, locks, and snapshot retention.
fn strict_state_root(path: &Path) -> Result<Option<RootFd>, CliError> {
    match RootFd::open_strict_root(path).map_err(|err| io_err(path, err))? {
        crate::anchored::StrictRoot::Pinned(root) => Ok(Some(root)),
        crate::anchored::StrictRoot::Absent => Ok(None),
        crate::anchored::StrictRoot::Symlink => Err(CliError::Topology(format!(
            "refusing to continue: the state directory '{}' is a symlink",
            path.display()
        ))),
    }
}

/// Pin the state root for reading: `None` when it does not exist yet.
fn open_state(repo_root: &Path) -> Result<Option<RootFd>, CliError> {
    strict_state_root(&state::state_root(repo_root)?)
}

/// Pin the state root, creating it (and missing parents, chmodded to
/// `0o700` like `fsx::makedirs_private`) when absent.
fn open_state_create(repo_root: &Path) -> Result<RootFd, CliError> {
    let path = state::state_root(repo_root)?;
    RootFd::open_or_create_private(&path).map_err(|err| io_err(&path, err))
}

/// A registry failure mapped onto the shared [`CliError`] surface.
fn fail(message: impl std::fmt::Display) -> CliError {
    CliError::Registry(format!("{message}"))
}

/// Where an install came from: a pinned GitHub repository or a local path
/// containing a root package or legacy catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSpec {
    /// `github:OWNER/REPO`.
    GitHub { owner: String, repo: String },
    /// Local source path containing a root package or legacy catalog.
    Local { path: PathBuf },
}

impl SourceSpec {
    /// Human/source syntax form (`github:OWNER/REPO` or the local path).
    pub fn describe(&self) -> String {
        match self {
            SourceSpec::GitHub { owner, repo } => format!("github:{owner}/{repo}"),
            SourceSpec::Local { path } => path.display().to_string(),
        }
    }
}

/// Lifecycle state recorded for one registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallState {
    /// Every planned file/config change completed.
    Installed,
    /// A previous install/remove was interrupted; the manifest is
    /// authoritative for what actually landed.
    Partial,
}

impl InstallState {
    fn as_str(self) -> &'static str {
        match self {
            InstallState::Installed => "installed",
            InstallState::Partial => "partial",
        }
    }
}

/// Metadata for one installed topology, keyed by its stable alias.
///
/// Contains metadata only — never credentials or config contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryEntry {
    /// Resolved absolute agent directory this install targets.
    pub agent_dir: PathBuf,
    /// OMP profile name (`--profile`), when the target was profile-selected.
    pub profile: Option<String>,
    /// Installed topology name.
    pub topology: String,
    /// Where the topology source came from.
    pub source: SourceSpec,
    /// Requested git ref (branch/tag) for GitHub sources.
    pub git_ref: Option<String>,
    /// Commit the source was resolved to at install time (`None` for local
    /// sources without a git pin).
    pub resolved_commit: Option<String>,
    /// Digest of the local overlay inputs, when an overlay was applied.
    pub overlay_digest: Option<String>,
    /// Install timestamp in [`fsx::timestamp`] format.
    pub installed_at: String,
    /// Installed/partial lifecycle state.
    pub state: InstallState,
    /// Which ownership manifest this alias owns (`state::ManifestSlot`).
    /// The first alias targeting an agent directory owns the legacy
    /// `manifest.json`; every additional alias for the same directory
    /// owns its per-alias `manifests/<alias>.json`, so ownership state
    /// is never shared between aliases.
    pub manifest_slot: state::ManifestSlot,
}

impl RegistryEntry {
    /// A new entry in [`InstallState::Installed`] stamped with the current
    /// local time.
    pub fn new(
        agent_dir: PathBuf,
        profile: Option<String>,
        topology: String,
        source: SourceSpec,
        git_ref: Option<String>,
        resolved_commit: Option<String>,
        overlay_digest: Option<String>,
    ) -> Self {
        RegistryEntry {
            agent_dir,
            profile,
            topology,
            source,
            git_ref,
            resolved_commit,
            overlay_digest,
            installed_at: fsx::timestamp(),
            state: InstallState::Installed,
            // Callers installing an additional alias for a directory that
            // already has one set [`state::ManifestSlot::PerAlias`].
            manifest_slot: state::ManifestSlot::Legacy,
        }
    }
}

/// Ownership that OTHER aliases hold over a shared agent directory, read
/// from their own manifests: the config keys they installed, the
/// `config.yml.new` publication, and the files/skills they own. An
/// install, update, or remove for one alias must fail closed before
/// writing, owning, restoring, or deleting anything recorded here —
/// there is no shared ownership, even for byte-identical content.
#[derive(Debug, Default)]
pub struct CrossAliasOwnership {
    /// Config leaf → owning alias, from every other alias's manifest.
    pub config_installed: BTreeMap<Vec<String>, String>,
    /// Alias recording ownership of `config.yml.new`, if any.
    pub candidate_owner: Option<String>,
    /// Agent-dir-relative file → owning alias.
    pub files: BTreeMap<String, String>,
    /// Recorded skills root → skills-root-relative file → owning alias.
    /// The same relative path under a different skills root is a
    /// different file, so lookups must carry the root.
    pub skills_files: BTreeMap<String, BTreeMap<String, String>>,
}

/// Collect the ownership every other registered alias for `agent_dir`
/// records in its own manifest, skipping the manifest at
/// `own_manifest_path` (the caller's). Missing manifests contribute
/// nothing; unreadable or invalid ones fail closed.
pub fn cross_alias_ownership(
    state_anchor: &Path,
    agent_dir: &Path,
    own_manifest_path: &Path,
) -> Result<CrossAliasOwnership, CliError> {
    let mut ownership = CrossAliasOwnership::default();
    let registry_doc = load_registry(state_anchor)?;
    for (alias, entry) in &registry_doc.aliases {
        if entry.agent_dir != agent_dir {
            continue;
        }
        let other_path =
            state::manifest_path_for_slot(agent_dir, state_anchor, alias, entry.manifest_slot)?;
        if other_path == own_manifest_path {
            continue;
        }
        let Some(other) = state::load_manifest_at(&other_path, state_anchor)? else {
            continue;
        };
        let config = other.get("config").cloned().unwrap_or(Json::Null);
        for parts in config
            .get("installed")
            .map(state::decode_values)
            .unwrap_or_default()
            .into_keys()
        {
            ownership
                .config_installed
                .entry(parts)
                .or_insert_with(|| alias.clone());
        }
        if other.get("config_candidate_sha256").is_some() {
            ownership.candidate_owner = Some(alias.clone());
        }
        if let Some(entries) = other.get("files").and_then(Json::as_object) {
            for rel in entries.keys() {
                ownership
                    .files
                    .entry(rel.clone())
                    .or_insert_with(|| alias.clone());
            }
        }
        if let Some(entries) = other.get("skills_files").and_then(Json::as_object)
            && !entries.is_empty()
        {
            // Skill files are located relative to the sibling's own
            // recorded skills root; without it they cannot be compared
            // against this install's targets, so fail closed rather
            // than match them against every root.
            let root = other
                .get("skills_dir")
                .and_then(Json::as_str)
                .filter(|dir| !dir.is_empty())
                .ok_or_else(|| {
                    CliError::Manifest(format!(
                        "alias '{alias}' records skills files but no \
                         'skills_dir' root; refusing to act on it"
                    ))
                })?;
            let owned = ownership.skills_files.entry(root.to_string()).or_default();
            for rel in entries.keys() {
                owned.entry(rel.clone()).or_insert_with(|| alias.clone());
            }
        }
    }
    Ok(ownership)
}

/// The versioned registry document: a deterministically sorted alias map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    /// Schema version; must equal [`REGISTRY_VERSION`] on load.
    pub version: u64,
    /// Stable alias → install metadata, sorted by alias.
    pub aliases: BTreeMap<String, RegistryEntry>,
}

impl Registry {
    /// An empty registry at the current schema version.
    pub fn empty() -> Self {
        Registry {
            version: REGISTRY_VERSION,
            aliases: BTreeMap::new(),
        }
    }

    /// The entry registered under `alias`, if any.
    pub fn get(&self, alias: &str) -> Option<&RegistryEntry> {
        self.aliases.get(alias)
    }

    /// Register `entry` under `alias`, replacing any previous entry.
    ///
    /// Validates `alias` before mutating, so a caller cannot persist a
    /// registry that the next [`load_registry`] would reject (same rule and
    /// error text as [`validate_alias`]).
    pub fn insert(
        &mut self,
        alias: impl Into<String>,
        entry: RegistryEntry,
    ) -> Result<(), CliError> {
        let alias = alias.into();
        validate_alias(&alias)?;
        if entry.manifest_slot == state::ManifestSlot::Legacy
            && self.aliases.iter().any(|(other_alias, other)| {
                other_alias != &alias
                    && other.agent_dir == entry.agent_dir
                    && other.manifest_slot == state::ManifestSlot::Legacy
            })
        {
            return Err(fail(format!(
                "alias '{alias}' cannot own the legacy manifest of '{}' \
                 (another alias already does)",
                entry.agent_dir.display()
            )));
        }
        self.aliases.insert(alias, entry);
        Ok(())
    }

    /// Remove and return the entry registered under `alias`.
    pub fn remove(&mut self, alias: &str) -> Option<RegistryEntry> {
        self.aliases.remove(alias)
    }
}

/// Load and strictly validate the registry; a missing registry loads empty.
///
/// Fails closed: only `NotFound` yields an empty registry. A symlinked
/// registry is refused, and any existing non-regular object (directory,
/// socket, device) is an error, as is unreadable JSON, an unsupported
/// version, unknown fields, or a malformed entry — never a guessed-partial
/// registry.
pub fn load_registry(repo_root: &Path) -> Result<Registry, CliError> {
    let path = registry_path(repo_root)?;
    let Some(root) = open_state(repo_root)? else {
        return Ok(Registry::empty());
    };
    // `kind` and `read_file` both re-walk with `O_NOFOLLOW` from the
    // pinned state-root descriptor, so a swapped-in symlink fails closed
    // instead of being followed.
    match root
        .kind("registry.json")
        .map_err(|err| io_err(&path, err))?
    {
        None => return Ok(Registry::empty()),
        Some(EntryKind::Symlink) => {
            return Err(fail(format!(
                "refusing to read symlinked registry '{}'",
                path.display()
            )));
        }
        Some(EntryKind::File) => {}
        Some(_) => {
            return Err(fail(format!(
                "registry '{}' exists but is not a regular file",
                path.display()
            )));
        }
    }
    let bytes = root
        .read_file("registry.json")
        .map_err(|err| io_err(&path, err))?
        .ok_or_else(|| io_err(&path, std::io::Error::from(std::io::ErrorKind::NotFound)))?;
    let text = String::from_utf8(bytes).map_err(|err| {
        fail(format!(
            "registry '{}' is unreadable: {err}",
            path.display()
        ))
    })?;
    let data: Json = serde_json::from_str(&text).map_err(|err| {
        fail(format!(
            "registry '{}' is unreadable: {err}",
            path.display()
        ))
    })?;
    registry_from_json(&data)
}

/// Canonical registry bytes: sorted keys, two-space indent, trailing
/// newline — the same convention as `state::serialize_manifest`.
///
/// Fails closed: registry paths must round-trip byte-faithfully through
/// the UTF-8-only JSON string form, so an agent or local-source path that
/// is not valid UTF-8 is rejected with an error instead of being written
/// with `Path::display()`'s lossy U+FFFD replacement, which could reload
/// a different directory.
pub fn serialize_registry(registry: &Registry) -> Result<Vec<u8>, CliError> {
    let mut text = serde_json::to_string_pretty(&registry_to_json(registry)?)
        .expect("registry serialization cannot fail: JSON value tree");
    text.push('\n');
    Ok(text.into_bytes())
}

/// Write the registry with owner-only permissions (atomic temp-file +
/// rename, descriptor-relative below the pinned state root). Callers
/// normally use [`with_registry`], which couples the write to the lock;
/// this is for callers that already hold a [`RegistryLock`].
pub fn write_registry(repo_root: &Path, registry: &Registry) -> Result<(), CliError> {
    let path = registry_path(repo_root)?;
    let root = open_state_create(repo_root)?;
    root.write_private("registry.json", &serialize_registry(registry)?)
        .map_err(|err| io_err(&path, err))
}

/// Hold the registry lock; released on drop by removing the lock file.
///
/// The lock is a private create-new file: no stale-lock stealing — a holder
/// that dies leaves the lock behind and the next acquirer fails after a
/// bounded wait with an instruction to remove it manually.
pub struct RegistryLock {
    root: Option<RootFd>,
}

impl RegistryLock {
    /// Acquire the registry lock, waiting at most [`LOCK_TIMEOUT`].
    pub fn acquire(repo_root: &Path) -> Result<RegistryLock, CliError> {
        let path = lock_path(repo_root)?;
        let root = open_state_create(repo_root)?;
        let start = Instant::now();
        loop {
            let line = format!("pid={} acquired={}\n", std::process::id(), fsx::timestamp());
            match root.write_exclusive(".registry.lock", line.as_bytes(), Some(0o600)) {
                Ok(true) => {
                    return Ok(RegistryLock { root: Some(root) });
                }
                Ok(false) => {
                    if start.elapsed() >= LOCK_TIMEOUT {
                        return Err(fail(format!(
                            "registry is locked by another process (lock file \
                             '{}'); if no other omp-profile process is \
                             running, remove the lock file manually",
                            path.display()
                        )));
                    }
                    std::thread::sleep(LOCK_POLL);
                }
                Err(err) => return Err(io_err(&path, err)),
            }
        }
    }
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        if let Some(root) = self.root.take() {
            let _ = root.remove_file(".registry.lock");
        }
    }
}

/// Acquire the registry lock, load the current registry, apply `merge`, and
/// persist the result — locked read/merge/write so concurrent processes
/// cannot lose each other's updates.
pub fn with_registry<T>(
    repo_root: &Path,
    merge: impl FnOnce(&mut Registry) -> Result<T, CliError>,
) -> Result<T, CliError> {
    let _lock = RegistryLock::acquire(repo_root)?;
    let mut registry = load_registry(repo_root)?;
    let result = merge(&mut registry)?;
    write_registry(repo_root, &registry)?;
    Ok(result)
}

/// Mark `snapshot_dir` as recovery-required: the snapshot is the only
/// pre-mutation recovery data retained by an incomplete install/remove
/// rollback and must survive ordinary retention pruning until a successful
/// reconciliation clears the marker via [`clear_recovery_snapshots`].
///
/// The marker is a private state file (atomic temp-file + rename,
/// owner-only permissions) named [`RECOVERY_MARKER`] inside the snapshot
/// directory, written descriptor-relative below a pinned snapshot-root
/// descriptor. When the failing operation belongs to a registry install
/// ID the marker content records it, so a successful run of a *different*
/// install ID sharing the agent directory never clears this install ID's
/// only rollback data; an install-ID-less operation writes the legacy
/// untagged content.
pub fn mark_recovery_snapshot(snapshot_dir: &Path, alias: Option<&str>) -> Result<(), CliError> {
    let marker = snapshot_dir.join(RECOVERY_MARKER);
    let root =
        RootFd::open_or_create_private(snapshot_dir).map_err(|err| io_err(snapshot_dir, err))?;
    let content = match alias {
        Some(alias) => {
            format!("retained: recovery required after incomplete rollback (alias '{alias}')\n")
        }
        None => "retained: recovery required after incomplete rollback\n".to_string(),
    };
    root.write_private(RECOVERY_MARKER, content.as_bytes())
        .map_err(|err| io_err(&marker, err))
}

/// Marker-content tag recording which alias a recovery marker belongs to.
fn marker_alias_tag(alias: &str) -> String {
    format!("(alias '{alias}')")
}

/// Remove [`RECOVERY_MARKER`] files inside the agent's snapshot
/// directories, returning the number of markers removed. Call only after
/// the operation that left recovery markers behind has fully reconciled;
/// while any marker remains, [`prune_snapshots`] keeps the marked
/// snapshots.
///
/// Several aliases may share one agent directory, so clearing is scoped:
/// `alias` selects markers whose content tags that alias, and
/// `include_untagged` additionally selects legacy markers with no alias
/// tag — pass it only for the operation that owns the legacy per-agent
/// manifest (or for an alias-less operation), never for an additional
/// per-alias install, so one alias's success cannot destroy a sibling
/// alias's only rollback data. A marker whose content cannot be read is
/// conservatively left in place.
///
/// The same name and real-directory rules as [`prune_snapshots`] apply:
/// only exact `install-<timestamp>` / `remove-<timestamp>` directories under
/// `backups/` and `snapshots/` are scanned, symlinked roots and entries are
/// never followed, and a marker that exists but is not a regular
/// non-symlink file (symlink, directory, device) is conservatively left in
/// place, so the snapshot stays protected.
pub fn clear_recovery_snapshots(
    agent_dir: &Path,
    state_anchor: &Path,
    alias: Option<&str>,
    include_untagged: bool,
) -> Result<usize, CliError> {
    let state = state::agent_state_dir(agent_dir, state_anchor)?;
    let Some(root) = strict_state_root(&state)? else {
        return Ok(0);
    };
    let tag = alias.map(marker_alias_tag);
    let mut cleared = 0usize;
    for (_, rel) in snapshot_rels(&root)? {
        let marker_rel = format!("{rel}/{RECOVERY_MARKER}");
        let marker = state.join(&marker_rel);
        // `kind` classifies `AT_SYMLINK_NOFOLLOW`: a symlinked or
        // non-regular marker fails this check and is left untouched.
        match root.kind(&marker_rel).map_err(|err| io_err(&marker, err))? {
            Some(EntryKind::File) => {}
            _ => continue,
        }
        let content = match root.read_file(&marker_rel) {
            Ok(Some(bytes)) => bytes,
            // An unreadable marker keeps its snapshot protected.
            _ => continue,
        };
        let tagged = tag
            .as_ref()
            .is_some_and(|tag| contains_bytes(&content, tag.as_bytes()));
        let untagged = !content
            .windows(b"(alias '".len())
            .any(|window| window == b"(alias '");
        if !(tagged || (include_untagged && untagged)) {
            continue;
        }
        root.remove_file(&marker_rel)
            .map_err(|err| io_err(&marker, err))?;
        cleared += 1;
    }
    Ok(cleared)
}

/// `true` when `haystack` contains `needle` as a contiguous byte run.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Enumerate the real snapshot directories of the pinned agent state
/// descriptor — `backups/install-<timestamp>/` (created by install) and
/// `snapshots/remove-<timestamp>/` (created by remove) — returning each
/// directory's timestamp suffix and its path relative to the state root.
///
/// Only directories whose name is exactly `install-<timestamp>` or
/// `remove-<timestamp>` in [`fsx::timestamp`] format (`YYYY-MM-DDTHH-MM-
/// SS-microseconds`) are considered snapshots; malformed or unrelated
/// names are skipped. Roots and entries are classified `AT_SYMLINK_NOFOLLOW`
/// from the pinned descriptor, so symlinked roots are skipped entirely and
/// symlinked entries are never followed — only real directories are
/// returned.
fn snapshot_rels(root: &RootFd) -> Result<Vec<(String, String)>, CliError> {
    let mut snapshots: Vec<(String, String)> = Vec::new();
    for root_name in ["backups", "snapshots"] {
        let root_path = root.path().join(root_name);
        match root
            .kind(root_name)
            .map_err(|err| io_err(&root_path, err))?
        {
            Some(EntryKind::Directory) => {}
            _ => continue,
        }
        let read = |err: std::io::Error| io_err(&root_path, err);
        for (name, kind) in root.list(root_name).map_err(read)? {
            // `list` classifies `AT_SYMLINK_NOFOLLOW`, so a symlinked
            // entry fails this check and is never returned.
            if kind != EntryKind::Directory {
                continue;
            }
            let name = name.to_string_lossy().into_owned();
            let suffix = name
                .strip_prefix("install-")
                .or_else(|| name.strip_prefix("remove-"));
            // Only exact `install-<timestamp>` / `remove-<timestamp>` names
            // are generated snapshots; anything else under these roots is
            // user data and must not be touched.
            let suffix = match suffix {
                Some(suffix) if is_timestamp(suffix) => suffix,
                _ => continue,
            };
            snapshots.push((suffix.to_string(), format!("{root_name}/{name}")));
        }
    }
    Ok(snapshots)
}

/// `true` when the snapshot at `snapshot_rel` below `root` contains a
/// [`RECOVERY_MARKER`], pinning the snapshot against retention pruning.
/// Any existing marker counts — including a non-regular or symlinked one,
/// which is conservatively treated as present so the only recovery data is
/// never destroyed.
fn recovery_marker_present(root: &RootFd, snapshot_rel: &str) -> bool {
    root.exists(&format!("{snapshot_rel}/{RECOVERY_MARKER}"))
        .unwrap_or(false)
}

/// Retain the `keep` newest unmarked snapshot directories across both
/// snapshot roots of the agent state directory — `backups/install-
/// <timestamp>/` (created by install) and `snapshots/remove-<timestamp>/`
/// (created by remove) — removing the rest.
///
/// A snapshot containing a [`RECOVERY_MARKER`] is always kept, even when
/// it would fall outside the `keep` newest: it is the only pre-mutation
/// recovery data retained by an incomplete rollback, and it must survive
/// until [`clear_recovery_snapshots`] runs after a successful
/// reconciliation. Only then does the normal combined `keep` bound apply
/// again. A marker that exists but is non-regular or a symlink also keeps
/// the snapshot (conservative). Only directories whose name is exactly
/// `install-<timestamp>` or `remove-<timestamp>` in [`fsx::timestamp`]
/// format (`YYYY-MM-DDTHH-MM-SS-microseconds`) are considered snapshots;
/// malformed or unrelated names are skipped, never deleted. Roots are
/// probed with `symlink_metadata` and entries classified via
/// `DirEntry::file_type`, neither of which follows links: symlinked roots
/// are skipped entirely and symlinked entries are never followed or
/// removed — only real directories are deleted. Call only after a
/// successful mutation — a prune failure must not roll back or block the
/// mutation it follows.
///
/// Complexity: O(N log N) in the number of snapshot directories for the
/// combined sort.
pub fn prune_snapshots(agent_dir: &Path, repo_root: &Path, keep: usize) -> Result<(), CliError> {
    let state = state::agent_state_dir(agent_dir, repo_root)?;
    let Some(root) = strict_state_root(&state)? else {
        return Ok(());
    };
    let mut keyed = snapshot_rels(&root)?;
    // Sort by the timestamp suffix, not the full name — `remove-*` would
    // always sort ahead of `install-*` lexicographically, regardless of
    // age. A valid timestamp suffix has a fixed width, so this order is
    // chronological; the path is the tie-break so the order stays total
    // and deterministic.
    keyed.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (_, rel) in keyed.iter().skip(keep) {
        // Recovery-pinned snapshots survive ordinary retention; they are
        // removed only after `clear_recovery_snapshots` once recovery has
        // been reconciled.
        if recovery_marker_present(&root, rel) {
            continue;
        }
        // Descriptor-relative recursive removal: sub-directories are
        // re-pinned with `O_NOFOLLOW` at every level, so a swapped-in
        // symlink inside a snapshot is unlinked, never followed.
        root.remove_tree(rel)
            .map_err(|err| io_err(&state.join(rel), err))?;
    }
    Ok(())
}

/// `true` if `text` is exactly the [`fsx::timestamp`] shape:
/// `YYYY-MM-DDTHH-MM-SS-microseconds` — 4-2-2 digits, `T`, 2-2-2 digits,
/// `-`, then exactly 6 fractional digits, with literal separators.
fn is_timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 26 {
        return false;
    }
    let digit = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    digit(0..4)
        && bytes[4] == b'-'
        && digit(5..7)
        && bytes[7] == b'-'
        && digit(8..10)
        && bytes[10] == b'T'
        && digit(11..13)
        && bytes[13] == b'-'
        && digit(14..16)
        && bytes[16] == b'-'
        && digit(17..19)
        && bytes[19] == b'-'
        && digit(20..26)
}

/// Strict parse of the registry document; rejects unknown fields so a
/// newer schema fails closed instead of being silently downgraded.
fn registry_from_json(data: &Json) -> Result<Registry, CliError> {
    let obj = data
        .as_object()
        .ok_or_else(|| fail("top level must be a JSON object"))?;
    reject_unknown(obj, &["version", "aliases"], "registry")?;
    let version = obj
        .get("version")
        .and_then(Json::as_u64)
        .ok_or_else(|| fail("missing or non-numeric 'version'"))?;
    if version != REGISTRY_VERSION {
        return Err(fail(format!(
            "unsupported registry version {version} (this installer writes \
             version {REGISTRY_VERSION}); refusing to downgrade"
        )));
    }
    let aliases_obj = obj
        .get("aliases")
        .and_then(Json::as_object)
        .ok_or_else(|| fail("missing or non-object 'aliases'"))?;
    let mut aliases = BTreeMap::new();
    for (alias, value) in aliases_obj {
        validate_alias(alias)?;
        let entry = entry_from_json(alias, value)?;
        aliases.insert(alias.clone(), entry);
    }
    // Fail closed on a corrupt document: the legacy manifest of one agent
    // directory can only be owned by one alias (per-alias manifests are
    // unique by construction, so only `Legacy` slots need this check).
    let mut legacy_owner: BTreeMap<&Path, &str> = BTreeMap::new();
    for (alias, entry) in &aliases {
        if entry.manifest_slot != state::ManifestSlot::Legacy {
            continue;
        }
        if let Some(other) = legacy_owner.insert(&entry.agent_dir, alias) {
            return Err(fail(format!(
                "aliases '{other}' and '{alias}' both claim the legacy \
                 manifest of '{}'",
                entry.agent_dir.display()
            )));
        }
    }
    Ok(Registry { version, aliases })
}

fn registry_to_json(registry: &Registry) -> Result<Json, CliError> {
    let mut aliases = Map::new();
    for (alias, entry) in &registry.aliases {
        aliases.insert(alias.clone(), entry_to_json(entry)?);
    }
    Ok(serde_json::json!({
        "version": registry.version,
        "aliases": aliases,
    }))
}

/// Validate an alias against the approved slug grammar
/// `[a-z0-9][a-z0-9-]{0,63}` (lowercase, at most 64 characters) before any
/// target mutation; [`Registry::insert`] and [`load_registry`] enforce the
/// same rule through this single validator.
pub fn validate_alias(alias: &str) -> Result<(), CliError> {
    let mut chars = alias.chars();
    let first = chars.next().unwrap_or('\0');
    if !(first.is_ascii_lowercase() || first.is_ascii_digit())
        || !alias
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || alias.len() > 64
    {
        return Err(fail(format!(
            "invalid install ID '{alias}': must match [a-z0-9][a-z0-9-]{{0,63}}"
        )));
    }
    Ok(())
}

fn entry_from_json(alias: &str, value: &Json) -> Result<RegistryEntry, CliError> {
    let obj = value
        .as_object()
        .ok_or_else(|| fail(format!("entry '{alias}' must be a JSON object")))?;
    reject_unknown(
        obj,
        &[
            "agent_dir",
            "profile",
            "topology",
            "source",
            "ref",
            "resolved_commit",
            "overlay_digest",
            "installed_at",
            "state",
            "manifest",
        ],
        &format!("entry '{alias}'"),
    )?;
    let agent_dir = PathBuf::from(require_str(obj, "agent_dir", alias)?);
    if agent_dir.as_os_str().is_empty() {
        return Err(fail(format!("entry '{alias}' has an empty agent_dir")));
    }
    if !agent_dir.is_absolute() {
        return Err(fail(format!(
            "entry '{alias}' has a non-absolute agent_dir"
        )));
    }
    let profile = optional_str(obj, "profile", alias)?;
    let topology = require_str(obj, "topology", alias)?.to_string();
    let source = source_from_json(alias, obj.get("source"))?;
    let git_ref = optional_str(obj, "ref", alias)?;
    let resolved_commit = optional_str(obj, "resolved_commit", alias)?;
    if let Some(commit) = &resolved_commit
        && !is_hex(commit, 40)
    {
        return Err(fail(format!(
            "entry '{alias}' has a resolved_commit that is not 40 hex \
             characters"
        )));
    }
    let overlay_digest = optional_str(obj, "overlay_digest", alias)?;
    if let Some(digest) = &overlay_digest
        && !is_hex(digest, 64)
    {
        return Err(fail(format!(
            "entry '{alias}' has an overlay_digest that is not 64 hex \
             characters"
        )));
    }
    let installed_at = require_str(obj, "installed_at", alias)?.to_string();
    let install_state = match require_str(obj, "state", alias)? {
        "installed" => InstallState::Installed,
        "partial" => InstallState::Partial,
        other => {
            return Err(fail(format!(
                "entry '{alias}' has unsupported state '{other}'"
            )));
        }
    };
    // Registries written before multiple aliases per agent directory
    // existed carry no 'manifest' field; those entries own the legacy
    // per-agent manifest.
    let manifest_slot = match optional_str(obj, "manifest", alias)?.as_deref() {
        None | Some("legacy") => state::ManifestSlot::Legacy,
        Some("per-alias") => state::ManifestSlot::PerAlias,
        Some(other) => {
            return Err(fail(format!(
                "entry '{alias}' has unsupported manifest '{other}'"
            )));
        }
    };
    Ok(RegistryEntry {
        agent_dir,
        profile,
        topology,
        source,
        git_ref,
        resolved_commit,
        overlay_digest,
        installed_at,
        state: install_state,
        manifest_slot,
    })
}

fn entry_to_json(entry: &RegistryEntry) -> Result<Json, CliError> {
    let mut obj = Map::new();
    obj.insert(
        "agent_dir".to_string(),
        Json::String(utf8_path(&entry.agent_dir, "agent_dir")?.to_string()),
    );
    if let Some(profile) = &entry.profile {
        obj.insert("profile".to_string(), Json::String(profile.clone()));
    }
    obj.insert("topology".to_string(), Json::String(entry.topology.clone()));
    obj.insert("source".to_string(), source_to_json(&entry.source)?);
    if let Some(git_ref) = &entry.git_ref {
        obj.insert("ref".to_string(), Json::String(git_ref.clone()));
    }
    if let Some(commit) = &entry.resolved_commit {
        obj.insert("resolved_commit".to_string(), Json::String(commit.clone()));
    }
    if let Some(digest) = &entry.overlay_digest {
        obj.insert("overlay_digest".to_string(), Json::String(digest.clone()));
    }
    obj.insert(
        "installed_at".to_string(),
        Json::String(entry.installed_at.clone()),
    );
    obj.insert(
        "state".to_string(),
        Json::String(entry.state.as_str().to_string()),
    );
    // Only the non-default slot is written: legacy entries stay
    // byte-identical to registries written before multiple aliases per
    // agent directory existed.
    if entry.manifest_slot == state::ManifestSlot::PerAlias {
        obj.insert(
            "manifest".to_string(),
            Json::String("per-alias".to_string()),
        );
    }
    Ok(Json::Object(obj))
}

fn source_from_json(alias: &str, value: Option<&Json>) -> Result<SourceSpec, CliError> {
    let obj = value
        .and_then(Json::as_object)
        .ok_or_else(|| fail(format!("entry '{alias}' has a missing source")))?;
    reject_unknown(
        obj,
        &["kind", "owner", "repo", "path"],
        &format!("source of '{alias}'"),
    )?;
    let kind = require_str(obj, "kind", alias)?;
    match kind {
        "github" => {
            let owner = require_str(obj, "owner", alias)?.to_string();
            let repo = require_str(obj, "repo", alias)?.to_string();
            for part in [&owner, &repo] {
                let valid = !part.is_empty()
                    && part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                    && !part.contains("..");
                if !valid {
                    return Err(fail(format!(
                        "entry '{alias}' has an invalid github source \
                         'github:{owner}/{repo}'"
                    )));
                }
            }
            Ok(SourceSpec::GitHub { owner, repo })
        }
        "local" => {
            let raw = require_str(obj, "path", alias)?;
            if raw.is_empty() {
                return Err(fail(format!("entry '{alias}' has an empty local path")));
            }
            if !Path::new(raw).is_absolute() {
                return Err(fail(format!(
                    "entry '{alias}' has a non-absolute local path"
                )));
            }
            Ok(SourceSpec::Local {
                path: PathBuf::from(raw),
            })
        }
        other => Err(fail(format!(
            "entry '{alias}' has unsupported source kind '{other}'"
        ))),
    }
}

fn source_to_json(source: &SourceSpec) -> Result<Json, CliError> {
    Ok(match source {
        SourceSpec::GitHub { owner, repo } => serde_json::json!({
            "kind": "github",
            "owner": owner,
            "repo": repo,
        }),
        SourceSpec::Local { path } => serde_json::json!({
            "kind": "local",
            "path": utf8_path(path, "local source path")?,
        }),
    })
}

/// The UTF-8 string form of `path` for JSON persistence.
///
/// `Path::display()` replaces non-UTF-8 bytes with U+FFFD, which can
/// collapse distinct paths onto one registry value and reload a different
/// directory than was installed. The registry stores paths as UTF-8 JSON
/// strings, so a path that cannot be represented that way is rejected with
/// a clear error before anything is persisted, rather than silently
/// corrupted.
fn utf8_path<'a>(path: &'a Path, what: &str) -> Result<&'a str, CliError> {
    path.as_os_str().to_str().ok_or_else(|| {
        fail(format!(
            "{what} '{}' is not valid UTF-8 and cannot be represented \
             in the registry without corruption; refusing to persist it",
            path.display()
        ))
    })
}

/// `true` if `text` is exactly `len` lowercase-or-uppercase hex digits.
fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Reject unknown fields so corruption or a newer schema fails closed.
fn reject_unknown(obj: &Map<String, Json>, known: &[&str], what: &str) -> Result<(), CliError> {
    for key in obj.keys() {
        if !known.contains(&key.as_str()) {
            return Err(fail(format!("{what} has unknown field '{key}'")));
        }
    }
    Ok(())
}

fn require_str<'a>(
    obj: &'a Map<String, Json>,
    key: &str,
    alias: &str,
) -> Result<&'a str, CliError> {
    obj.get(key).and_then(Json::as_str).ok_or_else(|| {
        fail(format!(
            "entry '{alias}' has a missing or non-string '{key}'"
        ))
    })
}

fn optional_str(
    obj: &Map<String, Json>,
    key: &str,
    alias: &str,
) -> Result<Option<String>, CliError> {
    match obj.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(fail(format!("entry '{alias}' has a non-string '{key}'"))),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    fn registry_with_entry(agent_dir: PathBuf, source: SourceSpec) -> Registry {
        let mut registry = Registry::empty();
        registry
            .insert(
                "sample",
                RegistryEntry {
                    agent_dir,
                    profile: None,
                    topology: "sample-topology".to_string(),
                    source,
                    git_ref: None,
                    resolved_commit: None,
                    overlay_digest: None,
                    installed_at: "2026-01-02T03:04:05.000000".to_string(),
                    state: InstallState::Installed,
                    manifest_slot: state::ManifestSlot::Legacy,
                },
            )
            .expect("valid test alias");
        registry
    }

    #[test]
    fn serialize_registry_rejects_non_utf8_agent_dir() {
        let agent_dir = PathBuf::from(OsString::from_vec(b"/tmp/agent-\xff".to_vec()));
        let registry = registry_with_entry(
            agent_dir,
            SourceSpec::GitHub {
                owner: "owner".to_string(),
                repo: "repo".to_string(),
            },
        );

        assert!(serialize_registry(&registry).is_err());
    }

    #[test]
    fn serialize_registry_rejects_non_utf8_local_source() {
        let source_path = PathBuf::from(OsString::from_vec(b"/tmp/source-\xff".to_vec()));
        let registry = registry_with_entry(
            PathBuf::from("/tmp/agent"),
            SourceSpec::Local { path: source_path },
        );

        assert!(serialize_registry(&registry).is_err());
    }

    #[test]
    fn serialize_registry_preserves_utf8_json_representation() {
        let registry = registry_with_entry(
            PathBuf::from("/tmp/agent"),
            SourceSpec::Local {
                path: PathBuf::from("/tmp/source"),
            },
        );

        let serialized = serialize_registry(&registry).expect("UTF-8 paths serialize");
        assert_eq!(
            serialized,
            b"{\n  \"aliases\": {\n    \"sample\": {\n      \"agent_dir\": \"/tmp/agent\",\n      \"installed_at\": \"2026-01-02T03:04:05.000000\",\n      \"source\": {\n        \"kind\": \"local\",\n        \"path\": \"/tmp/source\"\n      },\n      \"state\": \"installed\",\n      \"topology\": \"sample-topology\"\n    }\n  },\n  \"version\": 1\n}\n"
        );
    }
}
