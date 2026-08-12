#!/bin/bash
# herdr-attach -- get from a login node to herdr on your running idev job's compute node.
#
# That is its ONLY job: resolve which node the job is on, ssh there, and run `herdr`.
# Everything about how herdr itself runs -- socket path, session name, config file --
# belongs to site/bin/herdr-slurm, which `~/.local/bin/herdr` points at after
# `site/install.sh --link-herdr`. This script deliberately sets none of it, so the two
# can never disagree.
#
# Why the server lives on the compute node: frontera.tacc.utexas.edu round-robins across
# login1-4, so a server on "the login node" is a 3-in-4 chance of landing where your
# session isn't. The compute node is reachable from every login node for the life of the
# job, which makes the login node pure transit.
#
# Caveat inherited from idev-attach: a server born from THIS ssh gets a bare login
# environment (measured: 0 SLURM_* vars, versus 41 in a server started from inside the
# job step), so its panes cannot run ibrun/srun. For job-aware panes, start herdr from
# inside the job shell instead. We warn rather than refuse -- a plain shell is still useful.
set -u

DRY=0
JOBID=""
HERDR_BIN="${HERDR_BIN:-$HOME/.local/bin/herdr}"

usage() {
    cat <<'EOF'
usage: herdr-attach [-n] [-j JOBID] [-- ARGS...]

  -n, --dry-run   print the resolved node and command, do not connect
  -j, --job ID    use this job instead of auto-picking
  -- ARGS...      passed through to herdr on the compute node

Auto-pick order: the only RUNNING job; else the one named idv*; else it lists the
candidates and exits so you can pass -j. (Same selection as idev-attach/job-node.)

Socket, session and config are owned by site/bin/herdr-slurm on the far side.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        -n|--dry-run) DRY=1; shift ;;
        -j|--job)     JOBID="${2:-}"; shift 2 ;;
        -h|--help)    usage; exit 0 ;;
        --)           shift; break ;;
        -*)           echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
        *)            break ;;
    esac
done

mapfile -t JOBS < <(squeue -u "$USER" -h -t RUNNING -o '%i|%N|%j|%L')
if [ "${#JOBS[@]}" -eq 0 ]; then
    echo "no RUNNING job to attach to. start one first, e.g.:" >&2
    echo "  idev -N 1 -n 56 -p normal -t 12:00:00" >&2
    exit 1
fi

pick=""
if [ -n "$JOBID" ]; then
    for j in "${JOBS[@]}"; do
        [ "${j%%|*}" = "$JOBID" ] && pick="$j"
    done
    [ -n "$pick" ] || { echo "job $JOBID is not RUNNING for $USER" >&2; exit 1; }
elif [ "${#JOBS[@]}" -eq 1 ]; then
    pick="${JOBS[0]}"
else
    for j in "${JOBS[@]}"; do
        IFS='|' read -r _ _ jname _ <<<"$j"
        case "$jname" in idv*) [ -z "$pick" ] && pick="$j" ;; esac
    done
    if [ -z "$pick" ]; then
        echo "several RUNNING jobs -- pick one with -j JOBID:" >&2
        printf '%s\n' "${JOBS[@]}" | column -t -s'|' >&2
        exit 1
    fi
fi

IFS='|' read -r jobid nodelist jname left <<<"$pick"
hosts="$(scontrol show hostnames "$nodelist")" || exit 1
node="$(head -1 <<<"$hosts")"
nnodes="$(wc -l <<<"$hosts")"

printf 'job %s (%s): %s, %s left' "$jobid" "$jname" "$node" "$left"
[ "$nnodes" -gt 1 ] && printf ' [+%d more: %s]' "$((nnodes - 1))" "$nodelist"
printf '\n'

# The script is passed as the ssh COMMAND ARGUMENT, never on stdin. With `ssh -t`, stdin is
# the terminal the TUI needs; feeding the script through stdin would have the remote bash
# eat it and leave herdr with no input. Args are baked in via `set --` with %q quoting for
# the same reason.
#
# Absolute path rather than `ssh -t node bash -lc`: sourcing .bashrc on a compute node
# fires start-tailscaled.sh, whose owner lockfile then refuses and prints noise on every
# reconnect. The heredoc is QUOTED so $@ and $(hostname) resolve on the REMOTE side.
#
# The cold-start warning keys off whether a herdr server is already running on that host,
# not off a socket path -- the path is the launcher's business, and duplicating it here is
# exactly how the two drift apart.
remote="$(cat <<'EOF'
if [ ! -x "__BIN__" ]; then
    echo "herdr-attach: no herdr at __BIN__ on $(hostname -s)" >&2
    echo "  build it:  cd ~/herdr && source scripts/frontera/env.sh && cargo build --release" >&2
    echo "  install:   ~/herdr/site/install.sh --link-herdr" >&2
    exit 127
fi
if ! pgrep -u "$USER" -x herdr >/dev/null 2>&1; then
    echo "herdr-attach: starting a new server cold over ssh -- its panes will have NO" >&2
    echo "  SLURM_* env, so ibrun/srun/mpirun will not work in them. For job-aware panes," >&2
    echo "  run herdr from inside the job shell instead." >&2
    sleep 3
fi
exec "__BIN__" "$@"
EOF
)"
remote="${remote//__BIN__/$HERDR_BIN}"
# Bake the passthrough args in, so stdin stays free for the tty.
#
# ALWAYS emit `set --`, even with no args. bash sources ~/.bashrc for ssh commands, and a
# .bashrc that runs `set` with operands leaves positional parameters behind for us to
# inherit -- this one has `set umask 027` at line 107, which does not set the umask at all
# (that would be plain `umask 027`); it sets $1=umask $2=027. Those then reached
# `exec herdr "$@"` and herdr died with `unknown command: umask`. Clearing the positionals
# unconditionally makes this immune to whatever any future rc file leaves lying around.
#
# The `[ "$#" -gt 0 ] &&` guard matters: `printf '%q ' ` with zero arguments prints `''`,
# so an unconditional expansion would emit `set -- ''` and hand herdr one empty argument.
remote="set -- $([ "$#" -gt 0 ] && printf '%q ' "$@")
$remote"

if [ "$DRY" -eq 1 ]; then
    echo "socket/session/config: owned by site/bin/herdr-slurm on $node"
    if [ "$(hostname -s)" = "$node" ]; then
        echo "would run locally: $HERDR_BIN $*"
    else
        echo "would run: ssh -t $node '<script below, as the command argument>'"
        printf '%s\n' "$remote" | sed 's/^/  /'
    fi
    exit 0
fi

if [ -n "${TMUX:-}" ]; then
    echo "note: \$TMUX is set -- herdr is itself a multiplexer, so this nests one inside" >&2
    echo "      another and the prefix keys will collide. Consider detaching first." >&2
fi

if [ "$(hostname -s)" = "$node" ]; then
    exec "$HERDR_BIN" "$@"
fi

exec ssh -t "$node" "$remote"
