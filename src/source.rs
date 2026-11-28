//! Topology source resolution and caching.
//!
//! A source is either one root-level package (`topology.yml` and `agent/`)
//! or a legacy catalog with `topologies/<slug>/` packages. GitHub sources
//! are pinned and cached; local sources are canonicalized and used in place.
//!
//! * `github:OWNER/REPO` — fetched from GitHub with the Git CLI and pinned
//!   to a full 40-hex commit SHA. Persistent checkouts are staged into a
//!   temporary sibling and renamed into place, so a crash never leaves a
//!   half-cloned cache entry. Callers provide the cache root (expected to
//!   live under the XDG state root); bounded cache pruning is a separate
//!   integration concern.
//! * a local path — canonicalized and used in place. No commit pin applies,
//!   so local drift is detected with [`overlay_digest`].
//!
//! Git is always invoked through argument arrays (never a shell), with
//! interactive credential prompts disabled. `--ref` accepts a branch name,
//! tag name, or full 40-hex commit SHA; omitting it resolves the remote's
//! default `HEAD`. Resolution without a cache root (dry runs) clones into a
//! temporary directory that [`SourceCheckout`] removes on drop, leaving no
//! persistent cache behind.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{CliError, io_err};
use crate::fsx;
use crate::topology;

/// Marker file inside a completed cache checkout; its presence is what
/// makes a cache entry trustworthy, so an interrupted clone can never be
/// mistaken for a finished one.
const CHECKOUT_MARKER: &str = ".omp-topology-checkout";

/// Active-use lease file inside a cache checkout. Every process reading a
/// checkout holds a shared OS advisory lock on this file from resolution
/// until its [`SourceCheckout`] drops; cache pruning takes the same file
/// exclusively (non-blocking) and holds the claim across the whole
/// removal, so an in-use checkout is never deleted. The kernel releases
/// the lock when the descriptor closes — on orderly drop and on process
/// crash alike — so a crashed holder can never leave a live lease behind;
/// the inert file that remains afterwards is indistinguishable from a
/// never-leased one, and the next prune re-locks it before deleting.
const LEASE_FILE: &str = ".omp-topology-checkout.lease";

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique, unpredictable-in-name-only directory name for staging clones.
fn staging_name(prefix: &str) -> String {
    let sequence = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(".{prefix}-{}-{sequence}", std::process::id())
}

/// A parsed profile-package or legacy-catalog source specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopologySource {
    /// `github:OWNER/REPO`.
    GitHub { owner: String, repo: String },
    /// A canonicalized local path containing a root package or catalog.
    Local { path: PathBuf },
}

impl TopologySource {
    /// Parse `github:OWNER/REPO` or a local path.
    ///
    /// Local paths are expanded (`~`) and canonicalized here, so a
    /// [`TopologySource::Local`] always carries an absolute path containing
    /// either a root `topology.yml` package or a `topologies/` catalog.
    pub fn parse(spec: &str) -> Result<TopologySource, CliError> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err(CliError::Usage(
                "source must not be empty: use 'github:OWNER/REPO' or a local \
                 path containing a root topology.yml or a topologies/ catalog"
                    .to_string(),
            ));
        }
        if let Some(rest) = spec.strip_prefix("github:") {
            return parse_github(spec, rest);
        }
        if spec.starts_with("http://") || spec.starts_with("https://") {
            return parse_github_url(spec);
        }
        parse_local(spec)
    }
}

impl fmt::Display for TopologySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TopologySource::GitHub { owner, repo } => write!(f, "github:{owner}/{repo}"),
            TopologySource::Local { path } => write!(f, "{}", path.display()),
        }
    }
}

/// Validate `github:OWNER/REPO` pieces against GitHub's naming rules.
fn parse_github(spec: &str, rest: &str) -> Result<TopologySource, CliError> {
    let invalid = || {
        CliError::Usage(format!(
            "invalid GitHub source '{spec}': expected 'github:OWNER/REPO' with \
             a valid owner and repository name (letters, digits, '-', '_', '.')"
        ))
    };
    let (owner, repo) = rest.split_once('/').ok_or_else(invalid)?;
    let owner_ok = !owner.is_empty()
        && owner.len() <= 39
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let repo_ok = !repo.is_empty()
        && !repo.contains("..")
        && !repo.ends_with('.')
        && repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if !owner_ok || !repo_ok {
        return Err(invalid());
    }
    // Lowercase so display and cache identity are canonical regardless of
    // how the user typed the spec (GitHub names are case-insensitive).
    Ok(TopologySource::GitHub {
        owner: owner.to_ascii_lowercase(),
        repo: repo.to_ascii_lowercase(),
    })
}

