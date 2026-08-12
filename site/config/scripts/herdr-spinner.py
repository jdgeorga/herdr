#!/usr/bin/env python3
"""Animated agent-state icons for the Herdr sidebar.

Herdr compiles its own state glyphs in, so the sidebar shows a static dot for
every state including `working`. This daemon replaces that column with custom
pane-metadata tokens (`herdr pane report-metadata --token`) and animates the
`working` one.

One token per state, so each can carry its own colour in config.toml:

    sp_work   animated braille spinner   (working)
    sp_idle   hollow dot                 (idle)
    sp_done   filled dot                 (done)
    sp_block  filled ring                (blocked)
    sp_unk    faint dot                  (unknown)

Exactly one is set on a pane at a time; the rest are cleared. Every token is
written with a TTL, so if this daemon dies the icons disappear (a visible,
honest failure) instead of freezing on a stale frame.

Each token's value is the whole left cluster — state glyph, agent symbol, and
workspace label — as one string, e.g. "⠹ ✳ SETUP CONFIG". Herdr joins separate
row tokens with " · " and that separator is not configurable, so the only way
to render the cluster without dots between its parts is to emit it as a single
token. The consequence is that one colour covers all three, which is why the
row config still keeps five tokens: the colour tracks the state.

It also publishes a `display_agent` symbol. That is redundant for the row above
(the symbol is already inside the token) but keeps any other surface where
Herdr prints the agent label showing the glyph rather than "claude".

Talks to the Herdr socket API directly rather than shelling out to the `herdr`
binary: a metadata write is ~4.5ms over the socket vs ~40ms per subprocess.
The socket serves one request per connection, except event subscriptions,
which stream until closed.
"""

import errno
import fcntl
import json
import os
import select
import signal
import socket
import sys
import time

SOURCE = "herdr-spinner"

# Animation frames for `working`. Fixed one cell wide, so rows never shift.
WORKING_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
FRAME_INTERVAL = 0.10

# Static glyphs for the settled states.
STATIC_GLYPHS = {
    "idle": "○",
    "done": "●",
    "blocked": "◉",
    "unknown": "·",
}

# Which token name carries each state.
TOKEN_FOR_STATE = {
    "working": "sp_work",
    "idle": "sp_idle",
    "done": "sp_done",
    "blocked": "sp_block",
    "unknown": "sp_unk",
}
ALL_TOKENS = sorted(set(TOKEN_FOR_STATE.values()))

# Shown in place of the agent's name, keyed by canonical agent id. "✳" is the
# glyph Claude Code puts in its own terminal title. Agents not listed here keep
# their plain text label.
AGENT_SYMBOLS = {
    "claude": "✳",
    "codex": "◇",
}

TOKEN_TTL_MS = 6000
# Rewrite an unchanged pane this often so its TTL never lapses.
REFRESH_INTERVAL = 2.0
# Full reconcile against agent.list, in case an event was missed.
RESYNC_INTERVAL = 10.0
# Idle poll interval when nothing is working: just wait on events.
IDLE_INTERVAL = 1.0

# Reconnect backoff. A Herdr server restart is brief; anything longer than
# GIVE_UP_AFTER means this daemon is orphaned (its server is gone for good, or
# it lost a race with another instance), so it exits rather than retrying and
# logging forever. The SessionStart hook starts a fresh one when needed.
RETRY_MIN = 1.0
RETRY_MAX = 30.0
GIVE_UP_AFTER = 300.0

# Runtime state is per-node. ~/.config is a shared filesystem across every
# Perlmutter login node, so an unqualified pid/lock/log would be contended by
# daemons on hosts that know nothing about each other: they would fight over
# one lock and interleave into one log. The Herdr socket has the same problem
# from the other direction — the socket *file* is visible on every node, but
# only the node running the server can connect to it, which is why an orphan
# elsewhere sees ECONNREFUSED forever. See verify_server().
HOSTNAME = socket.gethostname().split(".")[0]
LOG_PATH = os.path.expanduser("~/.config/herdr/herdr-spinner.%s.log" % HOSTNAME)
LOCK_PATH = os.path.expanduser("~/.config/herdr/herdr-spinner.%s.pid" % HOSTNAME)
LOG_MAX_BYTES = 1 << 20


