#!/bin/bash
# Report the limits that actually bite on Frontera. Complement to `herdr health`.
#
#     scripts/frontera/frontera-limits.sh [host ...]     # default: this host + login1-4
#
# WHY THIS EXISTS
#   `herdr health` reads cgroup memory/pids. Frontera is cgroup v1 and the per-user slice
#   is not readable, so site/lib/herdr-site.sh warns and the MEMORY/TASKS columns come back
#   zeroed -- it reported "MEMORY 0K / LIMIT unlimited / TASKS 0 / TASKLIMIT max" for a
#   login node that was actually at 166 of 300 threads. A health check that reads "fine"
#   at the ceiling is worse than none.
#
#   Frontera enforces through ulimit instead:
#     ulimit -u  300 on login nodes, 4096 on compute nodes -- and it counts THREADS,
#                not processes. Overrunning it produces misleading failures: rustc
#                WouldBlock, EAGAIN from fork, servers killed with nothing logged.
#     ulimit -v  8 GB per process on login nodes, unlimited on compute nodes. A single
#                `claude` sits around 5 GB of VSZ, i.e. two thirds of the cap.
set -euo pipefail
export LC_ALL=C

hosts=("$@")
if [ ${#hosts[@]} -eq 0 ]; then
    hosts=("$(hostname -s)" login1 login2 login3 login4)
fi

# The probe body is evaluated on the REMOTE host, so it must stay unexpanded here.
# shellcheck disable=SC2016
probe='
  u=$(ulimit -u 2>/dev/null || echo "?")
  v=$(ulimit -v 2>/dev/null || echo unlimited)
  t=$(ps -Lu "$USER" --no-headers 2>/dev/null | wc -l)
  if [ "$v" = unlimited ]; then vg=unlimited; else vg=$(( v / 1048576 ))GB; fi
  # biggest single process by VSZ -- the one that will hit ulimit -v first
  big=$(ps -u "$USER" -o vsz=,comm= --no-headers 2>/dev/null | sort -rn | head -1)
  bv=$(echo "$big" | awk "{printf \"%.1f\", \$1/1048576}")
  bc=$(echo "$big" | awk "{print \$2}")
  h=$(pgrep -u "$USER" -x herdr 2>/dev/null | wc -l)
  printf "%-11s %6s/%-6s %8s %8sGB %-14s %5s\n" "$(hostname -s)" "$t" "$u" "$vg" "${bv:-0}" "${bc:-none}" "$h"
'

printf '%-11s %13s %8s %10s %-14s %5s\n' HOST THREADS/CAP VMEMCAP BIGGEST '' HERDR
for h in "${hosts[@]}"; do
    if [ "$h" = "$(hostname -s)" ]; then
        bash -c "$probe"
    else
        timeout 25 ssh -o BatchMode=yes -o ConnectTimeout=5 "$h" "$probe" 2>/dev/null \
            || printf '%-11s %s\n' "$h" "(unreachable)"
    fi
done

cat <<'EOF'

  THREADS/CAP is `ps -Lu $USER | wc -l` against `ulimit -u`. The cap counts threads, so
  ssh (~33 each) and tailscaled add up fast. Past it: EAGAIN, WouldBlock, silent kills.
  BIGGEST is the largest single process by VSZ against the per-process VMEMCAP.
  Run herdr on a compute node: 4096 threads and no vmem cap, versus 300 and 8GB.
EOF
