//! Update command: refresh one registry install ID to the current source pin.
//!
//! Unlike install, update never discovers profiles and never touches other
//! installs: it loads the registry entry recorded under the given install
//! ID and re-applies exactly that install (same agent directory, same
//! profile, same topology) against the current source state.
//!
//! Decision path, per install ID:
//!
//! * `--ref` on a local source is rejected (local trees are used exactly
//!   as they are on disk).
//! * With no `--ref` override, a healthy ownership manifest and an
//!   [`Freshness::UpToDate`] status mean the saved ref still resolves to
//!   the recorded pin/digest: the command is a no-op without any clone or
//!   checkout.
//! * A manifest whose recorded topology disagrees with the registry entry
//!   fails closed with an actionable error before any `--ref` validation,
//!   status resolution, or source checkout — never after a cache write; a
//!   missing manifest simply proceeds to reinstall.
//! * A `--ref` override always resolves that ref and refreshes the
//!   recorded ref — even when it resolves to the already-recorded commit.
//! * A `--version` argument is an exact assertion on the resolved
//!   topology's declared metadata version, validated before any registry
//!   or target mutation; the up-to-date shortcut never applies when it is
//!   given.
//! * Every update parses the resolved topology's declared version as
//!   strict SemVer (an invalid declaration fails closed before any
//!   mutation), and an ownership manifest that records a valid installed
//!   version rejects a lower resolved version as a downgrade — with or
//!   without `--version` — before the registry, cache pin, or target
//!   changes. A manifest that predates version recording has no installed
//!   version to compare, so it follows the upgrade path instead.
//! * Otherwise the source is resolved (`resolve` with the persistent cache
//!   root for a real run, an ephemeral checkout for `--dry-run`), the
//!   topology is located in the checkout, and [`install::run`] re-applies
//!   it with all output captured — nothing is printed directly.
//!
//! Before any real run mutates the target, the update precomputes every
//! fallible piece of post-install metadata (the local overlay digest) so a
//! failure aborts while both the target and the registry are untouched.
//! It then acquires [`registry::RegistryLock`], revalidates under the lock
//! that the install ID still holds exactly the entry the plan was built from,
//! and stages the intended entry as [`registry::InstallState::Partial`]. The lock is
//! held across [`install::run`] so a concurrent retarget or remove cannot
//! interleave with the reinstall. Only after the install lands is the
//! staged entry flipped to [`registry::InstallState::Installed`]; if the install
//! fails or the final write fails, the `Partial` marker remains truthful —
//! the ownership manifest is authoritative for what actually exists, and
//! the next run reconciles or reinstalls. Finally, bounded cache pruning
//! runs for that same GitHub source on the success path and on every
//! failure path after the persistent checkout was created, retaining every
//! commit still referenced by the registry as it stands after the attempt
//! plus this checkout's resolved commit; a prune failure is a warning line
//! in the output, never a command failure. Dry-run checkouts are
//! ephemeral and are never pruned; local sources are unaffected.

use std::collections::BTreeSet;
use std::path::Path;

use crate::error::CliError;
use crate::install;
use crate::registry::{self, RegistryEntry, SourceSpec};
use crate::source::{self, TopologySource};
use crate::state;
use crate::status::{self, Freshness};
use crate::topology;

/// Cache checkouts retained per GitHub source beyond those still
/// referenced by the registry (see [`source::prune_cache`]).
const CACHE_KEEP_UNREFERENCED: usize = 10;

pub struct UpdateOptions<'a> {
    /// Stable install ID to update (never "all profiles").
    pub install_id: &'a str,
    /// Ref override (branch, tag, or full 40-hex SHA); GitHub sources only.
    /// When given, the registry's recorded ref is refreshed even if the
    /// resolved commit is unchanged.
    pub reference: Option<&'a str>,
    /// Optional source replacement (`github:OWNER/REPO`, a full GitHub
    /// HTTPS URL, or a local path). When given, the update fetches from
    /// the new source, but the registry commits it only after the install
    /// lands; a failed update keeps the previously recorded source.
    pub source: Option<&'a str>,
    /// Exact topology metadata version assertion: when given, the resolved
    /// topology's declared `version` must equal this version exactly, checked
    /// before any registry or target mutation. This is never a range and is
    /// never compared against the previously recorded version. The CLI parses
    /// the flag as strict SemVer, so an unparsable value is rejected before
    /// any source resolution or state access.
    pub version: Option<&'a semver::Version>,
    /// Config handling for this update only; never remembered.
    pub config_mode: crate::config_mode::ConfigMode,
    /// Stable XDG state anchor for the registry document and ownership
    /// manifest paths.
    pub state_anchor: &'a Path,
    /// Plan only: ephemeral checkout, no registry/cache/target writes.
    pub dry_run: bool,
}

