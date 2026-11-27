//! Self-update: replace the running `omp-profile` binary with the signed
//! GitHub Release of the same tool.
//!
//! Trust model (mirrors `RELEASING.md`): every release of
//! `digi4care/omp-profile-installer` publishes per-target tarballs named
//! `omp-profile-${VERSION}-${TARGET}.tar.gz`, a `SHA256SUMS` manifest
//! listing those exact archive names, and a detached RSA/SHA-256 signature
//! `SHA256SUMS.sig` over that manifest. The public half of the release key
//! is committed at `keys/omp-topology-release-public.pem` and embedded in
//! the binary at compile time; the private key exists only in the GitHub
//! Actions secret. There is no unsigned fallback.
//!
//! An update is applied only after, in order:
//!
//! 1. the release version is resolved — pinned by the caller, or latest
//!    via the GitHub API — over HTTPS only,
//! 2. `SHA256SUMS.sig` verifies against the embedded public key with
//!    `openssl dgst -sha256 -verify`,
//! 3. the selected tarball's SHA-256, computed in-process with the `sha2`
//!    crate, matches the manifest entry for its exact asset name,
//! 4. the archive extracts to a single `omp-profile` binary that runs and
//!    reports the resolved version.
//!
//! Only then is the current executable replaced, atomically, by renaming a
//! staged copy inside the executable's own directory — never through a
//! shell, never with sudo. Any failure before the rename leaves the running
//! binary untouched. All downloads and scratch files live in a private
//! (0700) temporary directory that is removed on every return path.
//!
//! External commands, all invoked via `Command` argv (never a shell):
//!
//! * `curl` — HTTPS-only downloads (`--proto "=https"` and
//!   `--proto-redir "=https"`); also used for the GitHub API
//!   `releases/latest` query, which requires the explicit `User-Agent`
//!   header set here.
//! * `openssl` — detached signature verification
//!   (`openssl dgst -sha256 -verify <public.pem> -signature <sig> <sums>`).
//! * `tar` — archive extraction (`tar -xzf <archive> -C <dir>`).
//!
//! Missing tools, network errors, bad signatures or checksums, and
//! permission problems all fail closed with actionable messages.
//!
//! Supported targets: `x86_64-unknown-linux-musl` (Linux x86_64, static
//! build — immune to old-glibc hosts and the retired ubuntu-22.04
//! runner), `x86_64-apple-darwin` (macOS Intel), and
//! `aarch64-apple-darwin` (macOS Apple Silicon). Anything else is
//! rejected before any network access.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::error::{CliError, io_err};

/// Repository that publishes signed releases.
const REPO: &str = "digi4care/omp-profile-installer";

/// Public half of the release signing key, committed in the repository and
/// embedded at compile time so an installed binary needs no checkout.
const RELEASE_KEY_PEM: &str = include_str!("../keys/omp-topology-release-public.pem");

/// GitHub API endpoint for the latest release (HTTPS only).
const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/digi4care/omp-profile-installer/releases/latest";

/// `User-Agent` required by the GitHub API, sent on the latest-release
/// query.
const API_USER_AGENT: &str = "User-Agent: omp-profile-self-update";

/// Seconds curl waits to establish a TCP/TLS connection before giving up.
/// Every curl invocation (API query and downloads alike) gets this.
const CONNECT_TIMEOUT_SECS: &str = "15";

/// Overall seconds budget for any single curl transfer. Small requests
/// (API JSON, SHA256SUMS, signature) finish in seconds; the archive
/// budget is larger so slow links still succeed.
const SMALL_MAX_TIME_SECS: &str = "60";
const ARCHIVE_MAX_TIME_SECS: &str = "900";

/// Abort a transfer that delivers under 1 KiB/s for 30 consecutive
/// seconds — a hung-but-not-disconnected connection fails closed instead
/// of stalling forever.
const STALL_SPEED_BYTES: &str = "1024";
const STALL_TIME_SECS: &str = "30";

