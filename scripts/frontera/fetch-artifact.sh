#!/bin/bash
# Download the CI-built libghostty-vt.a onto /work2 and refuse to install it unless it
# matches what this branch expects.
#
#     scripts/frontera/fetch-artifact.sh [release-tag]
#
# Two provenance copies exist and they play different roles:
#   - scripts/frontera/provenance.json  the COMMITTED expectation (what this branch wants)
#   - the copy shipped beside the .a    what CI actually produced
# They must agree field-for-field. A CI rebuild the branch has not acknowledged is
# rejected here; a vendored-source change the artifact has not caught up to is rejected
# later, by zig-shim.sh, at build time.
set -euo pipefail
export LC_ALL=C

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
COMMITTED="$REPO_ROOT/scripts/frontera/provenance.json"
STAGE_DIR="${HERDR_FRONTERA_PREBUILT:-/work2/08526/jdgeorga/frontera/herdr-build/prebuilt}"
FORK=jdgeorga/herdr
TAG="${1:-frontera-libghostty-vt}"

die() { printf 'fetch-artifact: FATAL: %s\n' "$*" >&2; exit 1; }
jget() {
    /usr/bin/python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
v = d.get(sys.argv[2])
sys.stdout.write("" if v is None else str(v))
' "$1" "$2"
}

[ -f "$COMMITTED" ] || die "missing $COMMITTED"
command -v gh >/dev/null || die "gh not found; it is how we reach the fork's releases"

TMP=$(mktemp -d "${TMPDIR:-/tmp}/herdr-frontera-fetch.XXXXXX")
trap 'rm -rf "$TMP"' EXIT

echo "fetch-artifact: downloading '$TAG' from $FORK ..."
gh release download "$TAG" --repo "$FORK" --clobber --dir "$TMP" \
    --pattern 'libghostty-vt.a' \
    --pattern 'provenance.json' \
    --pattern 'libghostty-vt.undefined-symbols.txt' \
  || die "download failed. Has CI run yet? Actions must be enabled on the fork first:
  https://github.com/$FORK/actions   (one-time, needs a browser)
  Then: gh workflow list --repo $FORK   should list the workflows."

[ -f "$TMP/libghostty-vt.a" ] || die "release '$TAG' has no libghostty-vt.a asset"
[ -f "$TMP/provenance.json" ] || die "release '$TAG' has no provenance.json asset"

# --- the two provenance copies must agree --------------------------------------------
mismatch=0
for field in vendor_tree_sha256 archive_sha256 version_string source_commit \
             rust_target zig_target zig_version; do
    want=$(jget "$COMMITTED" "$field")
    got=$(jget "$TMP/provenance.json" "$field")
    if [ "$want" != "$got" ]; then
        printf '  %-20s committed=%s  ci=%s\n' "$field" "${want:-<null>}" "${got:-<null>}" >&2
        mismatch=1
    fi
done
if [ "$mismatch" -ne 0 ]; then
    die "committed provenance disagrees with what CI produced (fields above).
If CI is right, update scripts/frontera/provenance.json in a deliberate commit:
    cp $TMP/provenance.json $COMMITTED
and re-read the diff before committing. Never automate that step."
fi

# --- the archive must be the one provenance describes ---------------------------------
actual=$(sha256sum "$TMP/libghostty-vt.a" | cut -d' ' -f1)
want=$(jget "$COMMITTED" archive_sha256)
[ "$actual" = "$want" ] || die "archive sha256 mismatch
  expected $want
  actual   $actual"

mkdir -p "$STAGE_DIR"
install -m 0644 "$TMP/libghostty-vt.a" "$STAGE_DIR/libghostty-vt.a"
install -m 0644 "$TMP/provenance.json" "$STAGE_DIR/provenance.json"
[ -f "$TMP/libghostty-vt.undefined-symbols.txt" ] && \
    install -m 0644 "$TMP/libghostty-vt.undefined-symbols.txt" "$STAGE_DIR/"

echo "fetch-artifact: staged $(du -h "$STAGE_DIR/libghostty-vt.a" | cut -f1) -> $STAGE_DIR"
echo "fetch-artifact: next, run the link probe before a full build:"
echo "    scripts/frontera/link-probe.sh"
