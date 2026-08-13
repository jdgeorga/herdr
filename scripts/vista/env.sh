# Source this before building herdr on Vista.
#
#     source scripts/vista/env.sh
#     cargo build --release --locked
#
# Vista-specific by intent. Do not source this on Frontera; that cluster needs a great
# deal more machinery, and scripts/frontera/env.sh is its own file for that reason.
#
# This file is deliberately much shorter than the Frontera one. Every knob Frontera sets
# that is absent here was measured as unnecessary on Vista -- see README.md, section
# "What Vista does not need and why", which names the measurement for each.

# --- where everything lives -----------------------------------------------------------
# $HOME is NFS (192.168.16.21:/vista/home1) with a 500k FILE quota, 137k used, and only
# ~12.6G free of a 23.8G space quota. $WORK is Lustre with a 3,000,000 inode quota, 245k
# used, and ~690G free. A rustup toolchain plus a release target dir is tens of thousands
# of files and several GB, so none of it goes in $HOME.
#
# This is the same conclusion as Frontera but for a different reason: Frontera's /home1 is
# Lustre near a 200k inode limit; Vista's is NFS with a 500k file limit and a tight space
# quota. Both say "not in $HOME".
export HERDR_BUILD_PREFIX=/work/08526/jdgeorga/vista/herdr-build
export RUSTUP_HOME="$HERDR_BUILD_PREFIX/rustup"
export CARGO_HOME="$HERDR_BUILD_PREFIX/cargo"
export CARGO_TARGET_DIR="$HERDR_BUILD_PREFIX/target"
export PATH="$CARGO_HOME/bin:$PATH"

# --- zig: the real compiler, not a shim -----------------------------------------------
# build.rs shells out to `zig build` for the vendored libghostty-vt and honours $ZIG.
# vendor/libghostty-vt/build.zig.zon pins .minimum_zig_version = "0.15.2".
#
# This is the single largest difference from Frontera. Zig 0.15.x calls statx(2), which
# needs kernel >= 4.11; Frontera is CentOS 7 on kernel 3.10 and therefore cannot run zig
# at all, which is the sole reason scripts/frontera/ carries zig-shim.sh, provenance.json,
# fetch-artifact.sh, link-probe.sh, a prebuilt .a release asset and a CI workflow.
#
# Vista is Rocky 9.7 on kernel 5.14. Real zig runs. Verified before building anything:
#   zig version           -> 0.15.2
#   zig init && zig build -> produced a working binary, exit 0
# There is no shim here and there must never be one. See README.md.
#
# Zig is not on Vista's PATH and there is no zig Lmod module (`module spider zig` ->
# "Unable to find"), so the toolchain is unpacked under $HERDR_BUILD_PREFIX.
export HERDR_VISTA_ZIG="$HERDR_BUILD_PREFIX/zig/zig-aarch64-linux-0.15.2"
if [ -x "$HERDR_VISTA_ZIG/zig" ]; then
    export ZIG="$HERDR_VISTA_ZIG/zig"
    export PATH="$HERDR_VISTA_ZIG:$PATH"
else
    echo "vista env.sh: WARNING: no zig at $HERDR_VISTA_ZIG/zig" >&2
    echo "  build.rs will fall back to bare \`zig\` on PATH and fail." >&2
    echo "  install it:  see scripts/vista/README.md, \"Zig\"" >&2
fi

# Zig's caches default to ~/.cache/zig, which is the inode-constrained NFS $HOME. Move
# both, for the same reason CARGO_TARGET_DIR moves.
export ZIG_GLOBAL_CACHE_DIR="$HERDR_BUILD_PREFIX/zig-cache/global"
export ZIG_LOCAL_CACHE_DIR="$HERDR_BUILD_PREFIX/zig-cache/local"

echo "vista env: rust=$(rustc --version 2>/dev/null | cut -d' ' -f2) \
zig=$(zig version 2>/dev/null) target_dir=$CARGO_TARGET_DIR"