def log(message):
    try:
        # Truncate in place rather than unlinking: the daemon's stdout/stderr
        # are an append handle on this file, and unlinking would orphan them.
        if os.path.exists(LOG_PATH) and os.path.getsize(LOG_PATH) > LOG_MAX_BYTES:
            with open(LOG_PATH, "w"):
                pass
        with open(LOG_PATH, "a", encoding="utf-8") as handle:
            handle.write("%s %s\n" % (time.strftime("%Y-%m-%d %H:%M:%S"), message))
    except Exception:
        pass


def acquire_single_instance():
    """Take an exclusive lock, or return None if another daemon holds it.

    The lock file doubles as the pid file, written by the daemon itself. The
    launcher cannot do this: it backgrounds `setsid`, and `$!` is setsid's pid,
    not python's, so a launcher-written pid file points at a process that has
    already exited and every start would spawn another daemon.
    """
    handle = open(LOCK_PATH, "a+")
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except IOError:
        handle.close()
        return None
    handle.seek(0)
    handle.truncate()
    handle.write("%d\n" % os.getpid())
    handle.flush()
    return handle  # held open for the process lifetime; the lock dies with it


def socket_path():
    explicit = os.environ.get("HERDR_SOCKET_PATH")
    if explicit:
        return explicit
    return os.path.expanduser("~/.config/herdr/herdr.sock")


class HerdrApi(object):
    """One-shot request/response calls over the Herdr unix socket."""

    def __init__(self, path):
        self.path = path

    def call(self, method, params=None, timeout=5.0):
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(timeout)
        try:
            sock.connect(self.path)
            payload = {"id": SOURCE, "method": method, "params": params or {}}
            sock.sendall((json.dumps(payload) + "\n").encode("utf-8"))
            reader = sock.makefile("r", encoding="utf-8")
            line = reader.readline()
        finally:
            try:
                sock.close()
            except Exception:
                pass
        if not line:
            raise IOError("empty response to %s" % method)
        return json.loads(line)

    def subscribe(self, subscriptions, timeout=5.0):
        """Open a streaming connection. Returns the connected socket."""
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(timeout)
        sock.connect(self.path)
        payload = {
            "id": SOURCE + ":events",
            "method": "events.subscribe",
            "params": {"subscriptions": subscriptions},
        }
        sock.sendall((json.dumps(payload) + "\n").encode("utf-8"))
        ack = sock.makefile("r", encoding="utf-8").readline()
        if not ack:
            raise IOError("subscription closed immediately")
        parsed = json.loads(ack)
        if "error" in parsed:
            raise IOError("subscribe failed: %s" % parsed["error"])
        sock.setblocking(False)
        return sock