/// Size caps, enforced via curl `--max-filesize` and re-checked against
/// the downloaded file on disk (a server may lie about Content-Length).
/// The manifest, its signature, and the API JSON are tiny text payloads;
/// 1 MiB is generous. Release tarballs are single static binaries (musl
/// builds stay around 10 MiB), so 256 MiB leaves huge headroom while
/// bounding disk exhaustion from a hostile or broken endpoint.
const SMALL_DOWNLOAD_MAX_BYTES: &str = "1048576";
const ARCHIVE_DOWNLOAD_MAX_BYTES: &str = "268435456";

/// Resolve and (unless `dry_run`) apply the signed self-update.
///
/// `version_override` pins the release (`0.2.0` or `v0.2.0`); `None`
/// resolves the latest published release. Every human-readable line lands
/// in `lines`; nothing is printed here. Returns the process exit code: `0`
/// for updated, already-current, and dry-run plans. A `dry_run` resolves
/// the version and reports the plan without downloading anything or
/// touching the executable.
pub fn run(
    version_override: Option<&str>,
    dry_run: bool,
    lines: &mut Vec<String>,
) -> Result<i32, CliError> {
    let target = detect_target()?;
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let version = match version_override {
        Some(raw) => normalize_version(raw)?,
        None => resolve_latest_version(lines)?,
    };

    if version == current_version {
        lines.push(format!(
            "omp-profile is already at v{version} (the {} release)",
            if version_override.is_some() {
                "requested"
            } else {
                "latest"
            }
        ));
        return Ok(0);
    }

    lines.push(format!(
        "resolved release v{version} for target {target} (currently v{current_version})"
    ));

    if dry_run {
        lines.push(format!(
            "dry-run: would download and verify {asset}, then replace {exe}",
            asset = asset_name(&version, target),
            exe = current_exe_display()?,
        ));
        lines.push("dry-run: no files were changed".to_string());
        return Ok(0);
    }

    let work = TempWorkDir::new()?;

    // 1. Download manifest, detached signature, and the selected archive.
    let base = download_base(&version);
    let sums_path = work.path.join("SHA256SUMS");
    let sig_path = work.path.join("SHA256SUMS.sig");
    let asset = asset_name(&version, target);
    let archive_path = work.path.join(&asset);
    download(
        &format!("{base}/SHA256SUMS"),
        &sums_path,
        SMALL_DOWNLOAD_MAX_BYTES,
    )?;
    download(
        &format!("{base}/SHA256SUMS.sig"),
        &sig_path,
        SMALL_DOWNLOAD_MAX_BYTES,
    )?;
    download(
        &format!("{base}/{asset}"),
        &archive_path,
        ARCHIVE_DOWNLOAD_MAX_BYTES,
    )?;
    lines.push(format!(
        "downloaded {asset} and its signed checksum manifest"
    ));

    // 2. Verify the detached RSA/SHA-256 signature against the embedded
    //    committed public key. No unsigned fallback exists.
    let key_path = work.path.join("omp-topology-release-public.pem");
    fs::write(&key_path, RELEASE_KEY_PEM).map_err(|err| io_err(&key_path, err))?;
    set_private_file(&key_path)?;
    run_tool_checked(
        "openssl",
        &[
            "dgst",
            "-sha256",
            "-verify",
            path_str(&key_path)?,
            "-signature",
            path_str(&sig_path)?,
            path_str(&sums_path)?,
        ],
        "verify the SHA256SUMS signature",
    )?;
    lines.push(
        "verified SHA256SUMS signature against keys/omp-topology-release-public.pem".to_string(),
    );

    // 3. Verify the archive against the signed manifest entry.
    let expected = manifest_digest(&sums_path, &asset)?;
    let actual = sha256_hex_file(&archive_path)?;
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(CliError::Usage(format!(
            "checksum mismatch for {asset}: SHA256SUMS lists {expected}, \
             the downloaded archive hashes to {actual}; refusing to update"
        )));
    }
    lines.push(format!("verified {asset} checksum (sha256 {actual})"));

    // 4. Extract and confirm the candidate is a working binary that
    //    reports the resolved version.
    let extract_dir = work.path.join("extract");
    fs::create_dir(&extract_dir).map_err(|err| io_err(&extract_dir, err))?;
    run_tool_checked(
        "tar",
        &[
            "-xzf",
            path_str(&archive_path)?,
            "-C",
            path_str(&extract_dir)?,
        ],
        "extract the release archive",
    )?;
    let candidate = extract_dir.join("omp-profile");
    if !candidate.is_file() {
        return Err(CliError::Usage(format!(
            "release archive {asset} does not contain the expected \
             'omp-profile' binary; refusing to update"
        )));
    }
    make_executable(&candidate)?;
    let reported = candidate_version(&candidate)?;
    if reported != version {
        return Err(CliError::Usage(format!(
            "candidate binary reports version '{reported}', not the resolved \
             release '{version}'; refusing to update"
        )));
    }
    lines.push(format!(
        "verified candidate binary reports version {version}"
    ));

    // 5. All checks passed: stage next to the executable and rename over
    //    it, so the replacement is atomic and the running binary is never
    //    overwritten before verification finished.
    replace_current(&candidate, &current_version, &version, lines)?;

    Ok(0)
}

