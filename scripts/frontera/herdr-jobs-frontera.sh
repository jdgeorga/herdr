#!/bin/bash
# Interpreter wrapper for the SLURM sidebar provider on Frontera.
#
# herdr's [ui.sidebar.list] command points at THIS script, not at python directly:
#     command = ["<repo>/scripts/frontera/herdr-jobs-frontera.sh", "--mode", "{mode}"]
#
# Two Frontera facts make the wrapper necessary:
#
#   1. INTERPRETER. scripts/herdr-jobs.py needs Python >= 3.7 (it opens with
#      `from __future__ import annotations`). Frontera's /usr/bin/python3 is 3.6.8 and
#      fails to even parse it. The python3/3.9.2 module works -- verified: both --mode
#      live and --mode history produce correct JSON against Frontera's Slurm 23.11, and
#      `--selftest` passes.
#
#   2. libssp SHADOW. Juliaup ships its own libssp.so.0 and .bashrc puts that directory
#      first on LD_LIBRARY_PATH, but it lacks __vsnprintf_chk@LIBSSP_1.0. Every Intel-built
#      python then dies with a relocation error. herdr inherits the login environment, so
#      the sidebar would hit this too. gcc 8.3.0's lib64 has the working copy, and it must
#      come FIRST. (Do not clobber LD_LIBRARY_PATH outright -- the Intel runtime's libimf
#      is also needed.)
#
# The script itself is NOT modified. It was written for NERSC and runs here unchanged.
set -euo pipefail

PY_PREFIX=/opt/apps/intel19/python3/3.9.2
GCC_LIB=/opt/apps/gcc/8.3.0/lib64
INTEL_LIB=/opt/intel/compilers_and_libraries_2020.1.217/linux/compiler/lib/intel64_lin
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
PROVIDER="$REPO_ROOT/scripts/herdr-jobs.py"

# Contract: stdout carries JSON and nothing else, ever -- even when we fail. A non-zero
# exit tells herdr "couldn't ask" (retain + stale the previous rows) rather than "no jobs".
bail() {
    printf '{"version":1,"title":"JOBS","summary":"provider error","groups":[],"notify":[{"style":"fail","text":"%s"}]}\n' "$1"
    exit 1
}

[ -x "$PY_PREFIX/bin/python3" ] || bail "python3/3.9.2 missing at $PY_PREFIX"

# This python is Intel-built and needs libimf/libintlc from the Intel compiler runtime as
# well as its own lib dir. Name all three explicitly rather than inheriting them, so the
# wrapper does not depend on which modules happen to be loaded in herdr's environment.
export LD_LIBRARY_PATH="$GCC_LIB:$PY_PREFIX/lib:$INTEL_LIB:${LD_LIBRARY_PATH:-}"

# Preflight: a dynamic-loader failure would otherwise write to stderr and leave stdout
# empty, breaking the JSON-always contract. Cheap enough at a 10s poll interval.
"$PY_PREFIX/bin/python3" -I -S -c pass 2>/dev/null \
    || bail "python3/3.9.2 present but will not start (check LD_LIBRARY_PATH / libssp shadow)"

# -I -S are REQUIRED by the provider's invocation contract: nothing may print to stdout
# ahead of main() (sitecustomize, usercustomize, PYTHONSTARTUP). The script's own shebang
# is not consulted, because we exec the interpreter rather than the file.
exec "$PY_PREFIX/bin/python3" -I -S "$PROVIDER" "$@"
