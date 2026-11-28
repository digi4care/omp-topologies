# Pinned Rust toolchain for the persistent local dev container.
#
# Compose bind-mounts the repository at /workspace and keeps Cargo caches on
# named volumes (see compose.yaml), so this image carries only the toolchain.
# CARGO_HOME/CARGO_TARGET_DIR are set by compose.yaml, not here, so the
# image's own cargo/rustup installation under /usr/local stays intact.
FROM rust:1.100.0

# The CLI integration tests execute generated completions in all supported
# shells, not just inspect their output.
RUN apt-get update \
    && apt-get install -y --no-install-recommends fish zsh \
    && rm -rf /var/lib/apt/lists/*
RUN rustup component add rustfmt clippy

WORKDIR /workspace
