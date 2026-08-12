#!/bin/bash
# Go/no-go gate for the prebuilt libghostty-vt.a. Run this BEFORE a 600-crate cargo build.
#
#     scripts/frontera/link-probe.sh
#
# One ~30-second run simultaneously falsifies four independent assumptions:
#   1. no undefined C++ runtime / glibc-too-new symbols   (it links at all)
#   2. no PIC-vs-PIE relocation problem                   (it links as -pie)
#   3. binutils 2.27 can read an llvm-ar MRI fat archive  (ar/nm read the index)
#   4. the archive is not stale                           (it prints its embedded version,
#                                                          which must equal vendor VERSION)
#
# It runs the link twice, with XALT tracking on and off, so XALT interference is isolated
# here rather than discovered halfway through a cargo build.
set -euo pipefail
export LC_ALL=C

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
STAGE_DIR="${HERDR_FRONTERA_PREBUILT:-/work2/08526/jdgeorga/frontera/herdr-build/prebuilt}"
ARCHIVE="$STAGE_DIR/libghostty-vt.a"
INCLUDE="$REPO_ROOT/vendor/libghostty-vt/include"
EXPECT_VERSION=$(cat "$REPO_ROOT/vendor/libghostty-vt/VERSION")
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

note "no C++ runtime symbols left undefined"
cxx=$(/usr/bin/nm --undefined-only --format=posix "$ARCHIVE" 2>/dev/null \
      | awk '{print $1}' \
      | grep -E '^(_Z|__cxa_|__gxx_personality|_Unwind_|_Znwm|_ZdlPv)' | sort -u || true)
if [ -z "$cxx" ]; then good "none"; else bad "found:"; printf '     %s\n' $cxx; fi

note "undefined glibc symbols all resolvable against Frontera's glibc 2.17"
undef=$(/usr/bin/nm --undefined-only --format=posix "$ARCHIVE" 2>/dev/null | awk '{print $1}' | sort -u)
defined=$( { /usr/bin/nm -D --defined-only /usr/lib64/libc.so.6 2>/dev/null
             /usr/bin/nm -D --defined-only /usr/lib64/libm.so.6 2>/dev/null
             /usr/bin/nm -D --defined-only /usr/lib64/libpthread.so.0 2>/dev/null
             /usr/bin/nm -D --defined-only /usr/lib64/libdl.so.2 2>/dev/null
           } | awk '{print $3}' | sed 's/@@.*//;s/@.*//' | sort -u)
missing=$(comm -23 <(echo "$undef") <(echo "$defined") | grep -v '^$' || true)
if [ -z "$missing" ]; then
    good "every external symbol is present in glibc 2.17"
else
    printf '   unresolved-by-libc (may be intra-archive, the link below is authoritative):\n'
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
    if XALT_EXECUTABLE_TRACKING=$tracking "$CC_BIN" -fPIE -pie -O1 \
            -I "$INCLUDE" "${TMPDIR:-/tmp}/herdr_link_probe.c" "$ARCHIVE" \
            -lm -lpthread -ldl -o "$out" 2>"${TMPDIR:-/tmp}/herdr_link_probe.$tracking.err"; then
        good "linked"
        if got=$(XALT_EXECUTABLE_TRACKING=$tracking "$out" 2>&1); then
            if [ "$got" = "$EXPECT_VERSION" ]; then
                good "ran, reported '$got' (matches vendor VERSION)"
            else
                bad "version mismatch: archive says '$got', vendor VERSION says '$EXPECT_VERSION' -- STALE ARCHIVE"
            fi
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