/// Map this host to a release target triple; reject anything unsupported
/// before any network access happens. Linux uses the static musl build so
/// the binary runs on any Linux x86_64 host regardless of glibc age.
fn detect_target() -> Result<&'static str, CliError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        (os, arch) => Err(CliError::Usage(format!(
            "self-update is not supported on {os}/{arch}; supported targets: \
             x86_64-unknown-linux-musl, x86_64-apple-darwin, aarch64-apple-darwin"
        ))),
    }
}

/// Accept `MAJOR.MINOR.PATCH` with an optional leading `v` and normalize
/// to the bare version used in asset names and tags. Fields must be
/// SemVer-valid: numeric with no leading zeroes except the single digit
/// `0` (matching the release workflow's `vMAJOR.MINOR.PATCH` tag
/// contract). Everything else — path separators, URLs, empty fields — is
/// rejected so the value can safely appear in download URLs and asset
/// names.
fn normalize_version(raw: &str) -> Result<String, CliError> {
    let bare = raw.strip_prefix('v').unwrap_or(raw);
    let fields: Vec<&str> = bare.split('.').collect();
    let valid_field =
        |field: &str| field.bytes().all(|b| b.is_ascii_digit()) && valid_semver_number(field);
    let numeric = fields.len() == 3 && fields.iter().all(|field| valid_field(field));
    if numeric {
        Ok(bare.to_string())
    } else {
        Err(CliError::Usage(format!(
            "invalid version '{raw}'; expected vMAJOR.MINOR.PATCH (for example v0.2.0)"
        )))
    }
}

/// A SemVer version core number: digits only, no leading zero unless the
/// number is exactly `0`.
fn valid_semver_number(field: &str) -> bool {
    !field.is_empty() && (field == "0" || !field.starts_with('0'))
}