/// Normalize a full GitHub HTTPS URL (`https://github.com/OWNER/REPO`,
/// with an optional trailing `/` or `.git`) to the canonical
/// `github:OWNER/REPO` identity, so every spelling of the same
/// repository stores, displays, and caches as one source.
fn parse_github_url(spec: &str) -> Result<TopologySource, CliError> {
    let invalid = || {
        CliError::Usage(format!(
            "invalid GitHub URL '{spec}': expected \
             'https://github.com/OWNER/REPO'"
        ))
    };
    let Some(rest) = spec
        .strip_prefix("https://")
        .or_else(|| spec.strip_prefix("http://"))
        .and_then(|url| url.strip_prefix("github.com/"))
    else {
        return Err(invalid());
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    parse_github(spec, rest)
}

/// Canonicalize a local path and require a root package or catalog.
fn parse_local(spec: &str) -> Result<TopologySource, CliError> {
    let expanded = fsx::expanduser(Path::new(spec));
    let canonical = std::fs::canonicalize(&expanded).map_err(|err| {
        CliError::Usage(format!(
            "local source '{spec}' does not exist (resolved to {}): {err}; \
             pass a profile repo or 'github:OWNER/REPO' source",
            expanded.display()
        ))
    })?;
    if !topology::has_source_layout(&canonical)? {
        return Err(CliError::Usage(format!(
            "local source '{spec}' is not a profile or catalog source: {} \
             contains neither a root topology.yml package nor a topologies/ catalog",
            canonical.display()
        )));
    }
    Ok(TopologySource::Local { path: canonical })
}

/// `true` for a full 40-character lowercase/uppercase hex commit SHA.
fn is_full_sha(reference: &str) -> bool {
    reference.len() == 40 && reference.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Reject refs that git could misread as options, patterns, or refspecs.
fn validate_ref(reference: &str) -> Result<(), CliError> {
    let clean = !reference.is_empty()
        && !reference.starts_with('-')
        && !reference.starts_with('/')
        && !reference.ends_with('/')
        && !reference.contains("..")
        && !reference.contains("@{")
        && !reference.ends_with(".lock")
        && reference != "@"
        && reference.bytes().all(|b| {
            b > 0x20
                && b < 0x7f
                && !matches!(b, b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        });
    if !clean {
        return Err(CliError::Usage(format!(
            "invalid --ref '{reference}': use a branch name, tag name, or \
             full 40-hex commit SHA (no leading '-', spaces, '~', '^', ':', \
             '?', '*', '[', or '..' sequences)"
        )));
    }
    Ok(())
}

/// Run `git` with prompts disabled; `label` names the source for errors.
///
/// All arguments are passed as an argv array — no shell is involved — and
/// `GIT_TERMINAL_PROMPT=0` stops git from blocking on a credential prompt.
fn run_git(args: &[&str], label: &str) -> Result<String, CliError> {
    let output = Command::new("git")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                CliError::Usage(format!(
                    "git is required to fetch '{label}' but was not found in \
                     PATH; install Git (https://git-scm.com) and retry"
                ))
            } else {
                io_err(Path::new("git"), err)
            }
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let detail = if stderr.is_empty() {
            format!("git exited with {}", output.status)
        } else {
            stderr.to_string()
        };
        return Err(CliError::Topology(format!(
            "failed to fetch '{label}' via git: {detail}; check that the \
             repository exists, the ref is valid, and network access to \
             github.com is available"
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn github_url(owner: &str, repo: &str) -> String {
    format!("https://github.com/{owner}/{repo}.git")
}

/// Resolve a GitHub source's ref to a full 40-hex commit SHA without any
/// checkout, cache write, or working-tree access — only `git ls-remote`.
///
/// `reference` is a branch name, tag name, or full 40-hex SHA; `None`
/// resolves the remote's default `HEAD`. Local sources have no remote ref
/// to resolve and are rejected. Intended for cheap remote checks such as
/// update availability on `status`.
pub fn resolve_remote_commit(
    source: &TopologySource,
    reference: Option<&str>,
) -> Result<String, CliError> {
    let TopologySource::GitHub { owner, repo } = source else {
        return Err(CliError::Usage(format!(
            "source '{source}' is a local path with no remote ref to resolve; \
             --ref only applies to 'github:' sources"
        )));
    };
    if let Some(reference) = reference {
        validate_ref(reference)?;
    }
    github_resolve_commit(owner, repo, reference)
}

/// Resolve a GitHub ref to a full 40-hex commit SHA.
///
/// Branch and tag names go through `git ls-remote`; a full SHA passes
/// through unverified (the subsequent checkout of the pinned SHA in the
/// staged fetch is the verification). A missing ref resolves the remote's
/// default `HEAD` via `ls-remote --symref`.
fn github_resolve_commit(
    owner: &str,
    repo: &str,
    reference: Option<&str>,
) -> Result<String, CliError> {
    let url = github_url(owner, repo);
    let label = format!("github:{owner}/{repo}");
    match reference {
        Some(reference) if is_full_sha(reference) => Ok(reference.to_ascii_lowercase()),
        Some(reference) => {
            let out = run_git(&["ls-remote", &url, reference], &label)?;
            pick_remote_sha(&out, reference, &label)
        }
        None => {
            let out = run_git(&["ls-remote", "--symref", &url, "HEAD"], &label)?;
            // Normal output is two lines: "ref: refs/heads/<branch>\tHEAD"
            // (the symref pointer, whose first field is a branch name, not
            // a commit) and "<sha>\tHEAD". Only a full 40-hex SHA field
            // can be the HEAD commit, so the symref line must be skipped.
            out.lines()
                .filter_map(|line| line.split_once('\t'))
                .map(|(sha, name)| (sha.trim(), name.trim()))
                .find(|(sha, name)| *name == "HEAD" && is_full_sha(sha))
                .map(|(sha, _)| sha.to_ascii_lowercase())
                .ok_or_else(|| {
                    CliError::Topology(format!(
                        "could not determine the default branch of '{label}': \
                         'git ls-remote --symref' returned no HEAD commit; \
                         pass --ref with a branch, tag, or commit SHA"
                    ))
                })
        }
    }
}

/// Choose the commit for `reference` from `ls-remote` output.
///
/// `ls-remote <pattern>` matches any ref whose name ends with the pattern,
/// so an unambiguous answer is picked deterministically: an exact
/// `refs/heads/` match first, then an exact tag (preferring its peeled
/// `^{}` commit), then a single unambiguous suffix match.
fn pick_remote_sha(out: &str, reference: &str, label: &str) -> Result<String, CliError> {
    let head = format!("refs/heads/{reference}");
    let tag = format!("refs/tags/{reference}");
    let peeled = format!("{tag}^{{}}");
    let mut suffix_matches: Vec<&str> = Vec::new();
    let mut tag_sha: Option<&str> = None;
    let mut peeled_sha: Option<&str> = None;
    for line in out.lines() {
        let Some((sha, name)) = line.split_once('\t') else {
            continue;
        };
        let sha = sha.trim();
        let name = name.trim();
        // Never accept a malformed line as a pin: every candidate must be
        // a full 40-hex SHA before it is considered.
        if !is_full_sha(sha) {
            continue;
        }
        if name == head {
            return Ok(sha.to_ascii_lowercase());
        }
        if name == tag {
            tag_sha = Some(sha);
        } else if name == peeled {
            peeled_sha = Some(sha);
        } else if name.ends_with(&format!("/{reference}")) {
            suffix_matches.push(sha);
        }
    }
    if let Some(sha) = peeled_sha.or(tag_sha) {
        return Ok(sha.to_ascii_lowercase());
    }
    match suffix_matches.len() {
        1 => Ok(suffix_matches[0].to_ascii_lowercase()),
        _ => Err(CliError::Topology(format!(
            "no unambiguous branch or tag named '{reference}' on '{label}'; \
             check the spelling or pass a full 40-hex commit SHA"
        ))),
    }
}

/// Create `dest` as a git repo, shallow-fetch exactly `commit`, check it
/// out detached, and verify `HEAD` is the pinned 40-hex SHA. Any failure
/// leaves no marker, so a retried cache fill restages from scratch.
///
/// Only the pinned commit is fetched (`--depth 1`), never full history:
/// `git init` + `remote add` + `fetch origin <sha>` + detached checkout.
fn git_clone_checkout(url: &str, commit: &str, dest: &Path, label: &str) -> Result<(), CliError> {
    let dest_str = dest.display().to_string();
    let git_dir = |args: &[&str]| {
        let mut full: Vec<String> = Vec::with_capacity(args.len() + 2);
        full.push("-C".to_string());
        full.push(dest_str.clone());
        full.extend(args.iter().map(|arg| (*arg).to_string()));
        let refs: Vec<&str> = full.iter().map(String::as_str).collect();
        run_git(&refs, label)
    };
    run_git(&["init", "--quiet", &dest_str], label)?;
    git_dir(&["remote", "add", "origin", url])?;
    git_dir(&["fetch", "--quiet", "--depth", "1", "origin", commit])?;
    git_dir(&["checkout", "--quiet", "--detach", commit])?;
    let head = git_dir(&["rev-parse", "HEAD"])?;
    let head = head.trim();
    if head != commit {
        return Err(CliError::Topology(format!(
            "checkout of '{label}' at commit {commit} produced HEAD {head}; \
             refusing to cache a tree that does not match the pinned commit"
        )));
    }
    fsx::write_private(&dest.join(CHECKOUT_MARKER), commit.as_bytes())
}

/// Persistent checkout: `<cache_root>/<owner>/<repo>/<commit>`.
///
/// Owner and repo are already normalized lowercase by
/// [`TopologySource::parse`], so the nested path is an unambiguous cache
/// identity. The fetch lands in a staging sibling and is renamed into
/// place, so concurrent resolvers and crashed clones can never expose a
/// partial tree. An existing checkout with its marker is reused as-is —
/// but only when both the commit directory and the marker are real,
/// non-symlink objects; a symlinked cache entry is never followed, reused,
/// or replaced (fail closed, with an error naming the offending path).
///
/// The returned path is always accompanied by a shared [`CheckoutLease`]
/// that is taken before the path escapes to any reader: for a fresh
/// checkout the lease is created inside the still-invisible staging dir,
/// so a completed cache entry is born already claimed and pruning can
/// never observe an unprotected completed checkout. An existing entry
/// without its marker is an interrupted clone; it is discarded only under
/// the exclusive removal claim, after re-verifying it is still incomplete
/// while the claim is held, so a checkout a concurrent resolver completed
/// mid-check — or one a reader still leases — is never deleted. Callers
/// hold the lease until their last read of the tree.
fn checkout_cached(
    owner: &str,
    repo: &str,
    commit: &str,
    cache_root: &Path,
) -> Result<(PathBuf, CheckoutLease), CliError> {
    fsx::makedirs_private(cache_root).map_err(|err| io_err(cache_root, err))?;
    let label = format!("github:{owner}/{repo}");
    let owner_dir = cache_root.join(owner);
    let repo_dir = owner_dir.join(repo);
    // The owner/repo components sit directly beneath the caller-provided
    // cache root, so a planted symlink there could redirect every clone,
    // reuse, and prune. Refuse to write through them.
    for dir in [&owner_dir, &repo_dir] {
        if fsx::is_symlink(dir) {
            return Err(CliError::Topology(format!(
                "cache path {} is a symlink; refusing to write through it, \
                 remove the link and retry",
                dir.display()
            )));
        }
    }
    fsx::makedirs_private(&owner_dir).map_err(|err| io_err(&owner_dir, err))?;
    fsx::makedirs_private(&repo_dir).map_err(|err| io_err(&repo_dir, err))?;
    let final_dir = repo_dir.join(commit);
    if completed_checkout(&final_dir) {
        // Take the shared active-use lease before the checkout is handed
        // out, then re-verify completion: pruning may have removed the
        // tree between the observation and the lock (pruning holds the
        // exclusive claim across the removal, so a lost race is detected
        // here and the clone is restaged below).
        match CheckoutLease::acquire(&final_dir) {
            Ok(lease) if completed_checkout(&final_dir) => return Ok((final_dir, lease)),
            Ok(lease) => drop(lease), // pruned mid-acquire; restage below
            Err(_) if !completed_checkout(&final_dir) => {} // pruned mid-acquire
            Err(exc) => return Err(exc),
        }
    }
    // A symlinked commit dir is never removed: remove_dir_all would fail
    // on the link itself, and recreating through it would write elsewhere.
    if fsx::is_symlink(&final_dir) {
        return Err(CliError::Topology(format!(
            "cache entry {} is a symlink; refusing to reuse or replace it, \
             remove the link and retry",
            final_dir.display()
        )));
    }
    // A directory without its marker is an interrupted clone: discard it.
    // The removal is taken under the exclusive lease claim and re-verified
    // while the claim is held, so a checkout a concurrent resolver
    // completed between the initial check and the deletion — or one a
    // reader still leases — is never removed.
    if fsx::lexists(&final_dir) && !discard_stale_checkout(&final_dir)? {
        // The entry exists and was kept: it either completed under the
        // exclusive claim or was verifiably complete while its claim
        // could not be taken. Reuse it through the normal shared-lease
        // acquisition/revalidation path.
        match CheckoutLease::acquire(&final_dir) {
            Ok(lease) if completed_checkout(&final_dir) => return Ok((final_dir, lease)),
            Ok(lease) => drop(lease), // pruned mid-acquire; restage below
            Err(_) if !completed_checkout(&final_dir) => {} // pruned mid-acquire
            Err(exc) => return Err(exc),
        }
    }
    let staging = repo_dir.join(staging_name("staging"));
    fsx::makedirs_private(&staging).map_err(|err| io_err(&staging, err))?;
    // Claim the checkout while it is still invisible: the renamed tree is
    // born with an active holder, so pruning can never delete a completed
    // checkout that no one has leased yet.
    let lease = CheckoutLease::acquire(&staging)?;
    if let Err(err) = git_clone_checkout(&github_url(owner, repo), commit, &staging, &label) {
        drop(lease);
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&staging, &final_dir) {
        // Lost a race with a concurrent resolver that finished first.
        drop(lease);
        let _ = std::fs::remove_dir_all(&staging);
        if completed_checkout(&final_dir) {
            let lease = CheckoutLease::acquire(&final_dir)?;
            if completed_checkout(&final_dir) {
                return Ok((final_dir, lease));
            }
        }
        return Err(io_err(&final_dir, err));
    }
    Ok((final_dir, lease))
}

/// A cache checkout is trustworthy only when the commit directory and its
/// completion marker are both real, non-symlink filesystem objects. This
/// predicate decides both the normal reuse path and the rename-race reuse
/// path, so a symlinked commit dir or marker can never make cache reuse
/// follow a link planted after the checkout completed.
fn completed_checkout(dir: &Path) -> bool {
    let marker = dir.join(CHECKOUT_MARKER);
    dir.is_dir() && !fsx::is_symlink(dir) && marker.is_file() && !fsx::is_symlink(&marker)
}

/// Shared active-use lease on a completed cache checkout, held from
/// checkout resolution until the last source read finishes.
///
/// Acquiring opens the lease file — creating it when missing — after
/// proving the path is not a symlink, then takes a shared OS advisory
/// lock. The lock blocks while a pruning holds the exclusive removal
/// claim; because pruning removes the tree while holding that claim, the
/// caller must re-verify [`completed_checkout`] after acquiring before
/// using (or returning) the directory. Lock ownership is kernel-managed:
/// it is released when the descriptor closes, which covers process crash,
/// so no stale permanent lease can survive the holder.
#[derive(Debug)]
struct CheckoutLease {
    _file: std::fs::File,
}

impl CheckoutLease {
    /// Take the shared lease on the checkout inside `checkout_dir`.
    fn acquire(checkout_dir: &Path) -> Result<CheckoutLease, CliError> {
        let path = checkout_dir.join(LEASE_FILE);
        // Fail closed on a planted symlink: a lease is never created
        // through, or locked on behalf of, a link.
        if fsx::is_symlink(&path) {
            return Err(CliError::Topology(format!(
                "cache lease {} is a symlink; refusing to use it, remove the \
                 link and retry",
                path.display()
            )));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| io_err(&path, err))?;
        if fsx::is_symlink(&path) {
            return Err(CliError::Topology(format!(
                "cache lease {} is a symlink; refusing to use it, remove the \
                 link and retry",
                path.display()
            )));
        }
        file.lock_shared().map_err(|err| io_err(&path, err))?;
        Ok(CheckoutLease { _file: file })
    }
}

impl Drop for CheckoutLease {
    fn drop(&mut self) {
        // Dropping the descriptor releases the shared lock — on orderly
        // drop and, because the kernel reclaims locks of dead processes,
        // on crash too. The lease file itself is deliberately left in
        // place: it is inert once unlocked, and it keeps "no lease file"
        // distinguishable from "crashed holder" for future prunes.
    }
}

/// Attempt the exclusive removal claim on a cache checkout, for pruning.
///
/// Returns `Some(guard)` only when no process actively uses the checkout;
/// the guard holds the exclusive lease until dropped, so no resolver can
/// re-lease the tree while it is being removed. Returns `None` — the
/// checkout is kept — when a live shared holder holds it, when the lease
/// path is or became a symlink, or whenever the lease cannot be verified
/// at all: pruning errs on the side of never deleting through a link or
/// out from under a reader.
fn claim_checkout_removal(dir: &Path) -> Option<std::fs::File> {
    let path = dir.join(LEASE_FILE);
    if fsx::is_symlink(&path) {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .ok()?;
    if fsx::is_symlink(&path) {
        return None; // swapped between check and open; keep the checkout
    }
    match file.try_lock() {
        Ok(()) => Some(file),
        // WouldBlock: a live shared holder keeps the checkout; Error: the
        // lease cannot be verified. Either way pruning errs on the side of
        // keeping the checkout.
        Err(_) => None,
    }
}

/// Discard a stale or incomplete cache entry without ever deleting a
/// checkout a concurrent resolver completed or a reader still uses.
///
/// The deletion is performed only under the exclusive removal claim from
/// [`claim_checkout_removal`], after re-verifying — while the claim is
/// held, so no shared lease can be acquired in between — that the entry
/// is still incomplete. Returns `Ok(true)` when the entry was removed;
/// `Ok(false)` when it must be kept, either because it completed under
/// the claim or because it was verifiably complete while its claim could
/// not be taken. In the `Ok(false)` case the caller must go through the
/// normal shared-lease revalidation path and fail closed if the entry is
/// still not a completed checkout. Errors — including the fail-closed
/// case of an incomplete entry whose exclusive claim cannot be taken —
/// never fall back to removing on existence alone.
fn discard_stale_checkout(dir: &Path) -> Result<bool, CliError> {
    match claim_checkout_removal(dir) {
        Some(claim) => {
            // Under the exclusive claim the re-check sees exactly what the
            // removal would delete: no resolver can publish or lease the
            // entry until the claim is dropped.
            let completed = completed_checkout(dir);
            if !completed {
                std::fs::remove_dir_all(dir).map_err(|err| io_err(dir, err))?;
            }
            drop(claim);
            Ok(!completed)
        }
        None => {
            // The claim could not be taken: a live shared holder, or a
            // lease path that is or became a symlink. Never delete on
            // existence alone; reuse is safe only for a verifiably
            // complete entry, otherwise fail closed.
            if !completed_checkout(dir) {
                return Err(CliError::Topology(format!(
                    "cache entry {} is incomplete but its lease cannot be \
                     exclusively claimed; refusing to delete it, remove the \
                     entry manually and retry",
                    dir.display()
                )));
            }
            Ok(false)
        }
    }
}

/// Dry-run checkout: a unique directory in the system temp dir, removed by
/// [`SourceCheckout`]'s `Drop`. Nothing is written under the cache root.
fn checkout_ephemeral(owner: &str, repo: &str, commit: &str) -> Result<PathBuf, CliError> {
    let label = format!("github:{owner}/{repo}");
    let dest = std::env::temp_dir().join(staging_name("omp-profile-dryrun"));
    fsx::makedirs_private(&dest).map_err(|err| io_err(&dest, err))?;
    match git_clone_checkout(&github_url(owner, repo), commit, &dest, &label) {
        Ok(()) => Ok(dest),
        Err(err) => {
            let _ = std::fs::remove_dir_all(&dest);
            Err(err)
        }
    }
}

/// A resolved source: a root-level profile package or legacy catalog, the
/// pinned commit (GitHub only), the source root to hand to
/// [`topology::find_topology`]/[`topology::list_topologies`], the ephemeral
/// staging directory to clean up for dry runs, and — for persistent GitHub
/// cache checkouts — the active-use lease that keeps pruning from deleting
/// the tree while it is being read.
#[derive(Debug)]
pub struct SourceCheckout {
    pub source: TopologySource,
    /// Full 40-hex commit SHA for GitHub sources; `None` for local sources.
    pub commit: Option<String>,
    /// Root containing a profile package or legacy `topologies/` catalog.
    pub root: PathBuf,
    /// Dry-run staging directory; removed on drop.
    staging: Option<PathBuf>,
    /// Shared active-use lease on a persistent cache checkout; released
    /// on drop (and on process crash, by the kernel). `None` for local
    /// sources and ephemeral dry-run clones, which pruning never touches.
    /// Never read after construction — kept alive purely so `Drop` (and
    /// the kernel, on crash) releases the lock exactly when the checkout
    /// is no longer being read.
    _lease: Option<CheckoutLease>,
}

impl Drop for SourceCheckout {
    fn drop(&mut self) {
        if let Some(dir) = self.staging.take() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// Resolve a source spec to a usable topology-tree root.
///
/// * `spec`: `github:OWNER/REPO` or a local path (see [`TopologySource::parse`]).
/// * `reference`: branch, tag, or full 40-hex SHA for GitHub sources; must
///   be `None` for local sources and when omitted resolves the remote's
///   default `HEAD`.
/// * `cache_root`: where persistent GitHub checkouts are staged atomically
///   (`<cache_root>/<owner>/<repo>/<commit>/`, with owner/repo normalized
///   lowercase). Pass `None` for dry runs: the checkout is a temporary
///   shallow fetch that `SourceCheckout` removes on drop, leaving no
///   persistent cache behind.
///
/// The returned root can be passed straight to
/// [`topology::find_topology`], [`topology::list_topologies`], and
/// [`topology::topology_metadata`]. Keep the returned [`SourceCheckout`]
/// alive until the last read of the tree finishes: for persistent GitHub
/// cache checkouts it holds the shared active-use lease that keeps
/// concurrent cache pruning from deleting the checkout mid-read.
pub fn resolve(
    spec: &str,
    reference: Option<&str>,
    cache_root: Option<&Path>,
) -> Result<SourceCheckout, CliError> {
    let source = TopologySource::parse(spec)?;
    match &source {
        TopologySource::GitHub { owner, repo } => {
            let commit = resolve_remote_commit(&source, reference)?;
            match cache_root {
                Some(cache_root) => {
                    let (root, lease) = checkout_cached(owner, repo, &commit, cache_root)?;
                    Ok(SourceCheckout {
                        source,
                        commit: Some(commit),
                        root,
                        staging: None,
                        _lease: Some(lease),
                    })
                }
                None => {
                    let root = checkout_ephemeral(owner, repo, &commit)?;
                    Ok(SourceCheckout {
                        source,
                        commit: Some(commit),
                        root: root.clone(),
                        staging: Some(root),
                        _lease: None,
                    })
                }
            }
        }
        TopologySource::Local { path } => {
            if let Some(reference) = reference {
                return Err(CliError::Usage(format!(
                    "--ref '{reference}' is only valid for 'github:' sources; \
                     local source '{}' is used exactly as it is on disk",
                    path.display()
                )));
            }
            Ok(SourceCheckout {
                root: path.clone(),
                source,
                commit: None,
                staging: None,
                _lease: None,
            })
        }
    }
}

/// Prune stale cache checkouts for one GitHub source.
///
/// Only `<cache_root>/<owner>/<repo>` is inspected — nothing outside that
/// source's own cache subtree is ever touched or traversed. Local sources
/// have no cache and are a no-op.
///
/// Retention:
/// * every dir whose name is in `referenced_commits` (compared
///   case-insensitively against the normalized lowercase commit names) is
///   kept;
/// * among the remaining valid cache dirs, the `keep_unreferenced` newest
///   by checkout-marker mtime survive, ordered deterministically by commit
///   name on mtime ties;
/// * everything else valid is removed with `remove_dir_all`.
///
/// A "valid cache dir" is a real directory (not a symlink) named exactly
/// like a full 40-hex commit SHA and carrying the completion marker. The
/// owner and repo components beneath the cache root are validated the same
/// way: if either is a symlink, pruning is skipped entirely for that
/// source, so a planted link can never redirect a traversal or removal.
/// Symlinked roots/entries and incomplete or in-progress staging dirs are
/// skipped entirely — never followed, counted, or removed — so a concurrent
/// resolve cannot be disturbed and a symlink can never redirect a removal.
/// Each deletion is also re-checked at remove time against symlink swaps.
///
/// Active-use protection: a checkout still being read by another process
/// — a live shared lease on its lease file, or any lease that cannot be
/// verified — is never removed. Pruning takes the lease file exclusively
/// with a non-blocking lock and holds that claim across the whole
/// removal, so a resolver that lost the race re-verifies its checkout
/// after locking and restages instead of reading a deleted tree. A lease
/// left behind by a crashed holder carries no lock (the kernel released
/// it), and its checkout stays prunable.
///
/// Callers must invoke this only after a successful registry mutation; the
/// return value is the number of cache dirs removed.
pub fn prune_cache(
    cache_root: &Path,
    source: &TopologySource,
    referenced_commits: &BTreeSet<String>,
    keep_unreferenced: usize,
) -> Result<usize, CliError> {
    use std::time::SystemTime;

    let TopologySource::GitHub { owner, repo } = source else {
        return Ok(0);
    };
    let owner_dir = cache_root.join(owner);
    if fsx::is_symlink(&owner_dir) {
        return Ok(0);
    }
    let repo_dir = owner_dir.join(repo);
    if !repo_dir.is_dir() || fsx::is_symlink(&repo_dir) {
        return Ok(0);
    }
    let referenced: BTreeSet<String> = referenced_commits
        .iter()
        .map(|commit| commit.to_ascii_lowercase())
        .collect();
    let mut unreferenced: Vec<(SystemTime, String, PathBuf)> = Vec::new();
    let entries = match std::fs::read_dir(&repo_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(io_err(&repo_dir, err)),
    };
    for entry in entries {
        let entry = entry.map_err(|err| io_err(&repo_dir, err))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // Skip symlinks, in-progress staging dirs, and anything that is not
        // a completed per-commit cache dir; none of these are prunable.
        if name.starts_with('.') || !is_full_sha(&name) {
            continue;
        }
        let file_type = entry.file_type().map_err(|err| io_err(&path, err))?;
        if !file_type.is_dir() {
            continue;
        }
        let marker = path.join(CHECKOUT_MARKER);
        if fsx::is_symlink(&path) || !marker.is_file() || fsx::is_symlink(&marker) {
            continue;
        }
        if !referenced.contains(&name.to_ascii_lowercase()) {
            // lstat, never stat: the marker was validated as a real file
            // above, but must not be re-followed if it is swapped for a
            // symlink between validation and this read.
            let mtime = std::fs::symlink_metadata(&marker)
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            unreferenced.push((mtime, name, path));
        }
    }
    // Newest first; commit-name ascending breaks mtime ties deterministically.
    unreferenced.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut removed = 0usize;
    for (_, _, path) in unreferenced.into_iter().skip(keep_unreferenced) {
        // Re-check at remove time: if the entry was swapped for a symlink
        // (or vanished) after scanning, skip it — never delete through a
        // link, never fail the whole prune over a race.
        if fsx::is_symlink(&path) || !path.is_dir() {
            continue;
        }
        // Active-use protection: only delete while holding the exclusive
        // removal claim on the checkout's lease. A live shared holder (or
        // an unverifiable lease) keeps the checkout; the claim is held
        // across the removal so no resolver can lease the tree back and
        // read it while it is being deleted.
        let Some(_claim) = claim_checkout_removal(&path) else {
            continue;
        };
        std::fs::remove_dir_all(&path).map_err(|err| io_err(&path, err))?;
        removed += 1;
    }
    Ok(removed)
}

/// Deterministic digest of a topology's declared metadata and `agent/`
/// overlay for drift status.
///
/// The topology's own `topology.yml` feeds the hash first, framed as the
/// relative path `/topology.yml` — a spelling no overlay file can
/// collide with, since every overlay relative path must pass
/// [`fsx::safe_rel`] — so metadata-only version bumps still change the
/// digest. Every overlay file — sorted by relative path, `config.yml`
/// included, same safety rules as [`topology::source_agent_files`] —
/// then feeds `path NUL length contents 0xFF` into the same SHA-256.
/// Contents stream through a fixed-size stack buffer, so digest memory
/// is O(1) no matter how large the metadata, the overlay, or the largest
/// file grows; `length` is the file's lstat size recorded in the framing,
/// and the streamed byte count must match it exactly — a file that
/// shrinks or grows mid-hash fails the digest instead of silently
/// hashing a torn view. Metadata and overlay symlinks are rejected
/// outright, never followed. Including `config.yml` means config-only
/// edits still change the digest; the framing is unchanged, so digests
/// computed for identical trees remain identical across machines and
/// runs. An overlay with no files contributes nothing beyond the
/// metadata frame.
pub fn overlay_digest(topology_dir: &Path) -> Result<String, CliError> {
    use sha2::Digest as _;

    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    // The declared metadata is itself part of the source: a version bump
    // with no overlay change must still register as drift.
    let metadata_path = topology_dir.join("topology.yml");
    hash_framed_file(
        &mut hasher,
        "/topology.yml",
        &metadata_path,
        &mut buffer,
        "topology metadata file",
    )?;
    let files = topology::source_overlay_paths(topology_dir)?;
    for (rel, path) in &files {
        hash_framed_file(&mut hasher, rel, path, &mut buffer, "topology overlay file")?;
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push(char::from_digit((byte >> 4) as u32, 16).unwrap());
        hex.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap());
    }
    Ok(hex)
}

/// Hash one source file into `hasher` under the deterministic
/// `path NUL length contents 0xFF` framing shared by [`overlay_digest`].
///
/// `rel` is the framing path (metadata uses the collision-proof
/// `/topology.yml` spelling; overlay callers pass their `safe_rel`
/// relative path), `label` names the file class in diagnostics, and
/// `buffer` is the caller-owned streaming scratch, keeping digest memory
/// O(1). The file must be a real, non-symlink regular file, and its
/// streamed byte count must match the lstat size exactly — a file that
/// shrinks or grows mid-hash fails instead of silently hashing a torn
/// view.
fn hash_framed_file(
    hasher: &mut sha2::Sha256,
    rel: &str,
    path: &Path,
    buffer: &mut [u8],
    label: &str,
) -> Result<(), CliError> {
    use sha2::Digest as _;
    use std::io::Read as _;

    let stat = std::fs::symlink_metadata(path).map_err(|err| io_err(path, err))?;
    if stat.file_type().is_symlink() {
        return Err(CliError::Topology(format!(
            "{label} {} is a symlink; refusing to hash it",
            path.display()
        )));
    }
    if !stat.file_type().is_file() {
        return Err(CliError::Topology(format!(
            "{label} {} is not a regular file",
            path.display()
        )));
    }
    let length = stat.len();
    hasher.update(rel.as_bytes());
    hasher.update([0]);
    hasher.update(length.to_le_bytes());
    let mut file = std::fs::File::open(path).map_err(|err| io_err(path, err))?;
    let mut remaining = length;
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = file
            .read(&mut buffer[..want])
            .map_err(|err| io_err(path, err))?;
        if read == 0 {
            return Err(CliError::Topology(format!(
                "{label} {} changed size while hashing: \
                 expected {length} bytes, file ended early",
                path.display()
            )));
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    // A final read detects growth past the recorded length.
    if file.read(buffer).map_err(|err| io_err(path, err))? != 0 {
        return Err(CliError::Topology(format!(
            "{label} {} changed size while hashing: \
             expected {length} bytes, file kept going",
            path.display()
        )));
    }
    hasher.update([0xff]);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(staging_name("cache-lease-test"));
            std::fs::create_dir(&path).expect("failed to create isolated cache test directory");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn prune_cache_preserves_checkout_until_source_checkout_lease_drops() {
        let temp = TestDir::new();
        let owner = "owner";
        let repo = "repo";
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let cache_root = temp.0.join("cache");
        let checkout_dir = cache_root.join(owner).join(repo).join(commit);
        std::fs::create_dir_all(&checkout_dir).expect("failed to create cache checkout");
        std::fs::write(checkout_dir.join(CHECKOUT_MARKER), b"complete\n")
            .expect("failed to mark cache checkout complete");

        let source = TopologySource::GitHub {
            owner: owner.to_string(),
            repo: repo.to_string(),
        };
        let checkout = SourceCheckout {
            source: source.clone(),
            commit: Some(commit.to_string()),
            root: checkout_dir.clone(),
            staging: None,
            _lease: Some(
                CheckoutLease::acquire(&checkout_dir).expect("failed to acquire checkout lease"),
            ),
        };
        let referenced = BTreeSet::new();

        assert_eq!(
            prune_cache(&cache_root, &source, &referenced, 0).unwrap(),
            0
        );
        assert!(checkout_dir.is_dir(), "leased checkout was pruned");

        drop(checkout);

        assert_eq!(
            prune_cache(&cache_root, &source, &referenced, 0).unwrap(),
            1
        );
        assert!(!checkout_dir.exists(), "unleased checkout was not pruned");
    }

    #[test]
    fn discard_stale_checkout_removes_unleased_incomplete_directory() {
        let temp = TestDir::new();
        let checkout_dir = temp.0.join("incomplete");
        std::fs::create_dir(&checkout_dir).expect("failed to create incomplete checkout");

        assert!(
            discard_stale_checkout(&checkout_dir).expect("failed to discard stale checkout"),
            "incomplete checkout was not discarded"
        );
        assert!(
            !checkout_dir.exists(),
            "stale checkout directory still exists"
        );
    }

    #[test]
    fn discard_stale_checkout_preserves_completed_checkout() {
        let temp = TestDir::new();
        let checkout_dir = temp.0.join("complete");
        std::fs::create_dir(&checkout_dir).expect("failed to create completed checkout");
        std::fs::write(checkout_dir.join(CHECKOUT_MARKER), b"complete\n")
            .expect("failed to mark checkout complete");

        assert!(
            !discard_stale_checkout(&checkout_dir).expect("failed to inspect completed checkout"),
            "completed checkout was reported as discarded"
        );
        assert!(
            completed_checkout(&checkout_dir),
            "completed checkout was not preserved"
        );
    }

    #[test]
    fn discard_stale_checkout_keeps_incomplete_directory_with_shared_lease() {
        let temp = TestDir::new();
        let checkout_dir = temp.0.join("leased-incomplete");
        std::fs::create_dir(&checkout_dir).expect("failed to create incomplete checkout");
        let _lease =
            CheckoutLease::acquire(&checkout_dir).expect("failed to acquire shared checkout lease");

        let result = discard_stale_checkout(&checkout_dir);
        assert!(
            matches!(
                &result,
                Err(CliError::Topology(message))
                    if message.contains(
                        "incomplete but its lease cannot be exclusively claimed; refusing to delete it"
                    )
            ),
            "expected fail-closed error for leased incomplete checkout, got {result:?}"
        );
        assert!(
            checkout_dir.is_dir(),
            "incomplete checkout with a shared lease was deleted"
        );
    }
}
