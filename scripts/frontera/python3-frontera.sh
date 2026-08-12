#!/bin/bash
# Frontera interpreter for the SLURM jobs provider. Point HERDR_SITE_PYTHON here.
#
# site/lib/herdr-site.sh probes for a python >= 3.11 and finds none on Frontera: the
# newest module is 3.9.2 and /usr/bin/python3 is 3.6.8. The >= 3.11 gate is stricter
# than the provider actually needs -- scripts/herdr-jobs.py runs correctly on 3.9.2,
# verified 2026-08-12 in both --mode live and --mode history against Slurm 23.11, and
# `--selftest` passes. 3.6.8 genuinely cannot run it: it fails to parse
# `from __future__ import annotations`. So 3.9.2 is the floor here, not 3.11.
#
# A bare path to the interpreter is not enough, for two reasons:
#
#   1. The module python is Intel-built and needs its OWN lib dir plus the Intel
#      compiler runtime (libimf, libintlc) on LD_LIBRARY_PATH. Invoked by absolute
#      path from an environment that has not loaded the module, it dies with
#      "error while loading shared libraries: libpython3.9.so.1.0".
#   2. Juliaup ships its own libssp.so.0 at ~/.julia/juliaup/*/lib/julia/, which lacks
#      __vsnprintf_chk@LIBSSP_1.0. When that directory precedes gcc's on
#      LD_LIBRARY_PATH, python dies with a relocation error. /usr/lib64 has no
#      libssp.so.0 at all, so gcc 8.3.0's copy is the only working one and must win.
#
# Naming all three explicitly makes this independent of which modules happen to be
# loaded in the environment herdr spawns the provider from.
set -euo pipefail

PY_PREFIX=/opt/apps/intel19/python3/3.9.2
GCC_LIB=/opt/apps/gcc/8.3.0/lib64
INTEL_LIB=/opt/intel/compilers_and_libraries_2020.1.217/linux/compiler/lib/intel64_lin

if [ ! -x "$PY_PREFIX/bin/python3" ]; then
    echo "python3-frontera: no interpreter at $PY_PREFIX/bin/python3" >&2
    echo "  TACC may have moved the module; check: module spider python3" >&2
    exit 127
fi

export LD_LIBRARY_PATH="$GCC_LIB:$PY_PREFIX/lib:$INTEL_LIB:${LD_LIBRARY_PATH:-}"

exec "$PY_PREFIX/bin/python3" "$@"