/// Update the single registry install ID named in `opts`; every
/// human-readable line lands in `out`, nothing is printed here. Returns
/// the process exit code: `0` for updated, already-current, and dry-run
/// plans.
pub fn run(opts: &UpdateOptions<'_>, out: &mut Vec<String>) -> Result<i32, CliError> {
    let registry_doc = registry::load_registry(opts.state_anchor)?;
    let entry = registry_doc.get(opts.install_id).cloned().ok_or_else(|| {
        CliError::Usage(format!(
            "unknown install ID '{}' (see the registry under {})",
            opts.install_id,
            registry::registry_path(opts.state_anchor)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "the state root".to_string())
        ))
    })?;

    // The ownership manifest this install ID owns: a per-install-ID
    // manifest when several install IDs share the agent directory, else
    // the legacy default.
    let manifest_path = state::manifest_path_for_slot(
        &entry.agent_dir,
        opts.state_anchor,
        opts.install_id,
        entry.manifest_slot,
    )?;

    // Fail closed on a mismatched manifest before any --ref validation,
    // status resolution (which may hit the network), or source checkout:
    // the manifest would make install refuse mid-transaction anyway, and
    // nothing may be written first. A missing manifest is reinstall-safe
    // and proceeds; an unreadable manifest errors out of load_manifest.
    let manifest_name = manifest_topology(&manifest_path, opts.state_anchor)?;
    // A missing manifest is reinstall-safe: the install below
    // recreates it together with the managed files and config.
    let manifest_missing = manifest_name.is_none();
    match manifest_name {
        None => {}
        Some(name) if name == entry.topology => {}
        Some(other) => {
            return Err(CliError::Usage(format!(
                "refusing to update install ID '{}': the ownership manifest in {} \
                 records topology '{other}' but the registry expects '{}'; fix \
                 the mismatch manually, or remove and reinstall the install",
                opts.install_id,
                entry.agent_dir.display(),
                entry.topology
            )));
        }
    }

    // A `--source` override redirects this update to a new source; the
    // registry still commits it only after the install lands (the Partial
    // staging below keeps the old source until then), so a failed update
    // leaves the previously recorded source in place.
    let source_spec = match opts.source {
        Some(spec) => TopologySource::parse(spec)?,
        None => topology_source(&entry.source),
    };
    let spec_text = source_spec.to_string();
    if let Some(reference) = &opts.reference
        && matches!(source_spec, TopologySource::Local { .. })
    {
        return Err(CliError::Usage(format!(
            "--ref '{reference}' is only valid for 'github:' sources; local \
             source '{}' is used exactly as it is on disk",
            spec_text
        )));
    }

    // Effective ref: the explicit override wins; otherwise the ref recorded
    // at install time is re-resolved so a moving branch is followed. A ref
    // recorded for a GitHub source never carries over onto a local source
    // switch: the local tree is used exactly as it is on disk.
    let effective_ref = match &source_spec {
        TopologySource::GitHub { .. } => opts.reference.or(entry.git_ref.as_deref()),
        TopologySource::Local { .. } => None,
    };

    // A source override always re-resolves and re-installs: the recorded
    // pin belongs to the old source, so the up-to-date shortcut cannot
    // apply. A `--version` assertion also forces re-resolution: the
    // declared metadata version must be read from the resolved source
    // before it can be validated.
    if opts.reference.is_none() && opts.source.is_none() && opts.version.is_none() {
        // With the manifest healthy (present and matching) only the
        // freshness half of the status is needed for the skip decision;
        // a missing manifest must not skip, even when the pin is still
        // current, because the install below has to recreate it.
        let current = status::status_for_entry(opts.install_id, &entry, opts.state_anchor)?;
        if !manifest_missing
            && entry.state == registry::InstallState::Installed
            && matches!(current.freshness, Freshness::UpToDate { .. })
        {
            let pin = match &entry.source {
                SourceSpec::GitHub { .. } => entry
                    .resolved_commit
                    .clone()
                    .unwrap_or_else(|| "unknown commit".to_string()),
                SourceSpec::Local { path } => path.display().to_string(),
            };
            out.push(format!(
                "install ID '{}' is already up to date ({} at {pin})",
                opts.install_id, entry.topology,
            ));
            return Ok(0);
        }
    }

    // The pin moved, the manifest is missing, or a ref override was given:
    // resolve the source. Real runs cache persistently; dry runs clone
    // into a temporary directory that SourceCheckout removes on drop.
    let cache_root = if opts.dry_run {
        None
    } else {
        Some(state::state_root(opts.state_anchor)?.join("sources"))
    };
    let checkout = source::resolve(&spec_text, effective_ref, cache_root.as_deref())?;
    // Capture the resolved commit before anything can fail so every
    // cleanup path below can retain this exact checkout.
    let resolved_commit = checkout.commit.clone();
    // find_topology resolves either a root package or one catalog entry;
    // pass it the original source root.
    let topology_dir = match topology::find_topology(&checkout.root, &entry.topology, true) {
        Ok(dir) => dir,
        Err(err) => {
            // A failed lookup must not leave the freshly fetched
            // persistent checkout behind; dry-run checkouts are ephemeral
            // and prune themselves on drop.
            if !opts.dry_run {
                prune_source_cache(
                    &source_spec,
                    resolved_commit.as_deref(),
                    opts.state_anchor,
                    out,
                );
            }
            return Err(err);
        }
    };

    // SemVer gate, before any registry or target mutation so every
    // refusal below leaves both untouched:
    //
    // * The resolved topology's declared `version` must be strict SemVer;
    //   an unparsable declaration fails closed instead of being compared
    //   ad hoc. A requested `--version` must be strict SemVer too, and —
    //   as before — must equal the declared version exactly (an assertion,
    //   never a range).
    // * An ownership manifest that records a valid installed version must
    //   not be downgraded: a lower resolved version is refused with both
    //   versions named. A manifest that predates version recording (no
    //   `topology_version`) has nothing to compare against and proceeds.
    // * An installed version that is present but unusable — a non-string
    //   value, or a string that is not valid SemVer — fails closed with
    //   [`CliError::Manifest`]: installs always record strict SemVer here,
    //   so such a manifest was edited or corrupted, and ordering against
    //   it would be guesswork. The refusal names the install ID, the
    //   `topology_version` field, and the offending value, and happens
    //   before any registry or target mutation.
    let manifest = match state::load_manifest_at(&manifest_path, opts.state_anchor) {
        Ok(manifest) => manifest,
        Err(err) => {
            if !opts.dry_run {
                prune_source_cache(
                    &source_spec,
                    resolved_commit.as_deref(),
                    opts.state_anchor,
                    out,
                );
            }
            return Err(err);
        }
    };
    let declared = match topology::topology_metadata(&topology_dir) {
        Ok(meta) => topology::meta_version(&meta),
        Err(err) => {
            if !opts.dry_run {
                prune_source_cache(
                    &source_spec,
                    resolved_commit.as_deref(),
                    opts.state_anchor,
                    out,
                );
            }
            return Err(err);
        }
    };
    let source_version = match semver::Version::parse(&declared) {
        Ok(version) => version,
        Err(err) => {
            if !opts.dry_run {
                prune_source_cache(
                    &source_spec,
                    resolved_commit.as_deref(),
                    opts.state_anchor,
                    out,
                );
            }
            return Err(CliError::Topology(format!(
                "refusing to update install ID '{}': topology '{}' declares version \
                 '{declared}', which is not valid SemVer ({err}); fix the \
                 topology's topology.yml before updating",
                opts.install_id, entry.topology
            )));
        }
    };
    if let Some(expected) = opts.version
        && *expected != source_version
    {
        if !opts.dry_run {
            prune_source_cache(
                &source_spec,
                resolved_commit.as_deref(),
                opts.state_anchor,
                out,
            );
        }
        return Err(CliError::Usage(format!(
            "refusing to update install ID '{}': requested version \
             '{expected}' but topology '{}' declares version \
             '{declared}' at source '{spec_text}'; pass the declared \
             version or drop --version to accept it",
            opts.install_id, entry.topology
        )));
    }
    if let Some(manifest) = manifest.as_ref()
        && let Some(recorded) = manifest.get("topology_version")
    {
        let installed = match recorded {
            serde_json::Value::String(raw) => match semver::Version::parse(raw) {
                Ok(version) => version,
                Err(err) => {
                    if !opts.dry_run {
                        prune_source_cache(
                            &source_spec,
                            resolved_commit.as_deref(),
                            opts.state_anchor,
                            out,
                        );
                    }
                    return Err(CliError::Manifest(format!(
                        "refusing to update install ID '{}': the installed \
                         manifest in {} records topology_version '{raw}', \
                         which is not valid SemVer ({err}); fix the \
                         installed manifest or remove and reinstall the \
                         install to replace the recorded version",
                        opts.install_id,
                        manifest_path.display(),
                    )));
                }
            },
            other => {
                if !opts.dry_run {
                    prune_source_cache(
                        &source_spec,
                        resolved_commit.as_deref(),
                        opts.state_anchor,
                        out,
                    );
                }
                return Err(CliError::Manifest(format!(
                    "refusing to update install ID '{}': the installed manifest \
                     in {} records topology_version as {other}, which must \
                     be a SemVer string; fix the installed manifest or \
                     remove and reinstall the install to replace the \
                     recorded version",
                    opts.install_id,
                    manifest_path.display(),
                )));
            }
        };
        if source_version < installed {
            if !opts.dry_run {
                prune_source_cache(
                    &source_spec,
                    resolved_commit.as_deref(),
                    opts.state_anchor,
                    out,
                );
            }
            return Err(CliError::Usage(format!(
                "refusing to update install ID '{}': downgrade from installed \
                 version {installed} to source version {declared} at source \
                 '{spec_text}'; reinstall a newer topology, or remove and \
                 reinstall the install to replace the installed topology",
                opts.install_id
            )));
        }
    }

    let agent_dir_text = entry.agent_dir.to_string_lossy().into_owned();

    // Precompute every fallible piece of post-install metadata BEFORE any
    // target mutation: if the local overlay digest cannot be computed, the
    // update must abort while both the target and the registry are still
    // untouched. GitHub drift is tracked by the resolved commit pin, which
    // the checkout already carries; local drift by the fresh overlay
    // digest.
    let overlay_digest = match &source_spec {
        TopologySource::Local { .. } => Some(source::overlay_digest(&topology_dir)?),
        TopologySource::GitHub { .. } => None,
    };

    // A replace-mode update swaps the whole active config: refuse before
    // staging a Partial registry entry or taking the lock when the
    // replacement would overwrite or remove config keys a sibling install
    // owns. install::run repeats the check under the lock before
    // mutating, so ownership recorded in between still fails closed.
    if matches!(opts.config_mode, crate::config_mode::ConfigMode::Replace)
        && let Err(err) = install::preflight_replace_ownership(
            &topology_dir,
            opts.state_anchor,
            &entry.agent_dir,
            &manifest_path,
        )
    {
        if !opts.dry_run {
            prune_source_cache(
                &source_spec,
                resolved_commit.as_deref(),
                opts.state_anchor,
                out,
            );
        }
        return Err(err);
    }

    if opts.dry_run {
        // Plan only: no lock, no registry write, no target mutation —
        // install::run plans against the same ephemeral checkout.
        let mut install_out = Vec::new();
        let code = match install::run(
            &install::InstallOptions {
                topology: &entry.topology,
                topology_root: &checkout.root,
                state_anchor: opts.state_anchor,
                agent_dir: Some(&agent_dir_text),
                skills_dir: None,
                dry_run: true,
                config_mode: opts.config_mode,
                manifest_path: Some(&manifest_path),
                install_id: Some(opts.install_id),
                // Update already asserted the exact version above; the
                // reinstall only re-records the declared metadata string.
                expected_version: None,
            },
            &mut install_out,
        ) {
            Ok(code) => code,
            Err(err) => {
                out.append(&mut install_out);
                return Err(err);
            }
        };
        out.append(&mut install_out);
        if code != 0 {
            return Ok(code);
        }
        out.push(format!(
            "dry-run: would update install ID '{}'",
            opts.install_id
        ));
        return Ok(0);
    }

    // Real run: the registry stays truthful through the whole mutation by
    // staging the intended entry as Partial before touching the target and
    // flipping it to Installed only after the install lands. For GitHub
    // sources the resolved commit (and the requested ref, when given) is
    // recorded; for local sources the fresh overlay digest is recorded.
    let mut intended = RegistryEntry::new(
        entry.agent_dir.clone(),
        entry.profile.clone(),
        entry.topology.clone(),
        registry_source(&source_spec),
        match &source_spec {
            TopologySource::GitHub { .. } => opts
                .reference
                .map(str::to_string)
                .or_else(|| entry.git_ref.clone()),
            TopologySource::Local { .. } => None,
        },
        resolved_commit.clone(),
        overlay_digest,
    );
    // An update re-applies an existing install, so the original
    // installation time stays semantically stable — only the recorded
    // source pin/digest changes.
    intended.installed_at = entry.installed_at.clone();
    intended.state = registry::InstallState::Partial;
    // The ownership slot is immutable across an update: a per-install-ID
    // manifest stays per-install-ID, the legacy manifest stays legacy.
    intended.manifest_slot = entry.manifest_slot;

    // Hold the registry lock across the entire target mutation so a
    // concurrent retarget or remove cannot interleave with the reinstall.
    let lock = match registry::RegistryLock::acquire(opts.state_anchor) {
        Ok(lock) => lock,
        Err(err) => {
            // Another command may hold the lock while working from a
            // cache checkout it has not registered yet; pruning without
            // the lock could delete a checkout in active use. Cleanup
            // runs on the next successful mutation instead.
            out.push(
                "warning: source cache cleanup deferred while the registry \
                 is busy; it will run on the next successful update"
                    .to_string(),
            );
            return Err(err);
        }
    };

    // Revalidate under the lock: the install ID must still hold exactly
    // the entry this plan was built from; a disappearance or any
    // divergence means the install ID changed concurrently and must fail
    // clearly instead of clobbering another target. While the lock is
    // held the registry cannot change, so the prune below reloads exactly
    // the resulting state.
    let mut doc = match registry::load_registry(opts.state_anchor) {
        Ok(doc) => doc,
        Err(err) => {
            prune_source_cache(
                &source_spec,
                resolved_commit.as_deref(),
                opts.state_anchor,
                out,
            );
            return Err(err);
        }
    };
    let Some(current) = doc.get(opts.install_id) else {
        prune_source_cache(
            &source_spec,
            resolved_commit.as_deref(),
            opts.state_anchor,
            out,
        );
        return Err(CliError::Registry(format!(
            "registry entry for install ID '{}' disappeared during the update; \
             refusing to recreate it implicitly",
            opts.install_id
        )));
    };
    if *current != entry {
        let state_text = if current.state == registry::InstallState::Installed {
            "installed"
        } else {
            "partial"
        };
        prune_source_cache(
            &source_spec,
            resolved_commit.as_deref(),
            opts.state_anchor,
            out,
        );
        return Err(CliError::Registry(format!(
            "registry entry for install ID '{}' changed concurrently (now {} -> {}, \
             state {state_text}); refusing to overwrite it",
            opts.install_id,
            current.agent_dir.display(),
            current.topology,
        )));
    }

    // Stage a Partial marker before the target mutation; if this write
    // fails, neither the registry nor the target has been changed. The
    // staged entry keeps the previously recorded source with only the
    // state set to Partial: a failed source switch must not retain the
    // new source, and the intended entry commits only after the install
    // lands.
    let mut staged = entry.clone();
    staged.state = registry::InstallState::Partial;
    if let Err(err) = doc.insert(opts.install_id, staged) {
        prune_source_cache(
            &source_spec,
            resolved_commit.as_deref(),
            opts.state_anchor,
            out,
        );
        return Err(err);
    }
    if let Err(err) = registry::write_registry(opts.state_anchor, &doc) {
        prune_source_cache(
            &source_spec,
            resolved_commit.as_deref(),
            opts.state_anchor,
            out,
        );
        return Err(err);
    }

    let mut install_out = Vec::new();
    let code = match install::run(
        &install::InstallOptions {
            topology: &entry.topology,
            topology_root: &checkout.root,
            state_anchor: opts.state_anchor,
            agent_dir: Some(&agent_dir_text),
            skills_dir: None,
            dry_run: false,
            config_mode: opts.config_mode,
            manifest_path: Some(&manifest_path),
            install_id: Some(opts.install_id),
            // Update already asserted the exact version above; the
            // reinstall only re-records the declared metadata string.
            expected_version: None,
        },
        &mut install_out,
    ) {
        Ok(code) => code,
        // Keep whatever the install managed to report (and note any
        // side-effect lines) before propagating the failure; the staged
        // Partial entry above stays truthful for the rerun.
        Err(err) => {
            out.append(&mut install_out);
            // The cache prune still runs so the checkout this attempt
            // fetched does not accumulate across failures.
            prune_source_cache(
                &source_spec,
                resolved_commit.as_deref(),
                opts.state_anchor,
                out,
            );
            return Err(err);
        }
    };
    out.append(&mut install_out);
    if code != 0 {
        // The staged Partial entry stays in place: it truthfully says the
        // update did not complete, and the ownership manifest remains the
        // authority on what actually exists. The lock drops when this
        // returns, after the cache prune below.
        out.push(format!(
            "warning: update of install ID '{}' did not complete; the registry now \
             records it as 'partial' (the ownership manifest is authoritative); \
             rerun the update to finish",
            opts.install_id
        ));
        prune_source_cache(
            &source_spec,
            resolved_commit.as_deref(),
            opts.state_anchor,
            out,
        );
        return Ok(code);
    }

    // The install landed: flip Partial -> Installed while still holding
    // the lock. The in-memory doc is still current — no other writer can
    // touch the registry while the lock is held — so this is a plain
    // rewrite of the staged document. If the write itself fails, the
    // staged Partial entry remains, which is still truthful, and the next
    // run reconciles or reinstalls.
    intended.state = registry::InstallState::Installed;
    if let Err(err) = doc.insert(opts.install_id, intended) {
        prune_source_cache(
            &source_spec,
            resolved_commit.as_deref(),
            opts.state_anchor,
            out,
        );
        return Err(err);
    }
    if let Err(err) = registry::write_registry(opts.state_anchor, &doc) {
        prune_source_cache(
            &source_spec,
            resolved_commit.as_deref(),
            opts.state_anchor,
            out,
        );
        return Err(err);
    }

    // Commits are collected from the just-written registry (inside the
    // lock) so the freshly resolved pin is retained by the prune. The
    // filter must use the committed `source_spec`, not the pre-update
    // `entry.source`: after a source switch the registry's pins for the
    // new source (including this install's fresh commit) are what the
    // prune below must keep.
    let mut referenced = BTreeSet::new();
    if let TopologySource::GitHub { .. } = source_spec {
        referenced.extend(
            doc.aliases
                .values()
                .filter(|other| topology_source(&other.source) == source_spec)
                .filter_map(|other| other.resolved_commit.clone()),
        );
    }
    drop(lock);

    out.push(format!(
        "updated install ID '{}' ({} at {})",
        opts.install_id,
        entry.topology,
        entry.agent_dir.display()
    ));

    // Bounded cache cleanup for this GitHub source only, after the
    // successful registry mutation; a prune failure is a warning, never a
    // failure of the update itself.
    if let TopologySource::GitHub { .. } = source_spec {
        let cache_root = state::state_root(opts.state_anchor)?.join("sources");
        if let Err(exc) = source::prune_cache(
            &cache_root,
            &source_spec,
            &referenced,
            CACHE_KEEP_UNREFERENCED,
        ) {
            out.push(format!("warning: could not prune source cache: {exc}"));
        }
    }
    Ok(0)
}

