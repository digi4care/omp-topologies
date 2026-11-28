#!/bin/sh
# Run the Rust quality gates in a pinned Docker toolchain.
#
# This is the single gate entry point: it is what developers run locally and
# what the GitHub Actions pull-request workflow (.github/workflows/rust-gates.yml)
# invokes, so local and CI results always come from the same pinned toolchain.
#
# Requires: docker, unless OMP_TOPOLOGY_RUST_CONTAINER=1 selects the
# already-containerized path used by compose.yaml.
#
# Toolchain: rust:1.100.0 (matches rust-toolchain.toml). rustfmt and clippy
# are installed by the outer Docker path and preinstalled in the Compose image;
# no host Rust toolchain is needed.
# Fish and Zsh are installed for the actual shell-completion integration probes.
#
# Gates (any failure fails the script):
#   cargo fmt --all -- --check
#   cargo clippy --locked --all-targets -- -D warnings
#   cargo test --locked
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE="rust:1.100.0"

if [ "${OMP_TOPOLOGY_RUST_CONTAINER:-0}" != "1" ]; then
    # Build artifacts go to a container-local scratch dir, never the host
    # repository's (possibly root-owned or reused) target/ directory.
    exec docker run --rm \
        --volume "$ROOT":/repo \
        --workdir /repo \
        --env CARGO_TARGET_DIR=/tmp/omp-topology-target \
        --env OMP_TOPOLOGY_RUST_CONTAINER=1 \
        "$IMAGE" sh -c '
            set -eu
            apt-get update
            apt-get install -y --no-install-recommends fish zsh
            rm -rf /var/lib/apt/lists/*
            rustup component add rustfmt clippy
            exec ./scripts/rust-gates.sh
        '
fi

cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
