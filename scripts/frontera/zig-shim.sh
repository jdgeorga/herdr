#!/bin/bash
# Frontera stand-in for `zig build`, for the vendored libghostty-vt.
#
# WHY THIS EXISTS
#   Zig 0.15.x cannot run on Frontera at all: it calls statx(2) (Linux 4.11+) with no
#   fallback, and Frontera is CentOS 7 on kernel 3.10. Every invocation dies with
#   "error: unable to load package manifest ... Unexpected".
#
#   build.rs reads the ZIG environment variable and only asserts that the child exits 0
#   before emitting its link directives, so pointing ZIG at this script gets a full
#   cargo build with ZERO changes to any upstream-owned file.
#
#       ZIG=scripts/frontera/zig-shim.sh cargo build --release --locked
#
#   (scripts/frontera/env.sh sets this for you.)
#
# WHAT IT IS NOT
#   This is not a no-op. It is a verifier that happens to exit 0. It refuses to stage an
#   archive it cannot prove matches the current vendored source, because that specific
#   mismatch is the only silent failure mode in this design: src/ghostty/bindings.rs is
#   committed bindgen output with 64 #[repr(C)] types and zero layout tests, so a stale
#   archive links cleanly, boots, and reads garbage. Everything else fails loudly.
#
# build.rs invokes us with cwd = vendor/libghostty-vt and argv:
#   build -Demit-lib-vt -Doptimize=ReleaseFast -Dsimd=true -Dtarget=x86_64-linux-gnu \
#         -Dversion-string=<VERSION> -Demit-xcframework=false
set -euo pipefail

# Deterministic sort order. Without this the digest depends on locale and CI (C.UTF-8)
# would disagree with Frontera on every single build.
export LC_ALL=C

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
COMMITTED_PROVENANCE="$REPO_ROOT/scripts/frontera/provenance.json"
STAGE_DIR="${HERDR_FRONTERA_PREBUILT:-/work2/08526/jdgeorga/frontera/herdr-build/prebuilt}"
ARCHIVE="$STAGE_DIR/libghostty-vt.a"
VENDOR_REL="vendor/libghostty-vt"

die() {
    printf '\n' >&2
    printf 'frontera zig-shim: FATAL: %s\n' "$1" >&2
    shift
    for line in "$@"; do printf '  %s\n' "$line" >&2; done
    printf '\n' >&2
    exit 1
}

jget() {
    /usr/bin/python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception as exc:
    sys.stderr.write("unreadable provenance: %s\n" % exc)
    sys.exit(2)
v = d.get(sys.argv[2])
sys.stdout.write("" if v is None else str(v))
' "$1" "$2"
}

# The digest MUST prune generated directories: we stage the archive into zig-out/ below,
# which would otherwise land inside the tree we hash and change it on the next build.
vendor_digest() {
    ( cd "$REPO_ROOT" && \
      find "$VENDOR_REL" \( -name zig-out -o -name .zig-cache -o -name zig-cache \) -prune \
           -o -type f -print0 \
        | sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1 )
}

REBUILD_HINT=(
    "To rebuild the archive, push this branch so CI regenerates it:"
    "    git push origin herdr-slurm-frontera"
    "or dispatch it manually:"
    "    gh workflow run frontera-libghostty-vt.yml --repo jdgeorga/herdr --ref herdr-slurm-frontera"
    "then re-stage with:"
    "    scripts/frontera/fetch-artifact.sh"
)

# --- 0. sanity: are we being called the way we think we are? ------------------------
[ "${1:-}" = "build" ] || die \
    "expected 'build' as the first argument, got '${1:-<none>}'." \
    "build.rs's zig invocation has changed shape; re-read build.rs before trusting this shim."

# --- 1. the artifact and its provenance must both exist -----------------------------
[ -f "$COMMITTED_PROVENANCE" ] || die "missing committed provenance: $COMMITTED_PROVENANCE"
[ -f "$ARCHIVE" ] || die \
    "no prebuilt archive at $ARCHIVE" \
    "Frontera cannot build it (zig needs statx, kernel 3.10 has none)." \
    "${REBUILD_HINT[@]}"

EXPECT_TARGET=$(jget "$COMMITTED_PROVENANCE" rust_target)
EXPECT_TREE=$(jget "$COMMITTED_PROVENANCE" vendor_tree_sha256)
EXPECT_ARCHIVE=$(jget "$COMMITTED_PROVENANCE" archive_sha256)
EXPECT_VERSION=$(jget "$COMMITTED_PROVENANCE" version_string)

[ -n "$EXPECT_ARCHIVE" ] || die \
    "provenance has no archive_sha256 (status=$(jget "$COMMITTED_PROVENANCE" status))." \
    "The archive has never been produced by CI, so there is nothing to verify against." \
    "${REBUILD_HINT[@]}"

# --- 2. target must match, or we are about to link a foreign ABI --------------------
# `just check` cross-compiles to x86_64-pc-windows-msvc; without this it would silently
# consume a linux-gnu archive and mislead you into thinking the leg passed.
ACTUAL_TARGET="${TARGET:-<unset>}"
[ "$ACTUAL_TARGET" = "$EXPECT_TARGET" ] || die \
    "target mismatch: cargo is building for '$ACTUAL_TARGET', archive is for '$EXPECT_TARGET'." \
    "This shim only serves $EXPECT_TARGET. Frontera is scoped to 'cargo build --release';" \
    "'just check' / 'just test' need a real zig and are not supported here."

# --- 3. THE gate: does the archive match the CURRENT vendored source? ---------------
ACTUAL_TREE=$(vendor_digest)
[ "$ACTUAL_TREE" = "$EXPECT_TREE" ] || die \
    "vendored libghostty-vt has changed since the archive was built." \
    "  expected tree $EXPECT_TREE" \
    "  actual   tree $ACTUAL_TREE" \
    "Refusing to link a stale archive: bindings.rs has 64 #[repr(C)] types and no layout" \
    "tests, so the symbol names would still resolve while the struct layouts would not." \
    "That corrupts memory at runtime instead of failing here." \
    "${REBUILD_HINT[@]}"

# --- 4. corruption check (not staleness -- step 3 owns that) ------------------------
ACTUAL_ARCHIVE=$(sha256sum "$ARCHIVE" | cut -d' ' -f1)
[ "$ACTUAL_ARCHIVE" = "$EXPECT_ARCHIVE" ] || die \
    "archive sha256 mismatch -- truncated or corrupted download." \
    "  expected $EXPECT_ARCHIVE" \
    "  actual   $ACTUAL_ARCHIVE" \
    "Re-run: scripts/frontera/fetch-artifact.sh"

# --- 5. independent staleness check: the archive embeds -Dversion-string ------------
if [ -n "$EXPECT_VERSION" ] && ! strings -a "$ARCHIVE" | grep -qF -- "$EXPECT_VERSION"; then
    die "archive does not embed version string '$EXPECT_VERSION'." \
        "It was built from different sources than provenance claims." \
        "${REBUILD_HINT[@]}"
fi

# --- 6. stage it where build.rs looks ------------------------------------------------
# vendor/libghostty-vt/zig-out/ is gitignored (vendor/libghostty-vt/.gitignore:12).
mkdir -p zig-out/lib
cp -f "$ARCHIVE" zig-out/lib/libghostty-vt.a

printf 'frontera zig-shim: verified + staged libghostty-vt.a (%s, tree %s)\n' \
    "$EXPECT_VERSION" "${ACTUAL_TREE:0:12}" >&2
exit 0
