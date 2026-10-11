# OSS Scanner build image (https://github.com/anthropics/oss-scanner).
# The scanner builds this with the repository as the build context, then runs
# its audit offline, so every crate the scorer and its tests need (including
# the `neat-core` git dependency) is fetched here. The base image matches the
# channel pinned in rust-toolchain.toml.
FROM rust:1.99.0-trixie

# The base image already carries gcc, git and pkg-config. Mesa's lavapipe is a
# CPU-only Vulkan driver: with it `wgpu` finds a software adapter, so the GPU
# kernels can be exercised on a host with no GPU (`--gpu off` forces the CPU
# pipeline instead). GPU tests skip cleanly when no adapter is found at all.
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates libvulkan1 mesa-vulkan-drivers \
    && rm -rf /var/lib/apt/lists/*

# scanner contract: the checkout lives inside the image, at /src
COPY . /src
WORKDIR /src

# Full debug info and frame pointers. Setting RUSTFLAGS also replaces the
# `target-cpu=native` flag in .cargo/config.toml, so the binaries do not depend
# on the CPU that happened to build the image.
ENV CARGO_PROFILE_DEV_DEBUG=true \
    CARGO_PROFILE_TEST_DEBUG=true \
    RUSTFLAGS="-C force-frame-pointers=yes"

# Install the pinned toolchain's components, fetch every crate against the
# lockfile, then build the binaries and every test, bench and example target.
RUN rustup toolchain install \
    && rustc --version \
    && cargo fetch --locked \
    && cargo build --locked --workspace --all-features \
    && cargo test --locked --workspace --all-features --all-targets --no-run

# From here on the audit runs offline; keep cargo from reaching for the network.
ENV CARGO_NET_OFFLINE=true

# Smoke-run the library unit tests; a failure warns rather than aborting the
# build. The full suite is `cargo test --workspace --all-features -- --test-threads=2`.
RUN (timeout 900 cargo test --locked --workspace --all-features --lib -- --test-threads=2 \
      || echo "WARNING: smoke tests reported failures")
