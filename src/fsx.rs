//! Filesystem helpers: atomic writes, private state directories, symlink
//! detection, path resolution, and hashing.
//!
//! All mutating writes go through [`atomic_write`] (temp file in the target
//! directory + rename) so a crash never leaves a half-written file. State
//! files additionally get owner-only permissions via [`write_private`].

use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::Digest;

use crate::error::{CliError, io_err};

/// Owner-only permissions for state directories and files.
const STATE_DIR_MODE: u32 = 0o700;
const STATE_FILE_MODE: u32 = 0o600;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Hex-encoded SHA-256 of `data`.
///
/// Complexity: O(N) for N input bytes; the output is built in one
/// preallocated 64-byte string instead of per-byte temporary Strings.
pub fn sha256_hex(data: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// `true` if the path exists (including dangling symlinks).
pub fn lexists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// `true` if the final component is a symlink.
pub fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

/// `true` if any existing component of `path` (including a dangling final
/// link) is a symlink. Stops at the first component that does not exist.
///
/// Relative paths are first anchored at the physical working directory, so
/// a `..` component resolves exactly as the kernel resolves it: the lexical
/// pop is only faithful because every component probed so far exists and is
/// not a symlink, which the loop guarantees before popping.
///
/// Complexity: O(C) `lstat` calls for C path components, stopping early at
/// the first missing component.
pub fn has_symlink_component(path: &Path) -> bool {
    let anchored;
    let path = if path.is_absolute() {
        path
    } else {
        // `canonicalize` matches the physical directory the kernel resolves
        // relative paths against (unlike `current_dir` on hosts where a
        // parent of the working directory is a symlink); fall back to the
        // lexical working directory, then to a best-effort join.
        let cwd = std::fs::canonicalize(".")
            .or_else(|_| std::env::current_dir())
            .unwrap_or_else(|_| PathBuf::from("."));
        anchored = cwd.join(path);
        &anchored
    };
    let mut probe = PathBuf::from("/");
    for part in path.components() {
        match part {
            Component::RootDir => probe.push("/"),
            Component::Normal(segment) => probe.push(segment),
            Component::CurDir => continue,
            // Popping the root is a no-op, so `..` above `/` stays at `/`.
            Component::ParentDir => {
                probe.pop();
            }
            Component::Prefix(prefix) => probe.push(prefix.as_os_str()),
        }
        match std::fs::symlink_metadata(&probe) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return true;
                }
            }
            Err(_) => break,
        }
    }
    false
}

/// `true` if `rel` is a safe relative POSIX path inside the agent dir.
pub fn safe_rel(rel: &str) -> bool {
    if rel.is_empty() || rel.starts_with('/') || rel.contains('\\') {
        return false;
    }
    rel.split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Create `path` and any missing parents, chmod'ing only the directories
/// this call created to owner-only mode; a chmod failure is an error, not
/// a silently looser directory.
///
/// Complexity: O(C) path-component probes plus one `mkdir -p` syscall
/// chain and one chmod per created directory.
pub fn makedirs_private(path: &Path) -> std::io::Result<()> {
    let mut created: Vec<PathBuf> = Vec::new();
    let mut current = path;
    while !lexists(current) {
        created.push(current.to_path_buf());
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    std::fs::create_dir_all(path)?;
    for directory in &created {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(STATE_DIR_MODE))?;
    }
    Ok(())
}

pub(crate) fn temp_name(prefix: &OsStr) -> OsString {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    // Only the final component may seed the name: a full-path prefix would
    // reintroduce `/` separators into the temp file name (breaking relative
    // nested targets) and leak the whole path into the directory listing.
    // The bytes are appended without re-encoding, so non-UTF-8 names stay
    // byte-faithful; uniqueness comes from pid + nanos + counter.
    let mut name = prefix.to_os_string();
    name.push(format!(".tmp-{}-{nanos}-{counter}", std::process::id()));
    name
}

/// Temp path for an atomic write beside `path`.
fn temp_path(path: &Path) -> PathBuf {
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(temp_name(
            path.file_name()
                .unwrap_or_else(|| OsStr::new("omp-profile")),
        ))
}

/// Write `data` to `path` atomically (temp file + rename). The temp file is
/// created owner-only (0600) before any optional final mode is applied, so a
/// crash or rollback can never expose a file with looser, umask-derived
/// permissions; with `mode: None` the restrictive default stays, matching
/// the previous installer's mkstemp behavior.
pub fn atomic_write(path: &Path, data: &[u8], mode: Option<u32>) -> Result<(), CliError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| io_err(parent, err))?;
    }
    let tmp = temp_path(path);
    let write_result = (|| -> Result<(), CliError> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|err| io_err(&tmp, err))?;
        file.write_all(data).map_err(|err| io_err(&tmp, err))?;
        file.sync_all().map_err(|err| io_err(&tmp, err))?;
        if let Some(mode) = mode {
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
                .map_err(|err| io_err(&tmp, err))?;
        }
        std::fs::rename(&tmp, path).map_err(|err| io_err(path, err))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write_result
}

