# Releasing omp-profile

How maintainers publish a signed GitHub Release, and how the bootstrap
installer verifies it. Read this before pushing a release tag.

## Trust model

- Every release publishes `SHA256SUMS` (SHA-256 of every archive) plus a
  detached RSA/SHA-256 signature `SHA256SUMS.sig` over that file.
- Only the **public** half of the release key is committed, at
  `keys/omp-topology-release-public.pem`. The **private** key lives solely in
  the GitHub Actions secret `OMP_TOPOLOGY_RELEASE_PRIVATE_KEY`. No private
  key material may ever be committed.
- There is no unsigned fallback: the release workflow aborts before
  publishing if the secret is missing/invalid or the signature does not
  verify against the committed public key, and the installer refuses to
  install anything that fails verification.

## Supported release targets

| Target | Runner | Platform |
| --- | --- | --- |
| `x86_64-unknown-linux-musl` | `ubuntu-latest` | Linux x86_64 (static) |
| `x86_64-apple-darwin` | `macos-15-intel` | macOS Intel |
| `aarch64-apple-darwin` | `macos-15` | macOS Apple Silicon |

The Linux x86_64 binary is a statically linked musl build: it has no glibc
version dependency and runs on any Linux x86_64 system with a modern kernel.
Only `x86_64` Linux is supported — there is no Linux aarch64, armv7, or
glibc-specific build.

All macOS builds run on native runners (`macos-13` was retired in
December 2025; `macos-15-intel` is available through August 2027).

Every release asset is named `omp-profile-${VERSION}-${TARGET}.tar.gz`
(for example `omp-profile-0.2.0-x86_64-unknown-linux-musl.tar.gz`) and
contains a single `omp-profile` binary. `SHA256SUMS` lists these exact
archive filenames. The release tag is `v${VERSION}` (for example `v0.2.0`),
and `Cargo.toml`'s `version` must match the tag or the workflow fails.

## One-time setup: provision the release key

The public key at `keys/omp-topology-release-public.pem` already exists in
the repository. To (re)create a keypair, for example when rotating:

```sh
# Run locally; keep the private key out of the repository.
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:4096 -out release-private-key.pem
openssl rsa -in release-private-key.pem -pubout -out keys/omp-topology-release-public.pem
gh secret set OMP_TOPOLOGY_RELEASE_PRIVATE_KEY < release-private-key.pem
shred -u release-private-key.pem   # or: rm -P release-private-key.pem
```

**Rotating the key also requires updating the copy embedded in
`scripts/install-release.sh`**: replace the PEM block between
`-----BEGIN PUBLIC KEY-----` and `-----END PUBLIC KEY-----` in that script
with the exact contents of the newly committed
`keys/omp-topology-release-public.pem`. The release job compares the two and
aborts before publishing if they drift apart, but updating both in the same
commit avoids a broken installer between the key rotation and the next
release.

The committed public PEM must match the secret. The release job signs and
then verifies against the public PEM, so a mismatch fails the release before
anything is published.

## Publishing a release

1. Ensure `Cargo.toml` has the release version and the Rust gates pass
   (`scripts/rust-gates.sh`).
2. Tag and push (tags must be strict SemVer triples — `vMAJOR.MINOR.PATCH`,
   no leading zeroes, no prerelease suffixes; the workflow rejects anything
   else):

   ```sh
   git tag v0.2.0
   git push origin v0.2.0
   ```

3. The `Release` workflow (`.github/workflows/release.yml`) builds all three
   targets on native runners, checks that each binary runs and reports its
   version, packages the archives, generates `SHA256SUMS`, signs it, verifies
   the signature against the committed public key, and creates the GitHub
   Release with all assets.

## Installing from a release

The bootstrap installer needs **no sudo** and installs into `~/.local/bin`
(override with `--bin-dir` or `$OMP_TOPOLOGY_BIN_DIR`):

```sh
# Latest release:
sh scripts/install-release.sh

# Pinned release and/or custom location:
sh scripts/install-release.sh --version v0.2.0 --bin-dir ~/.local/bin
```

Or pipe it directly:

```sh
curl -fsSL https://raw.githubusercontent.com/digi4care/omp-profile-installer/main/scripts/install-release.sh | sh
```

The installer selects the asset for the current platform (Linux x86_64,
macOS Intel, macOS Apple Silicon), downloads `SHA256SUMS` and
`SHA256SUMS.sig` from the same release, verifies the RSA/SHA-256 signature
with the embedded public key, verifies the asset's checksum entry, and only
then replaces the installed binary. Any verification failure leaves the
previous installation untouched. Re-running it is safe and updates in place.

### Installer requirements

- POSIX `sh`, `curl`, `tar`, and `sha256sum` (Linux) or `shasum` (macOS).
- **OpenSSL** (`openssl` on `PATH`) for signature verification. It ships
  with macOS and virtually all Linux distributions; install it via the
  system package manager if it is missing.

## Verifying a release manually

```sh
# 1. Verify the checksum manifest against the committed public key.
openssl dgst -sha256 -verify keys/omp-topology-release-public.pem \
    -signature SHA256SUMS.sig SHA256SUMS

# 2. Verify an archive against the manifest.
#    Linux:
sha256sum -c <(grep omp-profile-0.2.0-x86_64-unknown-linux-musl.tar.gz SHA256SUMS)
#    macOS (stock BSD userland has no sha256sum; run in bash/zsh, since
#    `<(...)` process substitution is not POSIX):
shasum -a 256 -c <(grep omp-profile-0.2.0-aarch64-apple-darwin.tar.gz SHA256SUMS)
```
