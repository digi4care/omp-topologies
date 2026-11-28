#!/bin/sh
# Fail-closed bootstrap installer for the omp-profile CLI.
#
# Downloads the release asset matching the current platform from
# https://github.com/digi4care/omp-profile-installer, verifies the release signature
# and the asset checksum, and only then installs the binary into a
# user-owned bin directory (no sudo). Safe to re-run for updates: the
# installed binary is replaced only after every verification step has
# passed; any failure leaves the previous installation untouched.
#
# Trust model (no unsigned fallback):
#   - The release publishes SHA256SUMS and a detached RSA/SHA-256 signature
#     SHA256SUMS.sig, made with the private key held only in the GitHub
#     Actions secret OMP_TOPOLOGY_RELEASE_PRIVATE_KEY.
#   - This script embeds the matching public key (also committed at
#     keys/omp-topology-release-public.pem). It verifies SHA256SUMS.sig
#     first, then the selected asset's SHA-256 entry inside SHA256SUMS.
#   - If any check fails, the script exits with an error and does not
#     install or replace anything.
#
# Requirements: POSIX sh, curl, openssl, tar, sha256sum (or shasum on macOS).
#
# Usage:
#   scripts/install-release.sh [--version TAG] [--bin-dir DIR]
#   curl -fsSL https://raw.githubusercontent.com/digi4care/omp-profile-installer/main/scripts/install-release.sh | sh
#
# Environment overrides:
#   OMP_TOPOLOGY_VERSION   release tag to install (default: latest), e.g. v0.2.0
#   OMP_TOPOLOGY_BIN_DIR   install directory (default: ~/.local/bin)
#   OMP_TOPOLOGY_REPO      owner/repo override (default: digi4care/omp-profile-installer)
set -eu

REPO="${OMP_TOPOLOGY_REPO:-digi4care/omp-profile-installer}"
VERSION="${OMP_TOPOLOGY_VERSION:-latest}"
BIN_DIR="${OMP_TOPOLOGY_BIN_DIR:-$HOME/.local/bin}"

usage() {
    cat <<'EOF'
Usage: install-release.sh [--version TAG] [--bin-dir DIR]

  --version TAG   Release tag to install, e.g. v0.2.0 (default: latest).
  --bin-dir DIR   Install directory (default: ~/.local/bin, or
                  $OMP_TOPOLOGY_BIN_DIR). Created if missing; no sudo used.

Supported platforms:
  Linux x86_64, macOS Intel (x86_64), macOS Apple Silicon (arm64)
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        -h|--help)
            usage
            exit 0
            ;;
        --version)
            [ "$#" -ge 2 ] || { echo "install-release: error: --version requires a tag argument" >&2; exit 2; }
            VERSION="$2"
            shift 2
            ;;
        --bin-dir)
            [ "$#" -ge 2 ] || { echo "install-release: error: --bin-dir requires a directory argument" >&2; exit 2; }
            BIN_DIR="$2"
            shift 2
            ;;
        *)
            echo "install-release: error: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

die() {
    echo "install-release: error: $*" >&2
    exit 1
}

have() {
    command -v "$1" >/dev/null 2>&1
}

have curl || die "curl is required but was not found in PATH"
have openssl || die "openssl is required but was not found in PATH (install it via your package manager)"
have tar || die "tar is required but was not found in PATH"

if have sha256sum; then
    SHA256_CHECK="sha256sum -c"
elif have shasum; then
    SHA256_CHECK="shasum -a 256 -c"
else
    die "neither sha256sum nor shasum was found in PATH"
fi

# --- Platform selection -----------------------------------------------------

uname_s="$(uname -s)"
uname_m="$(uname -m)"

case "$uname_s" in
    Linux)
        case "$uname_m" in
            x86_64) target="x86_64-unknown-linux-musl" ;;
            *) die "unsupported Linux architecture: $uname_m (supported: x86_64)" ;;
        esac
        ;;
    Darwin)
        case "$uname_m" in
            x86_64) target="x86_64-apple-darwin" ;;
            arm64|aarch64) target="aarch64-apple-darwin" ;;
            *) die "unsupported macOS architecture: $uname_m (supported: x86_64, arm64)" ;;
        esac
        ;;
    *)
        die "unsupported operating system: $uname_s (supported: Linux, macOS)"
        ;;
esac

# --- Release tag resolution -------------------------------------------------

if [ "$VERSION" = "latest" ]; then
    tag="$(curl -fsSL --proto '=https' --proto-redir '=https' -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" \
        || die "could not resolve the latest release of $REPO (network error or no release exists)")"
    tag="${tag##*/}"
    [ -n "$tag" ] || die "could not resolve the latest release tag of $REPO"
else
    tag="$VERSION"
fi

if ! printf '%s' "$tag" | grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'; then
    die "invalid release tag '$tag' (expected e.g. v0.2.0)"
fi

version="${tag#v}"
asset="omp-profile-${version}-${target}.tar.gz"
base="https://github.com/$REPO/releases/download/${tag}"

# --- Download into a private temp directory ---------------------------------

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/omp-profile-install.XXXXXX")" \
    || die "could not create a temporary directory"
candidate=""
cleanup() {
    rm -rf "$tmpdir"
    rm -f "$candidate"
}
trap cleanup EXIT HUP INT TERM

echo "install-release: downloading $asset from $REPO release $tag"
# --proto/--proto-redir '=https' keeps every request and redirect on HTTPS.
curl -fsSL --retry 3 --proto '=https' --proto-redir '=https' -o "$tmpdir/$asset" "$base/$asset" \
    || die "failed to download $base/$asset"