/// Atomically create a state file (owner-only permissions, owner-only
/// parent directories) — used for manifests, backups, and snapshots.
pub fn write_private(path: &Path, data: &[u8]) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        makedirs_private(parent).map_err(|err| io_err(parent, err))?;
    }
    atomic_write(path, data, Some(STATE_FILE_MODE))
}

/// Atomically create `path` holding `data` only while the name is still
/// free — never following a symlink and never replacing a concurrent file,
/// directory, or symlink that appeared at `path`.
///
/// Publication writes a private same-directory temp, fsyncs it, and then
/// hard-links it to `path`: the link fails with `AlreadyExists` whenever
/// the name is taken by anything at all, and it never follows a
/// final-component symlink, so a concurrent create always wins and is left
/// byte-for-byte untouched (rename-without-replace, portable across the
/// supported Unix targets). `Ok(true)` means this call published the data,
/// `Ok(false)` means a concurrent entry exists and was preserved. As with
/// [`atomic_write`], the temp starts owner-only (0600) and `mode` is
/// applied before publication; the temp name is cleaned up either way.
#[cfg(test)]
pub fn atomic_create_exclusive(
    path: &Path,
    data: &[u8],
    mode: Option<u32>,
) -> Result<bool, CliError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| io_err(parent, err))?;
    }
    let tmp = temp_path(path);
    let create_result = (|| -> Result<bool, CliError> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|err| io_err(&tmp, err))?;
        file.write_all(data).map_err(|err| io_err(&tmp, err))?;
        file.sync_all().map_err(|err| io_err(&tmp, err))?;
        if let Some(mode) = mode {
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
                .map_err(|err| io_err(&tmp, err))?;
        }
        match std::fs::hard_link(&tmp, path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(err) => Err(io_err(path, err)),
        }
    })();
    // The temp name is ours whether or not publication succeeded; a
    // published path holds the data independent of it.
    let _ = std::fs::remove_file(&tmp);
    create_result
}

/// Timestamp in the previous installer's format, local time:
/// `YYYY-MM-DDTHH-MM-SS-microseconds`.
pub fn timestamp() -> String {
    use time::format_description::FormatItem;
    use time::macros::format_description;

    const FORMAT: &[FormatItem<'static>] =
        format_description!("[year]-[month]-[day]T[hour]-[minute]-[second]-[subsecond digits:6]");
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    now.format(FORMAT)
        .unwrap_or_else(|_| "1970-01-01T00-00-00-000000".to_string())
}

/// Expand a leading `~` to `$HOME`.
pub fn expanduser(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    path.to_path_buf()
}

/// Resolve a path that may not exist yet: canonicalize the longest existing
/// ancestor and append the rest lexically.
///
/// Complexity: O(C) component probes and at most one `canonicalize` per
/// existing ancestor level; the lexical `normalize_dotdot` pass is O(C).
pub fn resolve_loose(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut missing: Vec<&std::ffi::OsStr> = Vec::new();
    let mut base = absolute.as_path();
    while let Some(parent) = base.parent() {
        missing.push(match base.file_name() {
            Some(name) => name,
            None => break,
        });
        base = parent;
        if let Ok(canonical) = std::fs::canonicalize(base) {
            let mut result = canonical;
            for part in missing.iter().rev() {
                result.push(part);
            }
            return normalize_dotdot(&result);
        }
    }
    normalize_dotdot(&absolute)
}

/// Lexically resolve `.` and `..` components of an absolute path.
fn normalize_dotdot(path: &Path) -> PathBuf {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => parts.clear(),
            Component::Normal(segment) => parts.push(segment.to_os_string()),
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            Component::Prefix(prefix) => parts.push(prefix.as_os_str().to_os_string()),
        }
    }
    let mut result = PathBuf::from("/");
    for part in parts {
        result.push(part);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            loop {
                let path = std::env::temp_dir().join(format!(
                    "omp-fsx-test-{}-{}",
                    std::process::id(),
                    NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed)
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(err) => panic!("failed to create test directory: {err}"),
                }
            }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn atomic_create_exclusive_creates_an_absent_path() {
        let temp = TempDir::new();
        let path = temp.0.join("new-file");

        assert!(super::atomic_create_exclusive(&path, b"created", None).unwrap());
        assert_eq!(std::fs::read(path).unwrap(), b"created");
    }

    #[test]
    fn atomic_create_exclusive_preserves_an_existing_file() {
        let temp = TempDir::new();
        let path = temp.0.join("existing-file");
        std::fs::write(&path, b"original bytes").unwrap();

        assert!(!super::atomic_create_exclusive(&path, b"replacement", None).unwrap());
        assert_eq!(std::fs::read(path).unwrap(), b"original bytes");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_create_exclusive_preserves_an_existing_symlink() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new();
        let target = temp.0.join("target");
        let path = temp.0.join("link");
        std::fs::write(&target, b"target bytes").unwrap();
        symlink(&target, &path).unwrap();

        assert!(!super::atomic_create_exclusive(&path, b"replacement", None).unwrap());
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"target bytes");
        assert_eq!(std::fs::read_link(&path).unwrap(), target);
    }
}