/// Bounded cache cleanup for a GitHub source once its persistent checkout
/// has served its purpose — including when a later step of the update
/// failed and the checkout would otherwise be left behind for the next
/// attempt to pile onto. The referenced commits are recomputed from the
/// registry as it stands at call time, which is truthful for the
/// resulting state on every path (under the lock nothing can have changed
/// it since), and are always extended with this checkout's resolved
/// commit, when it has one, so the current pin survives even when the
/// registry mutation failed or never happened; a pin the registry does
/// not already reference counts against the unreferenced bound, so the
/// cache never retains more than `CACHE_KEEP_UNREFERENCED` checkouts
/// beyond those the registry references. When the registry cannot be
/// reloaded the prune is skipped entirely rather than risk deleting a
/// commit some other install still references. Every failure is a warning
/// line in `out`, never a change to the command's result; local sources
/// and dry-run checkouts never reach here.
fn prune_source_cache(
    source_spec: &TopologySource,
    resolved_commit: Option<&str>,
    state_anchor: &Path,
    out: &mut Vec<String>,
) {
    if !matches!(source_spec, TopologySource::GitHub { .. }) {
        return;
    }
    let mut referenced: BTreeSet<String> = match registry::load_registry(state_anchor) {
        Ok(doc) => doc
            .aliases
            .values()
            .filter(|other| topology_source(&other.source) == *source_spec)
            .filter_map(|other| other.resolved_commit.clone())
            .collect(),
        Err(exc) => {
            out.push(format!(
                "warning: could not reload the registry to prune the source cache: {exc}"
            ));
            return;
        }
    };
    // The current checkout's pin must survive even when the registry
    // mutation failed or never happened. When the registry does not
    // already reference it, that retained checkout counts against the
    // unreferenced bound so the total kept beyond registry references
    // stays within CACHE_KEEP_UNREFERENCED.
    let mut keep_unreferenced = CACHE_KEEP_UNREFERENCED;
    if let Some(commit) = resolved_commit
        && referenced.insert(commit.to_ascii_lowercase())
    {
        keep_unreferenced = keep_unreferenced.saturating_sub(1);
    }
    let cache_root = match state::state_root(state_anchor) {
        Ok(root) => root.join("sources"),
        Err(exc) => {
            out.push(format!(
                "warning: could not resolve source cache root: {exc}"
            ));
            return;
        }
    };
    if let Err(exc) = source::prune_cache(&cache_root, source_spec, &referenced, keep_unreferenced)
    {
        out.push(format!("warning: could not prune source cache: {exc}"));
    }
}