class Spinner(object):
    def __init__(self):
        self.api = HerdrApi(socket_path())
        # pane_id -> agent_status
        self.states = {}
        # pane_id -> canonical agent id, for the display symbol
        self.agents = {}
        # pane_id -> workspace_id, and workspace_id -> display label
        self.pane_workspace = {}
        self.workspace_labels = {}
        # pane_id -> (token_name, value, symbol, written_at)
        self.written = {}
        self.events = None
        self.buffer = b""
        self.running = True

    # -- state tracking ----------------------------------------------------

    def note_pane(self, pane):
        pane_id = pane.get("pane_id")
        if not pane_id:
            return
        # Only agent panes get a state icon in the sidebar.
        if not pane.get("agent"):
            self.forget_pane(pane_id)
            return
        self.states[pane_id] = pane.get("agent_status") or "unknown"
        self.agents[pane_id] = pane.get("agent")
        if pane.get("workspace_id"):
            self.pane_workspace[pane_id] = pane.get("workspace_id")

    def forget_pane(self, pane_id):
        self.states.pop(pane_id, None)
        self.agents.pop(pane_id, None)
        self.pane_workspace.pop(pane_id, None)
        self.written.pop(pane_id, None)

    def refresh_workspaces(self):
        response = self.api.call("workspace.list")
        spaces = (response.get("result") or {}).get("workspaces") or []
        self.workspace_labels = dict(
            (w["workspace_id"], w.get("label") or "")
            for w in spaces if w.get("workspace_id"))

    def resync(self):
        response = self.api.call("agent.list")
        agents = (response.get("result") or {}).get("agents") or []
        live = set()
        for agent in agents:
            pane_id = agent.get("pane_id")
            if not pane_id:
                continue
            live.add(pane_id)
            self.states[pane_id] = agent.get("agent_status") or "unknown"
            self.agents[pane_id] = agent.get("agent")
            if agent.get("workspace_id"):
                self.pane_workspace[pane_id] = agent.get("workspace_id")
        for pane_id in list(self.states):
            if pane_id not in live:
                self.forget_pane(pane_id)
        self.refresh_workspaces()

    # -- event stream ------------------------------------------------------

    def open_events(self):
        self.events = self.api.subscribe([
            {"type": "pane.updated"},
            {"type": "pane.created"},
            {"type": "pane.closed"},
            {"type": "pane.exited"},
            {"type": "pane.agent_detected"},
            # The workspace label is baked into the token value, so a rename
            # has to reach us without waiting for the next reconcile.
            {"type": "workspace.created"},
            {"type": "workspace.renamed"},
            {"type": "workspace.closed"},
        ])
        self.buffer = b""

    def drain_events(self):
        """Read whatever is pending. Returns False if the stream died."""
        while True:
            try:
                chunk = self.events.recv(65536)
            except socket.error as exc:
                if exc.errno in (errno.EAGAIN, errno.EWOULDBLOCK):
                    return True
                return False
            if not chunk:
                return False
            self.buffer += chunk
            while b"\n" in self.buffer:
                line, self.buffer = self.buffer.split(b"\n", 1)
                if line.strip():
                    self.handle_event(line)

    def handle_event(self, raw):
        try:
            event = json.loads(raw.decode("utf-8"))
        except Exception:
            return
        data = event.get("data") or {}
        pane = data.get("pane")
        if isinstance(pane, dict):
            self.note_pane(pane)
            return
        kind_any = data.get("type") or event.get("event") or ""
        if kind_any.startswith("workspace"):
            try:
                self.refresh_workspaces()
            except Exception as exc:
                log("workspace refresh failed: %s" % exc)
            return
        pane_id = data.get("pane_id")
        if not pane_id:
            return
        kind = data.get("type") or event.get("event") or ""
        if kind in ("pane_closed", "pane_exited"):
            self.forget_pane(pane_id)
        elif "agent_status" in data:
            self.states[pane_id] = data.get("agent_status") or "unknown"
        elif kind == "pane_agent_detected":
            if data.get("released"):
                self.forget_pane(pane_id)
            else:
                self.states[pane_id] = data.get("final_status") or "unknown"
                if data.get("agent"):
                    self.agents[pane_id] = data.get("agent")

    # -- rendering ---------------------------------------------------------

    def glyph_for(self, state, frame_index):
        if state == "working":
            return WORKING_FRAMES[frame_index % len(WORKING_FRAMES)]
        return STATIC_GLYPHS.get(state, STATIC_GLYPHS["unknown"])

    def compose(self, pane_id, state, frame_index, symbol):
        """The whole left cluster as one token value: "⠹ ✳ SETUP CONFIG"."""
        parts = [self.glyph_for(state, frame_index)]
        if symbol:
            parts.append(symbol)
        label = self.workspace_labels.get(self.pane_workspace.get(pane_id))
        if label:
            parts.append(label)
        return " ".join(parts)

    def paint(self, frame_index, now):
        for pane_id, state in list(self.states.items()):
            token = TOKEN_FOR_STATE.get(state, TOKEN_FOR_STATE["unknown"])
            symbol = AGENT_SYMBOLS.get(self.agents.get(pane_id))
            value = self.compose(pane_id, state, frame_index, symbol)
            previous = self.written.get(pane_id)
            if previous is not None:
                same = (previous[0] == token and previous[1] == value
                        and previous[2] == symbol)
                if same and now - previous[3] < REFRESH_INTERVAL:
                    continue
            tokens = dict((name, None) for name in ALL_TOKENS)
            tokens[token] = value
            params = {
                "pane_id": pane_id,
                "source": SOURCE,
                "tokens": tokens,
                "ttl_ms": TOKEN_TTL_MS,
            }
            if symbol:
                params["display_agent"] = symbol
            else:
                params["clear_display_agent"] = True
            try:
                self.api.call("pane.report_metadata", params)
            except Exception:
                # Could be a vanished pane or just a slow socket, and there is
                # no way to tell from here. Leave it in self.states and let the
                # next resync drop it if it really went away; the 6s token TTL
                # covers the gap either way.
                continue
            self.written[pane_id] = (token, value, symbol, now)

    def clear_all(self):
        tokens = dict((name, None) for name in ALL_TOKENS)
        for pane_id in list(self.states):
            try:
                self.api.call("pane.report_metadata", {
                    "pane_id": pane_id,
                    "source": SOURCE,
                    "tokens": tokens,
                    "clear_display_agent": True,
                })
            except Exception:
                pass

    # -- main loop ---------------------------------------------------------

    def stop(self, *_args):
        self.running = False

    def drop_events(self):
        if self.events is not None:
            try:
                self.events.close()
            except Exception:
                pass
        self.events = None

    def run(self):
        signal.signal(signal.SIGTERM, self.stop)
        signal.signal(signal.SIGINT, self.stop)
        self.resync()
        self.open_events()
        log("started, tracking %d agent panes" % len(self.states))

        started = time.time()
        last_resync = started
        retry_delay = RETRY_MIN
        down_since = None
        while self.running:
            now = time.time()

            # A Herdr server restart drops the stream. Reconnect from the top
            # of the loop so a failed attempt is simply retried next pass.
            if self.events is None:
                try:
                    self.open_events()
                    self.resync()
                    last_resync = now
                    if down_since is not None:
                        log("event stream reconnected after %.0fs"
                            % (now - down_since))
                    down_since = None
                    retry_delay = RETRY_MIN
                except Exception as exc:
                    if down_since is None:
                        down_since = now
                        # Log the first failure only; the backoff loop that
                        # follows would otherwise flood the file.
                        log("lost the Herdr server (%s), retrying" % exc)
                    elif now - down_since > GIVE_UP_AFTER:
                        log("no Herdr server for %.0fs, exiting"
                            % (now - down_since))
                        self.running = False
                        break
                    self.drop_events()
                    time.sleep(retry_delay)
                    retry_delay = min(retry_delay * 2, RETRY_MAX)
                    continue

            # Frame from elapsed time, not loop count: a burst of pane.updated
            # events wakes select() early and would otherwise spin the glyph.
            frame_index = int((now - started) / FRAME_INTERVAL)
            if now - last_resync >= RESYNC_INTERVAL:
                try:
                    self.resync()
                except Exception as exc:
                    log("resync failed: %s" % exc)
                last_resync = now

            self.paint(frame_index, now)

            any_working = any(s == "working" for s in self.states.values())
            wait = FRAME_INTERVAL if any_working else IDLE_INTERVAL
            try:
                ready, _, _ = select.select([self.events], [], [], wait)
            except (select.error, ValueError):
                ready = []
            if ready and not self.drain_events():
                log("event stream closed")
                self.drop_events()
                time.sleep(0.5)

        log("stopping, clearing tokens")
        self.clear_all()


def verify_server():
    """True if a Herdr server on THIS node answers on the socket.

    Testing that the socket file exists is not enough: it lives on a shared
    filesystem, so every login node sees it whether or not a server is running
    there. Only a completed ping proves there is something to talk to.
    """
    try:
        HerdrApi(socket_path()).call("ping", timeout=3.0)
        return True
    except Exception:
        return False


def main():
    if not verify_server():
        # No server on this node. Exit instead of spinning: the socket file is
        # shared storage and would otherwise refuse connections indefinitely.
        return 0
    lock = acquire_single_instance()
    if lock is None:
        # Another daemon is already running. Not an error: the SessionStart
        # hook fires once per Claude session and they all call `start`.
        return 0
    try:
        Spinner().run()
    except Exception as exc:
        log("fatal: %s" % exc)
        return 1
    finally:
        lock.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
