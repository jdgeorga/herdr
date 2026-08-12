#!/bin/bash
# herdr-attach -- attach to the herdr server on your running job's compute node,
# from any Frontera login node. The herdr equivalent of idev-attach.
#
# herdr is itself a terminal workspace manager, so this REPLACES tmux rather than
# running inside it. Its server persists on the compute node for as long as you hold
# the allocation, which is the same durability tmux gave you. Nesting herdr inside
# tmux works but gives you two multiplexers competing for the same keys.
#
# Why the server lives on the compute node: frontera.tacc.utexas.edu round-robins
# across login1-4, so a server on "the login node" is a 3-in-4 chance of landing
# where your session isn't. The compute node is reachable from every login node for
# the life of the job, which makes the login node pure transit.
#
# ALL MUTABLE STATE MUST BE NODE-LOCAL. herdr's data dir defaults to ~/.config/herdr,
# and /home1 is Lustre -- shared across every login and compute node. Two consequences,
# the first of which is destructive:
#
#   1. The socket file is visible from hosts where the server process does not exist.
#      Connecting to it from such a host returns ECONNREFUSED, and herdr's
#      prepare_socket_path() treats ECONNREFUSED as "stale" and DELETES the file. So a
#      herdr started on a login node removes the live compute-node server's socket and
#      starts a second one, orphaning the first server's panes. Verified 2026-08-11 by
#      connecting to a Lustre-hosted socket from login1 and login2.
#   2. session.json, both logs, and .plugins.lock would be written by both servers.
#
# ONLY the socket is relocated. Everything else stays at herdr's default ~/.config/herdr,
# which lives on /home1 and is therefore already durable: session.json records each pane's
# cwd and agent_session id, and herdr restores from it at startup ([session]
# resume_agents_on_restore, default true). Verified 2026-08-12: killed the server, deleted
# the sockets, restarted -> persist.restore outcome="ok" with the pane cwd intact. So a
# layout, and resumable Claude sessions, survive the job ending and return on a new node.
#
# DO NOT relocate the data dir with XDG_CONFIG_HOME. That variable is not herdr-specific,
# and herdr exports its whole environment into every pane -- so an XDG_CONFIG_HOME set for
# the server silently redirects gh, gcloud, yazi and matplotlib inside every pane. It broke
# `gh auth status` in a live session on 2026-08-12. The socket variable is targeted and
# safe; XDG_CONFIG_HOME is not.
#
# Residual, accepted: two servers on different nodes share one ~/.config/herdr/session.json,
# so the last to save wins the layout. That is an annoyance, not corruption -- the
# destructive shared-socket case is what the node-local socket fixes. herdr has workspaces
# and tabs, so prefer ONE server with several workspaces over several servers.
#
# Socket, session and config are left to site/bin/herdr-slurm, which ~/.local/bin/herdr
# points at after `site/install.sh --link-herdr`. It sets HERDR_SOCKET_PATH to a per-host
# /tmp dir, HERDR_SESSION to slurm-<host> (durable per-host state under
# ~/.config/herdr/sessions/), and HERDR_CONFIG_PATH to ~/.config/herdr-slurm/config.toml.
# Forcing our own socket here would diverge from that and hide the session from `herdr ls`.
#
# Note the --session FLAG is still the thing to avoid: src/session.rs:80 sets
# EXPLICIT_SESSION_REQUESTED only for the flag, and active_api_socket_path() checks it
# before the environment, which would drag the socket back onto Lustre. The HERDR_SESSION
# env var the launcher uses is explicitly safe -- line 82 checks HERDR_SOCKET_PATH first
# and stores false.
#
# Caveat inherited from idev-attach: a server born from THIS ssh gets a bare login
# environment (measured: 0 SLURM_* vars, versus 41 in a server started from inside
# the job step), so its panes cannot run ibrun/srun. For job-aware panes, start herdr
# from inside the job shell instead. We warn rather than refuse -- a plain shell is
# still useful.
set -u

NAME=agents
DRY=0
JOBID=""
HERDR_BIN="${HERDR_BIN:-$HOME/.local/bin/herdr}"

usage() {
    cat <<'EOF'
usage: herdr-attach [-n] [-j JOBID] [NAME]

  -n, --dry-run   print the resolved node and command, do not connect
  -j, --job ID    use this job instead of auto-picking
  NAME            herdr instance name (default: agents); selects the state dir

Auto-pick order: the only RUNNING job; else the one named idv*; else it lists the
candidates and exits so you can pass -j. (Same selection as idev-attach/job-node.)
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        -n|--dry-run) DRY=1; shift ;;
        -j|--job)     JOBID="${2:-}"; shift 2 ;;
        -h|--help)    usage; exit 0 ;;
        -*)           echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
        *)            NAME="$1"; shift ;;
    esac
done

# Interpolated into a remote shell command and into a filesystem path. Keep it boring.
case "$NAME" in
    ""|*[!A-Za-z0-9._-]*)
        echo "name must match [A-Za-z0-9._-]+ (got '$NAME')" >&2
        exit 2 ;;
esac

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

# Absolute path rather than `ssh -t node bash -lc`: sourcing .bashrc on a compute node
# fires start-tailscaled.sh, whose owner lockfile then refuses and prints noise on every
# reconnect. The heredoc is QUOTED so $HOME/$USER are expanded by the REMOTE shell.
remote="$(cat <<'EOF'
if [ ! -x "__BIN__" ]; then
    echo "herdr-attach: no herdr at __BIN__ on $(hostname -s)" >&2
    echo "  build it first:  cd ~/herdr && source scripts/frontera/env.sh && cargo build --release" >&2
    exit 127
fi
if [ ! -S "${TMPDIR:-/tmp}/herdr-slurm-$USER-$(hostname -s)/herdr.sock" ]; then
    echo "herdr-attach: starting a new server cold over ssh -- its panes will have NO" >&2
    echo "  SLURM_* env, so ibrun/srun/mpirun will not work in them. For job-aware panes," >&2
    echo "  run herdr from inside the job shell instead." >&2
    sleep 3
fi
exec "__BIN__"
EOF
)"
remote="${remote//__BIN__/$HERDR_BIN}"

if [ "$DRY" -eq 1 ]; then
    echo "socket/session/config: delegated to site/bin/herdr-slurm on the target host"
    if [ "$(hostname -s)" = "$node" ]; then
        echo "would run locally: $HERDR_BIN"
    else
        echo "would run: ssh -t $node <<'---'"
        printf '%s\n' "$remote" | sed 's/^/  /'
        echo "---"
    fi
    exit 0
fi

if [ -n "${TMUX:-}" ]; then
    echo "note: \$TMUX is set -- herdr is itself a multiplexer, so this nests one inside" >&2
    echo "      another and the prefix keys will collide. Consider detaching first." >&2
fi

if [ "$(hostname -s)" = "$node" ]; then
    exec "$HERDR_BIN"
fi

exec ssh -t "$node" "$remote"