/// The topology name recorded in the given ownership manifest, if a
/// manifest exists. Purely local and read-only — this is the same probe
/// `status::manifest_status` performs, split out so update can refuse a
/// mismatched manifest before any network or cache activity. A missing
/// manifest yields `None` (reinstall-safe); an unreadable manifest is a
/// hard error from `state::load_manifest_at`.
fn manifest_topology(
    manifest_path: &Path,
    state_anchor: &Path,
) -> Result<Option<String>, CliError> {
    let Some(manifest) = state::load_manifest_at(manifest_path, state_anchor)? else {
        return Ok(None);
    };
    Ok(topology::manifest_topology(&manifest).map(str::to_string))
}

/// Map the resolver's source type back onto the registry's recorded
/// source spec; both are the same `github:`/local pair of shapes.
fn registry_source(source: &TopologySource) -> SourceSpec {
    match source {
        TopologySource::GitHub { owner, repo } => SourceSpec::GitHub {
            owner: owner.clone(),
            repo: repo.clone(),
        },
        TopologySource::Local { path } => SourceSpec::Local { path: path.clone() },
    }
}

/// Map the registry's recorded source spec onto the resolver's source
/// type; both are the same `github:`/local pair of shapes.
fn topology_source(spec: &SourceSpec) -> TopologySource {
    match spec {
        SourceSpec::GitHub { owner, repo } => TopologySource::GitHub {
            owner: owner.clone(),
            repo: repo.clone(),
        },
        SourceSpec::Local { path } => TopologySource::Local { path: path.clone() },
    }
}