/// Ask the GitHub API for the latest release tag and normalize it. The
/// query gets the same bounded connect/overall timeouts, stall detection,
/// and 1 MiB size cap as every other curl request.
fn resolve_latest_version(lines: &mut Vec<String>) -> Result<String, CliError> {
    let output = run_tool(
        "curl",
        &[
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            CONNECT_TIMEOUT_SECS,
            "--max-time",
            SMALL_MAX_TIME_SECS,
            "--speed-time",
            STALL_TIME_SECS,
            "--speed-limit",
            STALL_SPEED_BYTES,
            "--max-filesize",
            SMALL_DOWNLOAD_MAX_BYTES,
            "--header",
            API_USER_AGENT,
            LATEST_RELEASE_API,
        ],
        "look up the latest release",
    )?;
    if !output.status.success() {
        return Err(tool_failure("curl", "look up the latest release", &output));
    }
    // Same on-disk size re-check as `download`: Content-Length can lie.
    if output.stdout.len() as u64 > SMALL_DOWNLOAD_MAX_BYTES.parse::<u64>().unwrap_or(u64::MAX) {
        return Err(CliError::Usage(format!(
            "the GitHub latest-release response exceeded the \
             {SMALL_DOWNLOAD_MAX_BYTES}-byte safety limit; refusing to update"
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|err| {
        CliError::Usage(format!(
            "could not parse the GitHub latest-release response as JSON: {err}"
        ))
    })?;
    let tag = value
        .get("tag_name")
        .and_then(|tag| tag.as_str())
        .ok_or_else(|| {
            CliError::Usage(
                "the GitHub latest-release response has no string 'tag_name'; \
                 refusing to guess a version"
                    .to_string(),
            )
        })?;
    let version = normalize_version(tag)?;
    lines.push(format!("latest release is {tag}"));
    Ok(version)
}

/// Release download base for a validated version.
fn download_base(version: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/v{version}")
}

/// Exact release asset name for a validated version and known target.
fn asset_name(version: &str, target: &str) -> String {
    format!("omp-profile-{version}-{target}.tar.gz")
}

/// Download `url` to `dest` with curl, HTTPS only. The URL is constructed
/// from validated components; enforce the scheme anyway so a future edit
/// cannot silently weaken transport. `max_filesize` caps the transfer in
/// bytes (curl `--max-filesize`, re-checked on disk): every request gets
/// bounded connect/overall timeouts and stall detection, so no endpoint —
/// GitHub API, manifest, signature, or archive — can stall or exhaust
/// disk indefinitely.
fn download(url: &str, dest: &Path, max_filesize: &str) -> Result<(), CliError> {
    if !url.starts_with("https://") {
        return Err(CliError::Usage(format!(
            "refusing to download from a non-HTTPS URL: {url}"
        )));
    }
    let output = run_tool(
        "curl",
        &[
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            CONNECT_TIMEOUT_SECS,
            "--max-time",
            ARCHIVE_MAX_TIME_SECS,
            "--speed-time",
            STALL_TIME_SECS,
            "--speed-limit",
            STALL_SPEED_BYTES,
            "--max-filesize",
            max_filesize,
            "--output",
            path_str(dest)?,
            url,
        ],
        "download the release asset",
    )?;
    if !output.status.success() {
        return Err(tool_failure("curl", &format!("download {url}"), &output));
    }
    // `--max-filesize` trusts Content-Length; a server that understates
    // it could still deliver an oversized body, so re-check on disk.
    let size = fs::metadata(dest).map_err(|err| io_err(dest, err))?.len();
    if size > max_filesize.parse::<u64>().unwrap_or(u64::MAX) {
        return Err(CliError::Usage(format!(
            "downloaded file from {url} is {size} bytes, exceeding the \
             {max_filesize}-byte safety limit; refusing to update"
        )));
    }
    Ok(())
}

/// Find the manifest line for `asset` and return its hex SHA-256. Exactly
/// one entry must name the asset; duplicates are ambiguous (a smuggled
/// second line could shadow the real digest), so the manifest is rejected
/// as malformed.
fn manifest_digest(sums_path: &Path, asset: &str) -> Result<String, CliError> {
    let text = fs::read_to_string(sums_path).map_err(|err| io_err(sums_path, err))?;
    let mut found: Option<&str> = None;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(digest), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if name != asset {
            continue;
        }
        // A selected row must be exactly `<digest> <filename>` (GNU
        // sha256sum may prefix the digest with `\` when escaping is in
        // play; that is handled below). Trailing tokens make the row
        // malformed, so reject rather than guess which fields to trust.
        if parts.next().is_some() {
            return Err(CliError::Usage(format!(
                "SHA256SUMS entry for {asset} has unexpected extra fields; \
                 the manifest is malformed, refusing to update"
            )));
        }
        if found.is_some() {
            return Err(CliError::Usage(format!(
                "SHA256SUMS lists {asset} more than once; \
                 the manifest is ambiguous, refusing to update"
            )));
        }
        found = Some(digest);
    }
    let digest = found.ok_or_else(|| {
        CliError::Usage(format!(
            "{asset} is not listed in the signed SHA256SUMS manifest; refusing to update"
        ))
    })?;
    let hex = digest.strip_prefix('\\').unwrap_or(digest);
    let valid = hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit());
    if valid {
        Ok(hex.to_string())
    } else {
        Err(CliError::Usage(format!(
            "SHA256SUMS entry for {asset} is not a 64-digit hex SHA-256 digest"
        )))
    }
}

/// Compute the lowercase hex SHA-256 of a file without loading it whole.
fn sha256_hex_file(path: &Path) -> Result<String, CliError> {
    let mut file = fs::File::open(path).map_err(|err| io_err(path, err))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf).map_err(|err| io_err(path, err))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Run the extracted candidate with `--version` and return the version it
/// reports.
fn candidate_version(candidate: &Path) -> Result<String, CliError> {
    let output = run_tool(
        path_str(candidate)?,
        &["--version"],
        "check the downloaded binary",
    )?;
    if !output.status.success() {
        return Err(tool_failure(
            "omp-profile (downloaded candidate)",
            "run the downloaded binary with --version",
            &output,
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.lines().next().unwrap_or("").trim();
    first
        .split_whitespace()
        .find(|token| token.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::Usage(format!(
                "the downloaded binary did not report a version ('{first}'); \
                 refusing to update"
            ))
        })
}

/// Stage the verified candidate next to the current executable and rename
/// it over the current path: same filesystem, so the swap is atomic, and
/// replacing a running binary this way is safe on Linux and macOS. Never
/// uses sudo; permission problems fail closed with the running binary
/// untouched.
///
/// The staging file is created exclusively (`create_new`) under a unique
/// name, so a symlink planted at a predictable path is never followed and
/// a concurrent self-update cannot clobber our file. Only the staging
/// file this invocation created is ever removed.
fn replace_current(
    candidate: &Path,
    current_version: &str,
    version: &str,
    lines: &mut Vec<String>,
) -> Result<(), CliError> {
    let current = std::env::current_exe().map_err(|err| {
        CliError::Usage(format!(
            "could not determine the current executable path: {err}"
        ))
    })?;
    let dir = current.parent().ok_or_else(|| {
        CliError::Usage(format!(
            "could not determine the directory of {}",
            current.display()
        ))
    })?;
    // Mirror the current binary's permissions (fall back to rwxr-xr-x) so
    // the replacement is executable and keeps the prior mode.
    let mode = current_mode(&current)?;

    let staging = stage_candidate(dir, candidate, mode, &current)?;

    if let Err(err) = fs::rename(&staging, &current) {
        // Rename failed: the staging file is ours alone (create_new), so
        // removing it cannot touch anyone else's path.
        let _ = fs::remove_file(&staging);
        return Err(replace_io_err(&current, err));
    }
    // Persist the directory entry so the replaced binary survives a
    // crash; best-effort where the platform cannot fsync a directory.
    sync_dir(dir);

    lines.push(format!(
        "updated omp-profile v{current_version} → v{version} at {}",
        current.display()
    ));
    Ok(())
}

/// Create a unique staging file in `dir` with `create_new` (retrying name
/// collisions), copy the verified candidate into it, and apply `mode`.
/// Returns the owned staging path. Nothing pre-existing at the path is
/// ever opened, followed, or removed.
fn stage_candidate(
    dir: &Path,
    candidate: &Path,
    mode: u32,
    current: &Path,
) -> Result<PathBuf, CliError> {
    let mut source = fs::File::open(candidate).map_err(|err| io_err(candidate, err))?;
    for attempt in 0..100u32 {
        let staging = dir.join(format!(
            ".omp-profile-self-update-{}-{}-{attempt}",
            std::process::id(),
            now_nanos()
        ));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(replace_io_err(current, err)),
        };
        if let Err(err) = std::io::copy(&mut source, &mut file) {
            // Copy failed mid-way: remove only the file we just created.
            let _ = fs::remove_file(&staging);
            return Err(io_err(&staging, err));
        }
        // Push the staged bytes to stable storage before the rename so a
        // crash cannot leave a zero-length or torn binary at the target.
        if let Err(err) = file.sync_all() {
            let _ = fs::remove_file(&staging);
            return Err(io_err(&staging, err));
        }
        drop(file);
        if let Err(err) = set_permissions(&staging, mode) {
            let _ = fs::remove_file(&staging);
            return Err(err);
        }
        return Ok(staging);
    }
    Err(CliError::Usage(format!(
        "could not create a unique staging file in {} after 100 attempts",
        dir.display()
    )))
}

/// Map staging/replace I/O errors, turning permission problems into an
/// actionable no-sudo message.
fn replace_io_err(current: &Path, err: std::io::Error) -> CliError {
    if err.kind() == std::io::ErrorKind::PermissionDenied {
        CliError::Usage(format!(
            "cannot replace {}: permission denied (self-update never uses sudo); \
             fix the ownership of the executable directory or reinstall manually",
            current.display()
        ))
    } else {
        io_err(current, err)
    }
}

/// fsync a directory so a rename inside it is durable across a crash.
/// Supported on unix (Linux and macOS both allow opening a directory and
/// syncing the fd); best-effort elsewhere, where an unsynced rename still
/// leaves a valid file on all supported targets.
#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(file) = fs::File::open(dir) {
        let _ = file.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

/// Human-readable current executable path, for dry-run output.
fn current_exe_display() -> Result<String, CliError> {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .map_err(|err| {
            CliError::Usage(format!(
                "could not determine the current executable path: {err}"
            ))
        })
}

/// Spawn a tool, mapping "not found in PATH" to an actionable error.
fn run_tool(name: &str, args: &[&str], what: &str) -> Result<std::process::Output, CliError> {
    Command::new(name).args(args).output().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            CliError::Usage(format!(
                "'{name}' is required to {what} but was not found in PATH; \
                 install it via your system package manager"
            ))
        } else {
            CliError::Usage(format!("failed to start '{name}' to {what}: {err}"))
        }
    })
}

