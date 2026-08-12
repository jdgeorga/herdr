#!/usr/bin/env python3
"""List (or kill) herdr-spinner daemons on THIS node.

Run over ssh across login nodes to find orphans. Lives in ~/.config, which is
shared storage, so every login node can run the same copy. `list` separates
the node's one legitimate daemon (per the launcher's pid file) from orphans;
`kill` SIGTERMs only the orphans, never the pid-file daemon.

Usage: orphan-scan.py [list|kill]

Identifies daemons by /proc/<pid>/cmdline structure — argv[0] is a python and
one of the later argv entries (skipping interpreter flags like `-u`) is the
daemon script — not by a substring match. A substring match also hits the
scanning shell itself whenever the command line merely mentions the script
name, and `pkill -f` on such a pattern kills the shell running it. Matches are
also filtered to this uid, since /proc is world-readable on these shared login
nodes and would otherwise surface other users' processes. The script name is
assembled at runtime as a matter of hygiene, so this file's own command line
never contains the literal being searched for.
"""

import os
import signal
import socket
import sys

TARGET = "herdr-spinner" + ".py"


def daemons():
    found = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open("/proc/%s/cmdline" % entry, "rb") as handle:
                parts = handle.read().split(b"\0")
        except Exception:
            continue
        # /proc is world-readable on these nodes, so another user's daemon
        # would otherwise be reported (and SIGTERMed) as one of ours.
        try:
            if os.stat("/proc/%s" % entry).st_uid != os.getuid():
                continue
        except Exception:
            continue
        argv = [p.decode("utf-8", "replace") for p in parts if p]
        # Skip interpreter flags, so `python3 -u herdr-spinner.py` still matches.
        script = next((a for a in argv[1:] if not a.startswith("-")), "")
        if argv and "python" in os.path.basename(argv[0]) \
                and script.endswith(TARGET):
            found.append(int(entry))
    return sorted(found)


USAGE = """usage: orphan-scan.py [list|kill]

  list  report this node's daemons, separating orphans from the pid-file one
  kill  SIGTERM orphans only; the pid-file daemon is never touched
"""


def own_pid():
    """The pid this node's launcher considers legitimate, or None.

    The daemon writes its own pid into the per-node lock file after taking
    the flock, so this is the one process on this host that must survive.
    """
    path = os.path.expanduser(
        "~/.config/herdr/herdr-spinner.%s.pid"
        % socket.gethostname().split(".")[0])
    try:
        with open(path) as handle:
            return int(handle.read().strip())
    except Exception:
        return None


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "list"
    if mode not in ("list", "kill"):
        sys.stderr.write(USAGE)
        return 2
    host = socket.gethostname().split(".")[0]
    mine = own_pid()
    pids = daemons()
    if not pids:
        print("%s: none" % host)
        return 0
    orphans = [p for p in pids if p != mine]
    if mode == "kill":
        killed = []
        for pid in orphans:
            try:
                os.kill(pid, signal.SIGTERM)
                killed.append(pid)
            except Exception as exc:
                print("%s: pid %d kill failed: %s" % (host, pid, exc))
        print("%s: SIGTERM -> %s (kept pid-file daemon %s)" % (host, killed, mine))
    else:
        print("%s: orphans=%s pid-file-daemon=%s" % (host, orphans, mine))
    return 0


if __name__ == "__main__":
    sys.exit(main())
