#!/bin/bash
# Build environment for the herdr fork on NERSC Perlmutter.
# Usage:  source build-env.sh   then   cargo build --release -j 8
#
# Everything is redirected to $PSCRATCH. $HOME is a 40 GiB quota running ~73%
# full and must not hold cargo/zig artifacts.

module load rust/stable 2>/dev/null

# The module's own RUSTUP_HOME is read-only shared software and errors on use.
export RUSTUP_HOME="$PSCRATCH/rust/rustup"
export CARGO_HOME="$PSCRATCH/rust/cargo"
export CARGO_TARGET_DIR="$PSCRATCH/rust/target"

# build.rs shells out to `zig build` for the vendored libghostty-vt.
# Requires >= 0.15.2 (vendor/libghostty-vt/build.zig.zon). No NERSC module exists.
export ZIG="$PSCRATCH/tools/zig-0.15.2/zig"
export ZIG_GLOBAL_CACHE_DIR="$PSCRATCH/tools/zig-cache"
export ZIG_LOCAL_CACHE_DIR="$PSCRATCH/tools/zig-cache-local"

export PATH="$CARGO_HOME/bin:$PSCRATCH/tools/zig-0.15.2:$PATH"

mkdir -p "$RUSTUP_HOME" "$CARGO_HOME" "$CARGO_TARGET_DIR" \
         "$ZIG_GLOBAL_CACHE_DIR" "$ZIG_LOCAL_CACHE_DIR"

# Shared login node: keep parallelism modest.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-8}"
