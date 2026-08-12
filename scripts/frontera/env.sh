# Source this before building herdr on Frontera.
#
#     source scripts/frontera/env.sh
#     cargo build --release --locked
#
# Frontera-specific by intent. Do not try to run this anywhere else.

# --- where everything lives ----------------------------------------------------------
# /home1 is at ~85% of its 200k inode quota, so nothing build-shaped goes there.
export HERDR_BUILD_PREFIX=/work2/08526/jdgeorga/frontera/herdr-build
export RUSTUP_HOME="$HERDR_BUILD_PREFIX/rustup"
export CARGO_HOME="$HERDR_BUILD_PREFIX/cargo"
export CARGO_TARGET_DIR="$HERDR_BUILD_PREFIX/target"
export HERDR_FRONTERA_PREBUILT="$HERDR_BUILD_PREFIX/prebuilt"
export PATH="$CARGO_HOME/bin:$PATH"

# --- the zig escape hatch -------------------------------------------------------------
# build.rs shells out to `zig build`, which cannot run here (kernel 3.10 has no statx).
# It honours $ZIG, so we point it at a script that verifies and stages a prebuilt archive.
_herdr_frontera_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
export ZIG="$_herdr_frontera_root/scripts/frontera/zig-shim.sh"

# --- XALT: stop it from hijacking every link ------------------------------------------
# COMPILER_PATH forces collect2 to find /opt/apps/xalt/xalt/bin/ld, which is a *bash
# script* that rewrites the link argv, injects a watermark object, appends -ldcgm -luuid,
# and (with function tracking on) runs a second full link per invocation. Rust links are
# static-archive-heavy and order-sensitive; this is a prime source of inscrutable
# undefined-reference errors. The bypass at xalt/bin/ld:198 is a clean pass-through.
export XALT_EXECUTABLE_TRACKING=no
unset LD_PRELOAD
unset COMPILER_PATH

# --- pin the linker driver -------------------------------------------------------------
# Bare `cc` is GCC 4.8.5 with binutils 2.27. gcc 8.3.0 is present but is NOT a loaded
# module -- it is only on PATH incidentally, via dotfiles. Pin it explicitly and fail
# loudly if TACC moves it, rather than silently falling back to 4.8.5.
_herdr_gcc=/opt/apps/gcc/8.3.0/bin/gcc
if [ -x "$_herdr_gcc" ] && "$_herdr_gcc" --version 2>/dev/null | head -1 | grep -q '8\.3\.0'; then
    export CC="$_herdr_gcc"
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$_herdr_gcc"
else
    echo "frontera env.sh: WARNING: $_herdr_gcc missing or not 8.3.0;" >&2
    echo "  falling back to bare cc ($(cc --version 2>/dev/null | head -1))." >&2
    echo "  Links may behave differently than documented." >&2
fi

# --- librt --------------------------------------------------------------------------
# libghostty-vt's kitty graphics code calls shm_open/shm_unlink. On glibc 2.17 those live
# in librt; they only moved into libc proper in glibc 2.34, so upstream's toolchains never
# have to ask for them and build.rs does not emit a link directive for it. Without this the
# final link fails with "undefined reference to shm_open".
#
# Done with RUSTFLAGS rather than a build.rs edit, to keep the branch diff free of
# upstream-owned files. Appending, so an existing RUSTFLAGS is preserved.
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C link-arg=-lrt"

# --- libssp shadow ---------------------------------------------------------------------
# Juliaup ships its own libssp.so.0 and .bashrc puts it first on LD_LIBRARY_PATH, but it
# lacks __vsnprintf_chk@LIBSSP_1.0. Any Intel-built binary that needs it dies with a
# relocation error. The working copy is in gcc 8.3.0's lib64.
if [ -d /opt/apps/gcc/8.3.0/lib64 ]; then
    export LD_LIBRARY_PATH="/opt/apps/gcc/8.3.0/lib64:${LD_LIBRARY_PATH:-}"
fi

unset _herdr_frontera_root _herdr_gcc

echo "frontera env: rust=$(rustc --version 2>/dev/null | cut -d' ' -f2) \
target_dir=$CARGO_TARGET_DIR prebuilt=$HERDR_FRONTERA_PREBUILT xalt=off"