/// Spawn a tool that must exit successfully.
fn run_tool_checked(name: &str, args: &[&str], what: &str) -> Result<(), CliError> {
    let output = run_tool(name, args, what)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(tool_failure(name, what, &output))
    }
}

/// Build the standard "tool failed" error with a trimmed stderr excerpt.
fn tool_failure(name: &str, what: &str, output: &std::process::Output) -> CliError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Truncate by chars: a raw byte slice could split a multibyte UTF-8
    // sequence and panic.
    let detail: String = stderr.trim().chars().take(300).collect();
    CliError::Usage(format!(
        "failed to {what}: '{name}' exited with {}{}",
        output.status,
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    ))
}

/// Borrow a path as `&str` for argv, failing closed on non-UTF-8.
fn path_str(path: &Path) -> Result<&str, CliError> {
    path.to_str()
        .ok_or_else(|| CliError::Usage(format!("path is not valid UTF-8: {}", path.display())))
}

/// Private scratch directory (0700 on unix) removed on every return path,
/// including errors and panics unwinding through `run`.
struct TempWorkDir {
    path: PathBuf,
}

/// Nanoseconds since the Unix epoch, or 0 if the clock predates it —
/// uniqueness still comes from pid and the retry counter.
fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

impl TempWorkDir {
    fn new() -> Result<Self, CliError> {
        let base = std::env::temp_dir();
        for attempt in 0..100u32 {
            let path = base.join(format!(
                "omp-profile-self-update-{}-{}-{attempt}",
                std::process::id(),
                now_nanos()
            ));
            if path.exists() {
                continue;
            }
            create_private_dir(&path)?;
            return Ok(Self { path });
        }
        Err(CliError::Usage(format!(
            "could not create a private temporary directory under {}",
            base.display()
        )))
    }
}

