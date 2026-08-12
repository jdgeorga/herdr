#!/bin/bash
# Go/no-go gate for the prebuilt libghostty-vt.a. Run this BEFORE a 600-crate cargo build.
#
#     scripts/frontera/link-probe.sh
#
# One ~30-second run simultaneously falsifies four independent assumptions:
#   1. no undefined C++ runtime / glibc-too-new symbols   (it links at all)
#   2. no PIC-vs-PIE relocation problem                   (it links as -pie)
#   3. binutils 2.27 can read an llvm-ar MRI fat archive  (ar/nm read the index)
#   4. nothing calls a post-3.10 syscall                  (it runs without ENOSYS)
#
# Staleness is NOT checked here -- that is the vendored-source digest gate in zig-shim.sh.
# An earlier version of this script compared the reported version against vendor VERSION,
# which was wrong: the library reports config.lib_version ("0.1.0-dev" by default), not
# the -Dversion-string value, and the vendored VERSION never appears in the archive.
#
# It runs the link twice, with XALT tracking on and off, so XALT interference is isolated
# here rather than discovered halfway through a cargo build.
set -euo pipefail
export LC_ALL=C

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
STAGE_DIR="${HERDR_FRONTERA_PREBUILT:-/work2/08526/jdgeorga/frontera/herdr-build/prebuilt}"
ARCHIVE="$STAGE_DIR/libghostty-vt.a"
INCLUDE="$REPO_ROOT/vendor/libghostty-vt/include"
CC_BIN=${CC:-/opt/apps/gcc/8.3.0/bin/gcc}

fail=0
note() { printf '\n== %s\n' "$*"; }
bad()  { printf '   FAIL: %s\n' "$*"; fail=1; }
good() { printf '   ok: %s\n' "$*"; }

[ -f "$ARCHIVE" ] || { echo "link-probe: no archive at $ARCHIVE; run fetch-artifact.sh" >&2; exit 1; }

note "archive readable by Frontera's OWN binutils (system ar/nm, not gcc-8.3's)"
if /usr/bin/ar t "$ARCHIVE" >/dev/null 2>&1; then good "/usr/bin/ar t"; else bad "/usr/bin/ar cannot list members"; fi
if /usr/bin/nm --print-armap "$ARCHIVE" >/dev/null 2>&1; then good "/usr/bin/nm --print-armap"; else bad "/usr/bin/nm cannot read the symbol index"; fi
printf '   members: %s\n' "$(/usr/bin/ar t "$ARCHIVE" 2>/dev/null | wc -l)"

note "no EXTERNAL C++ runtime dependency"
# Subtract the archive's own defined symbols first: `nm --undefined-only` on an archive
# reports per-member undefineds, so simdutf's and highway's own C++ functions appear
# undefined even though a sibling member defines them. Only what survives is external.
undef=$(/usr/bin/nm --undefined-only --format=posix "$ARCHIVE" 2>/dev/null | awk '{print $1}' | sort -u)
defined=$(/usr/bin/nm --defined-only --format=posix "$ARCHIVE" 2>/dev/null | awk '{print $1}' | sort -u)
external=$(comm -23 <(echo "$undef") <(echo "$defined"))
cxx=$(printf '%s\n' "$external" | grep -E '^(_Z|__cxa_|__gxx_personality|_Unwind_)' || true)
# deliberate word split below: one symbol per line
# shellcheck disable=SC2086
if [ -z "$cxx" ]; then good "none"; else bad "found:"; printf '     %s\n' $cxx; fi

note "external symbols all resolvable against Frontera's glibc 2.17 (libc/m/pthread/dl/rt)"
undef="$external"
defined=$( { /usr/bin/nm -D --defined-only /usr/lib64/libc.so.6 2>/dev/null
             /usr/bin/nm -D --defined-only /usr/lib64/libm.so.6 2>/dev/null
             /usr/bin/nm -D --defined-only /usr/lib64/libpthread.so.0 2>/dev/null
             /usr/bin/nm -D --defined-only /usr/lib64/libdl.so.2 2>/dev/null
             /usr/bin/nm -D --defined-only /usr/lib64/librt.so.1 2>/dev/null
           } | awk '{print $3}' | sed 's/@@.*//;s/@.*//' | sort -u)
missing=$(comm -23 <(echo "$undef") <(echo "$defined") | grep -v '^$' || true)
if [ -z "$missing" ]; then
    good "every external symbol is present in glibc 2.17"
else
    printf '   unresolved-by-libc (may be intra-archive, the link below is authoritative):\n'
    # deliberate word split below: one symbol per line
    # shellcheck disable=SC2086
    printf '     %s\n' $missing | head -20
fi

cat > "${TMPDIR:-/tmp}/herdr_link_probe.c" <<'EOF'
#include <stdio.h>
#include <stdint.h>
#include <stddef.h>
#include "ghostty/vt.h"

int main(void) {
    GhosttyString v = {0};
    if (ghostty_build_info(GHOSTTY_BUILD_INFO_VERSION_STRING, &v) != GHOSTTY_SUCCESS) {
        fprintf(stderr, "ghostty_build_info failed\n");
        return 2;
    }
    printf("%.*s\n", (int)v.len, (const char *)v.ptr);
    return 0;
}
EOF

for tracking in yes no; do
    note "link + run with XALT_EXECUTABLE_TRACKING=$tracking"
    out="${TMPDIR:-/tmp}/herdr_link_probe.$tracking"
    # -lrt is REQUIRED on this host: the kitty graphics code calls shm_open/shm_unlink,
    # which live in librt on glibc 2.17. They only moved into libc proper in glibc 2.34,
    # so upstream's modern toolchains never need to ask for it.
    if XALT_EXECUTABLE_TRACKING=$tracking "$CC_BIN" -fPIE -pie -O1 \
            -I "$INCLUDE" "${TMPDIR:-/tmp}/herdr_link_probe.c" "$ARCHIVE" \
            -lm -lpthread -ldl -lrt -o "$out" 2>"${TMPDIR:-/tmp}/herdr_link_probe.$tracking.err"; then
        good "linked"
        if got=$(XALT_EXECUTABLE_TRACKING=$tracking "$out" 2>&1); then
            # Do NOT compare this against vendor/libghostty-vt/VERSION. This reports
            # config.lib_version (-Dlib-version-string, default "0.1.0-dev"), not
            # -Dversion-string. Staleness is the digest gate's job, not this probe's.
            good "ran, reported lib version '$got'"
        else
            bad "linked but crashed at runtime: $got"
            echo "     (if this is ENOSYS, the library touches a syscall kernel 3.10 lacks)"
        fi
    else
        bad "link failed; stderr:"
        sed 's/^/     /' "${TMPDIR:-/tmp}/herdr_link_probe.$tracking.err" | head -25
    fi
done

printf '\n'
if [ "$fail" -eq 0 ]; then
    echo "link-probe: PASS -- safe to run a full cargo build."
else
    echo "link-probe: FAIL -- do not start a full build until the above is resolved."
fi
exit "$fail"
