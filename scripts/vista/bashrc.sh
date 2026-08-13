# herdr-slurm shell integration for TACC Vista.
#
# Install by symlink, so the repo copy stays the single source of truth:
#     ln -sf ~/herdr/scripts/vista/bashrc.sh ~/.config/herdr-slurm/bashrc.sh
#
# and source it from ~/.bashrc with exactly one line:
#     [ -r ~/.config/herdr-slurm/bashrc.sh ] && . ~/.config/herdr-slurm/bashrc.sh
#
# Two things live here. Both are defensive: with `site/install.sh --link-herdr`
# done, ~/.local/bin/herdr IS site/bin/herdr-slurm and already does the right
# thing on every invocation. These cover the paths that bypass it.

# ---------------------------------------------------------------------------
# 1. Node-local socket. THE load-bearing hazard on this cluster.
# ---------------------------------------------------------------------------
# herdr's data dir is ~/.config/herdr. On Vista that is /home1, which is NFS
# (192.168.16.21:/vista/home1) and shared by every login and compute node. So a
# server's unix socket placed there is VISIBLE from hosts where the server
# process does not exist.
#
# Connecting to it from such a host returns ECONNREFUSED, and
# src/ipc.rs::prepare_socket_path() classifies ConnectionRefused | NotFound |
# TimedOut as a stale socket: it deletes the file and starts a SECOND server.
# The original server keeps running with your panes inside it, now permanently
# unreachable. session.json, both logs and .plugins.lock would also then be
# written by two servers at once.
#
# Vista's /tmp is a genuinely node-local xfs volume on its own block device
# (/dev/mapper/rootvg01-lv_tmp), not a symlink into shared storage, so moving
# just the socket there removes the cross-host visibility entirely.
#
# The formula below is byte-identical to site/bin/herdr-slurm:113. Keep it that
# way: `herdr ls` discovers sessions by this exact path, so a divergence hides
# sessions instead of raising an error.
#
# Only the SOCKET moves. Never the data dir -- see the guard in section 2.
herdr() {
    local rt="${TMPDIR:-/tmp}/herdr-slurm-${USER}-$(hostname -s)"

    # `command` is required: without it this function calls itself forever.
    #
    # Note what is deliberately NOT done here: the environment is not scrubbed.
    # HERDR_PANE_ID is exported into every pane by the server, and herdr's CLI
    # subcommands use it to reach the server that owns the pane they are run in.
    # It passes through untouched because this is a plain prefix assignment
    # rather than `env -i`; using `env -i` here would silently break every
    # `herdr agent`/`herdr pane` call made from inside a pane. The same applies
    # to an already-set HERDR_SOCKET_PATH, which is why both use :- defaults.
    HERDR_SOCKET_PATH="${HERDR_SOCKET_PATH:-$rt/herdr.sock}" \
    HERDR_CLIENT_SOCKET_PATH="${HERDR_CLIENT_SOCKET_PATH:-$rt/herdr-client.sock}" \
        command herdr "$@"
}

# ---------------------------------------------------------------------------
# 2. XDG_CONFIG_HOME repair guard.
# ---------------------------------------------------------------------------
# Relocating herdr's data dir with XDG_CONFIG_HOME was tried on Frontera and
# backfired badly. The variable is not herdr-specific, and herdr exports its
# whole environment into every pane, so a server started with XDG_CONFIG_HOME
# hands that value to every process in every pane. gh, gcloud, yazi and
# matplotlib then look for their config inside herdr's state dir and find
# nothing -- on Frontera this made `gh auth status` report "not logged into any
# GitHub hosts" with ~/.config/gh/hosts.yml perfectly intact.
#
# This repairs a pane that inherited such a value, without restarting the server
# and killing live work. It only fires when the value actually points into a
# herdr state dir, so a legitimate XDG_CONFIG_HOME is left alone.
case "${XDG_CONFIG_HOME:-}" in
    */herdr|*/herdr/*|*/herdr-slurm|*/herdr-slurm/*)
        echo "herdr-slurm: unsetting inherited XDG_CONFIG_HOME=$XDG_CONFIG_HOME" >&2
        echo "  (it pointed into a herdr state dir; see ~/herdr/scripts/vista/README.md)" >&2
        unset XDG_CONFIG_HOME
        ;;
esac