impl Drop for TempWorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|err| io_err(path, err))
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> Result<(), CliError> {
    fs::create_dir(path).map_err(|err| io_err(path, err))
}

/// Restrict a scratch file to the owner (0600 on unix).
#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|err| io_err(path, err))
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> Result<(), CliError> {
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|err| io_err(path, err))
}

#[cfg(not(unix))]
fn make_executable(path: &Path) -> Result<(), CliError> {
    let mut perms = fs::metadata(path)
        .map_err(|err| io_err(path, err))?
        .permissions();
    perms.set_readonly(false);
    fs::set_permissions(path, perms).map_err(|err| io_err(path, err))
}

/// Current executable's permission bits, or `0o755` if unavailable.
#[cfg(unix)]
fn current_mode(current: &Path) -> Result<u32, CliError> {
    use std::os::unix::fs::PermissionsExt;
    Ok(fs::metadata(current)
        .map_err(|err| io_err(current, err))?
        .permissions()
        .mode()
        & 0o7777)
}

#[cfg(not(unix))]
fn current_mode(_current: &Path) -> Result<u32, CliError> {
    Ok(0o755)
}

#[cfg(unix)]
fn set_permissions(path: &Path, mode: u32) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|err| io_err(path, err))
}

#[cfg(not(unix))]
fn set_permissions(path: &Path, _mode: u32) -> Result<(), CliError> {
    make_executable(path)
}