curl -fsSL --retry 3 --proto '=https' --proto-redir '=https' -o "$tmpdir/SHA256SUMS" "$base/SHA256SUMS" \
    || die "failed to download $base/SHA256SUMS"
curl -fsSL --retry 3 --proto '=https' --proto-redir '=https' -o "$tmpdir/SHA256SUMS.sig" "$base/SHA256SUMS.sig" \
    || die "failed to download $base/SHA256SUMS.sig"

# --- Signature verification (RSA/SHA-256, detached) --------------------------

# Public key matching keys/omp-topology-release-public.pem. The release
# workflow fails if this copy and the committed PEM drift apart.
pubkey="$tmpdir/release-public.pem"
cat > "$pubkey" <<'EOF'
-----BEGIN PUBLIC KEY-----
MIICIjANBgkqhkiG9w0BAQEFAAOCAg8AMIICCgKCAgEAoiRSlxjSe5G2Jw+Gx7dz
H55Oj5K6jjwWq7ApPMmxphYgK64RjBGDmDISqYnf+qegG7SO0LomVXLNGyUPy8/j
a535m2OZ/yXQi2CUaxGzQsOZ3gr6WrV022xAcJMHqKP8oHMju8NrvrQE9oyM01uW
oNV/3AJPoiX6lxe19syb7wwRL1JtRXvBOlE9HZw/WL2UAu1bPnDGfwwURVa3nkZr
sgRsagJkiuxeUhRsbzfgi35tYsWqIK12C3XPcay70zheMTeLwm+DA9VOXl+NtLpk
q5YDVdvuWo0HeUVuMUB2a491dK0/MAAF293kxurFSs00JuKndHm2jVPCV8AXtMgF
Z7p/WPmdJ3NF2tibn6q7T0xOvCGk3BcsykfGe9+rnIGXvaK882enMtfJmrJWivMI
M+aVglhw0R9apIZ8SbzL6i1vlVXtOyVqFbG62pbKowkbaveVuN2N6xeHsq2n1ksa
WJ+xCKG+fpk3MK423rejU7rMes6jKWLLjsYQm3MKVKEvbT9BC/cHr/GnLvf4ENth
Z0W6drBKp5uRHiwJG3sx+xIJvT1YkFl9QiCjDPwgGcSaMIuAlw6624XLkecKXsvm
DpEICYwCsM86njoz7HKdgyliH/i+z8HmXDUbhxzRgEpac8pRumFussX/QShu5G/Z
rFg/8h8tz4zyv3KYxS/Hm1kCAwEAAQ==
-----END PUBLIC KEY-----
EOF

echo "install-release: verifying SHA256SUMS signature (RSA/SHA-256)"
openssl dgst -sha256 -verify "$pubkey" -signature "$tmpdir/SHA256SUMS.sig" "$tmpdir/SHA256SUMS" \
    || die "signature verification failed for SHA256SUMS; refusing to install (no unsigned fallback)"

# --- Checksum verification of the selected asset ----------------------------

# Select the checksum row by exact filename match ($2 compared byte-for-byte
# against $asset, no regex metacharacters); require exactly one row.
( cd "$tmpdir" && awk -v asset="$asset" \
    '$2 == asset { rows += 1; line = $0 } END { if (rows == 1) print line; else exit 1 }' \
    SHA256SUMS > asset.sha256 ) \
    || die "expected exactly one SHA256SUMS entry for $asset; refusing to install"
( cd "$tmpdir" && $SHA256_CHECK asset.sha256 ) \
    || die "checksum mismatch for $asset; refusing to install"

# --- Extract and install (only after all checks passed) ----------------------

tar -xzf "$tmpdir/$asset" -C "$tmpdir" \
    || die "failed to extract $asset"
[ -f "$tmpdir/omp-profile" ] \
    || die "archive $asset does not contain the omp-profile binary"

mkdir -p "$BIN_DIR" || die "could not create install directory $BIN_DIR"

# A directory at the destination would make `mv` nest the candidate inside
# it instead of replacing it, and the post-install probe would silently
# report "unknown" — reject it up front.
if [ -d "$BIN_DIR/omp-profile" ]; then
    die "$BIN_DIR/omp-profile is a directory; refusing to install over it (remove the directory and re-run)"
fi

# Stage the verified candidate inside BIN_DIR first: $TMPDIR may live on a
# different filesystem, and the final replacement must be a same-directory
# rename so an existing binary is swapped atomically or left untouched.
candidate="$(mktemp "$BIN_DIR/.omp-profile.new.XXXXXX")" \
    || die "could not create a staging file in $BIN_DIR"
cp "$tmpdir/omp-profile" "$candidate" || die "could not stage $candidate"
chmod 0755 "$candidate" || die "could not chmod $candidate"

candidate_version="$("$candidate" --version 2>/dev/null)" \
    || die "candidate binary could not run with --version; refusing to replace the installed binary"
if [ "$candidate_version" != "omp-profile $version" ]; then
    die "candidate binary reports '${candidate_version:-no version output}', expected 'omp-profile $version'; refusing to replace the installed binary"
fi

mv -f "$candidate" "$BIN_DIR/omp-profile" \
    || die "could not replace $BIN_DIR/omp-profile"
trap - EXIT HUP INT TERM
rm -rf "$tmpdir"

installed_version="$("$BIN_DIR/omp-profile" --version 2>/dev/null || echo "unknown")"
echo "install-release: installed $installed_version"
echo "install-release: source $REPO@$tag asset $asset (signature and checksum verified)"
echo "install-release: binary location: $BIN_DIR/omp-profile"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "install-release: note: $BIN_DIR is not on PATH; add it to use 'omp-profile' directly" ;;
esac
