#!/bin/bash
#SBATCH -J herdr-build
#SBATCH -o /work2/08526/jdgeorga/frontera/herdr-build/build.%j.log
#SBATCH -e /work2/08526/jdgeorga/frontera/herdr-build/build.%j.log
#SBATCH -p normal
#SBATCH -N 1
#SBATCH -n 1
#SBATCH -t 03:00:00
#
# Build herdr on a Frontera compute node.
#
#     sbatch scripts/frontera/build.slurm
#
# Queue choice: `normal`, deliberately not `development`. No full release build of this
# tree has ever completed on this host, so the wall time is unmeasured, and the dev
# queue's QOS MaxWall is exactly 2h. CARGO_TARGET_DIR on /work2 is persistent, so a
# re-submit resumes rather than restarting.
#
# Login nodes are not an option: `ulimit -u` there is 300 and counts THREADS, so a
# parallel rustc run trips it and produces misleading errors (WouldBlock, EAGAIN).
set -euo pipefail

REPO_ROOT="${HERDR_REPO:-$HOME/herdr}"
cd "$REPO_ROOT"

source scripts/frontera/env.sh

echo "=== node: $(hostname)  cores: $(nproc --all)  ulimit -u: $(ulimit -u)"
echo "=== rust: $(rustc --version)"
echo "=== ZIG shim: $ZIG"

# Verify the prebuilt archive before burning an hour of compute on a build that cannot link.
if ! scripts/frontera/link-probe.sh; then
    echo "!!! link probe failed -- aborting before the full build" >&2
    exit 1
fi

cargo build --release --locked -j 24

BIN="$CARGO_TARGET_DIR/release/herdr"
echo "=== built: $BIN"
"$BIN" --version
