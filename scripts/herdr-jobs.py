#!/usr/bin/python3.11 -IS
"""SLURM provider for herdr's sidebar jobs list.

Invoked by herdr as `/usr/bin/python3.11 herdr-jobs.py --mode live|history`.
Emits exactly one JSON object on stdout matching the herdr list-section
protocol (see specs/2026-08-06-herdr-slurm-jobs-sidebar-design.md). Standard
library only, targets Python 3.11 (NERSC's bare `python3` is 3.6.15 and is
not usable).

INVOCATION CONTRACT (LOW 7a): this script must run with site customization
and user-site imports disabled, so nothing can print to stdout ahead of
main() (sitecustomize.py, a stray usercustomize, a PYTHONSTARTUP file, etc).
The shebang above passes `-IS` (combined `-I -S`) for direct execution, but
herdr's `[ui.sidebar.list] command` invokes `/usr/bin/python3.11 <script>
--mode {mode}` directly rather than exec'ing this file, which means the
shebang line is NOT consulted. The command array in config.toml MUST include
the flags itself:

    command = ["/usr/bin/python3.11", "-I", "-S", "<path>/herdr-jobs.py", "--mode", "{mode}"]

If the configured command ever omits `-I -S`, this contract is not actually
enforced at runtime; that is a config-side fix outside this file.

Contract: stdout carries JSON and nothing else, ever.
  - A successful poll (including a genuinely empty queue/history) prints one
    JSON object and exits 0.
  - Any command failure, timeout, malformed external output, or internal
    exception still prints exactly one valid JSON object (never a bare
    traceback) but exits NON-ZERO, so herdr can tell "no jobs" apart from
    "couldn't ask" and retain+stale the previous rows instead of clearing
    them (HIGH 2).
Run with --selftest to exercise the parsing/linger logic against recorded
fixtures without touching the real cluster; --selftest reports on stderr
(LOW 7b) and still uses its exit code (0 pass / 1 fail) as the signal.
"""

from __future__ import annotations

import argparse
import fcntl
import getpass
import json
import math
import os
import re
import stat
import subprocess
import sys
import tempfile
import time
from datetime import datetime
from typing import Any, Callable

PROTOCOL_VERSION = 1
TITLE = "JOBS"
MAX_ROWS = 2000
DONE_TTL_SECONDS = 600

# Consumer-side hard caps (see spec "Provider protocol" section): 2000 rows,
# 1 KiB per string, 256 KiB total. Exceeding any of these fails the WHOLE
# payload. We enforce our own versions of these caps before emitting so a
# large/adversarial poll degrades gracefully instead of being rejected
# wholesale (MEDIUM 3).
STRING_BUDGET_BYTES = 1024
MAX_PAYLOAD_BYTES = 256 * 1024 - 64  # small margin for the trailing newline

STYLE_NAMES = {"ok", "fail", "warn", "muted", "normal"}

# Valid herdr *notify levels* (distinct from row *styles* above). "muted" is
# a valid row style but NOT a valid notify level -- see NOTIFY_LEVEL_FOR_STYLE
# (LOW 8).
NOTIFY_LEVELS = {"info", "ok", "warn", "fail"}
NOTIFY_LEVEL_FOR_STYLE = {"ok": "ok", "fail": "fail", "warn": "warn", "muted": "info", "normal": "info"}

# COMPLETED/FAILED/TIMEOUT/CANCELLED are the four states the spec assigns a
# style+glyph. Anything else sacct can report (NODE_FAIL, OUT_OF_MEMORY, ...)
# is not covered by the spec; history mode falls back to a generic marker
# rather than silently dropping the job (see module-level note in
# compute_history_payload). Linger intentionally does NOT use that fallback:
# an unmapped state on a vanished job is dropped rather than guessed at.
STATE_STYLE_MAP: dict[str, tuple[str, str]] = {
    "COMPLETED": ("ok", "✓"),   # check mark
    "FAILED": ("fail", "✗"),    # ballot x
    "TIMEOUT": ("warn", "TO"),
    "CANCELLED": ("muted", "CA"),
}

# Order the collapsed-header summary counts appear in.
SUMMARY_GLYPH_ORDER = (("ok", "✓"), ("fail", "✗"), ("warn", "TO"), ("muted", "CA"))

TERMINAL_SACCT_STATES = {
    "COMPLETED", "FAILED", "TIMEOUT", "CANCELLED", "NODE_FAIL",
    "OUT_OF_MEMORY", "PREEMPTED", "BOOT_FAIL", "DEADLINE",
}

# Directory-name components too generic to identify a job on their own; when
# the last path component is one of these (or purely numeric, e.g. a run
# index), the parent component is prepended for context. This heuristic is
# not fully specified by the design doc (which only says "last one or two
# components"); see the returned `problems` note in the task summary.
GENERIC_DIR_NAMES = {
    "run", "runs", "out", "output", "outputs", "logs", "log",
    "tmp", "scratch", "job", "jobs", "results", "result", "work",
}

_CONTROL_RE = re.compile(r"[\x00-\x1f\x7f]")


class ProviderError(Exception):
    """Raised for any external-command failure; always caught at the top."""


def clean(value: Any) -> str:
    """Strip control characters / escapes before a string reaches the renderer."""
    if value is None:
        return ""
    return _CONTROL_RE.sub("", str(value))


def dir_label(workdir: str) -> str:
    """Last one or two path components of a job's workdir, for the row's first cell."""
    if not workdir:
        return ""
    parts = [p for p in workdir.strip("/").split("/") if p]
    if not parts:
        return "/"
    last = parts[-1]
    if len(parts) >= 2 and (last.isdigit() or last.lower() in GENERIC_DIR_NAMES):
        return f"{parts[-2]}/{last}"
    return last


def nodes_cell(nodes_field: str) -> str:
    nodes_field = (nodes_field or "").strip()
    if not nodes_field:
        return "?N"
    return clean(f"{nodes_field}N")


def map_state(raw_state: str) -> tuple[str, str] | None:
    """Map a sacct State value (possibly 'CANCELLED by 12345') to (style, glyph)."""
    if not raw_state:
        return None
    stripped = raw_state.strip()
    if not stripped:
        return None
    token = stripped.split()[0]
    return STATE_STYLE_MAP.get(token)


def error_payload(message: str) -> dict[str, Any]:
    return {
        "version": PROTOCOL_VERSION,
        "title": TITLE,
        "summary": "",
        "groups": [],
        "notify": [{"id": "error-internal", "level": "fail", "text": clean(message)[:200]}],
    }


def cap_rows(groups: list[dict[str, Any]], cap: int) -> list[dict[str, Any]]:
    total = 0
    result = []
    for group in groups:
        rows = group.get("rows", [])
        remaining = cap - total
        if remaining <= 0:
            break
        if len(rows) > remaining:
            rows = rows[:remaining]
        total += len(rows)
        capped = dict(group)
        capped["rows"] = rows
        result.append(capped)
    return result


# --- squeue parsing (HIGH 1: JSON, not pipe-delimited text) ----------------
#
# `squeue -u $USER -h -o '%i|%j|...'` silently mis-parses (and drops) any row
# whose job name or workdir contains "|". `squeue --json` is structured, so a
# "|" or a newline inside a job name can never shift a field boundary.
# Measured on this machine: ~40ms for a live user, no slower than the text
# format. Numeric/boolean-ish fields come back as
# `{"set": bool, "infinite": bool, "number": N}` objects rather than plain
# scalars; `_slurm_number` below is the one place that unwraps that shape.

def _slurm_number(field: Any) -> tuple[float | int | None, bool]:
    """Unwrap a Slurm JSON `{set, infinite, number}` object.

    Returns (value, is_infinite). value is None when the field is unset or
    infinite; is_infinite is True for an explicit "no bound" (e.g. an
    unlimited time limit), which callers render as UNLIMITED rather than N/A.
    """
    if not isinstance(field, dict) or not field.get("set", False):
        return None, False
    if field.get("infinite", False):
        return None, True
    value = field.get("number")
    if not isinstance(value, (int, float)):
        return None, False
    return value, False


def format_duration(total_seconds: float) -> str:
    """H:MM:SS, or D-HH:MM:SS past 24h -- the same human strings squeue's
    %L/%l used to hand us pre-formatted as text."""
    seconds_int = max(0, int(round(total_seconds)))
    if seconds_int >= 86400:
        days, rem = divmod(seconds_int, 86400)
        hours, rem = divmod(rem, 3600)
        minutes, secs = divmod(rem, 60)
        return f"{days}-{hours:02d}:{minutes:02d}:{secs:02d}"
    hours, rem = divmod(seconds_int, 3600)
    minutes, secs = divmod(rem, 60)
    return f"{hours}:{minutes:02d}:{secs:02d}"


def _time_limit_str(job: dict[str, Any]) -> str:
    value, infinite = _slurm_number(job.get("time_limit", {}))
    if infinite:
        return "UNLIMITED"
    if value is None:
        return "N/A"
    return format_duration(value * 60)  # time_limit.number is in minutes


def _time_left_str(job: dict[str, Any], now: float) -> str:
    tl_value, tl_infinite = _slurm_number(job.get("time_limit", {}))
    if tl_infinite:
        return "UNLIMITED"
    end_value, end_infinite = _slurm_number(job.get("end_time", {}))
    if end_infinite:
        return "UNLIMITED"
    if end_value is None:
        return "N/A"
    return format_duration(end_value - now)


def _start_str(job: dict[str, Any]) -> str:
    value, infinite = _slurm_number(job.get("start_time", {}))
    if infinite:
        return "UNLIMITED"
    if value is None or value == 0:
        return "N/A"
    try:
        return datetime.fromtimestamp(value).isoformat()
    except (OverflowError, OSError, ValueError):
        return "N/A"


_STATE_CODE_MAP = {
    "RUNNING": "R", "PENDING": "PD", "COMPLETING": "CG", "CONFIGURING": "CF",
    "SUSPENDED": "S", "STOPPED": "ST", "PREEMPTED": "PR", "REQUEUED": "RQ",
    "RESIZING": "RS",
}


def _job_state_code(job: dict[str, Any]) -> str:
    """Only 'R' and 'PD' are meaningful to compute_live_payload's grouping;
    everything else just needs to be a stable non-crashing token so a
    transitional state (e.g. CG) doesn't look like a vanish."""
    states = job.get("job_state")
    token = ""
    if isinstance(states, list) and states and isinstance(states[0], str):
        token = states[0]
    elif isinstance(states, str):
        token = states
    return _STATE_CODE_MAP.get(token, token[:2] if token else "UNK")


# --- MEDIUM 4: pending array jobs must not collapse to an uncancellable id -
#
# Without -r/--array, squeue's TEXT format collapses pending array elements
# into "123_[1-20]", which fails the consumer's job-id regex
# ^[0-9]+(_[0-9]+)?(\+[0-9]+)?$ and makes cancel silently always fail.
# squeue --json on this cluster (Slurm 25.11) gives each started/expanded
# task its own entry with array_task_id.set=True, which already yields a
# valid "<array_job_id>_<task_id>" id with no special handling. The one case
# that still needs explicit expansion is a *pending* array task collapsed
# into `array_task_string` (e.g. "5-7" or "1,3,5-9", optionally with a
# "%throttle" suffix) -- verified against a synthetic fixture below since no
# real pending array job existed on this account at review time. If a future
# Slurm build represents this differently in a way `_parse_array_task_string`
# can't parse, we fail safe by keeping the row visible but omitting `cancel`
# rather than emitting an id the consumer would reject.

_ARRAY_RANGE_RE = re.compile(r"^\s*(\d+)(?:-(\d+))?\s*$")


def _parse_array_task_string(raw: str, cap: int = 4096) -> list[int] | None:
    raw = raw.strip()
    if not raw:
        return None
    raw = raw.split("%", 1)[0]  # strip a "%throttle" suffix, e.g. "1-100%10"
    ids: list[int] = []
    for token in raw.split(","):
        match = _ARRAY_RANGE_RE.match(token)
        if not match:
            return None
        lo = int(match.group(1))
        hi = int(match.group(2)) if match.group(2) else lo
        if hi < lo:
            return None
        for n in range(lo, hi + 1):
            ids.append(n)
            if len(ids) >= cap:
                return ids
    return ids or None


def _job_row_ids(job: dict[str, Any]) -> list[tuple[str, bool]]:
    """Return [(row_id, cancelable), ...] for one squeue --json job entry.
    Most jobs yield exactly one row; a collapsed pending array entry expands
    to one row per task."""
    array_job_id, _ = _slurm_number(job.get("array_job_id", {}))
    array_task_field = job.get("array_task_id")
    array_task_id, _ = _slurm_number(array_task_field)
    array_task_id_set = isinstance(array_task_field, dict) and array_task_field.get("set", False)
    array_task_string = job.get("array_task_string") or ""
    base_job_id = job.get("job_id")

    if not array_job_id:  # 0/None/unset: not part of an array
        return [(str(base_job_id), True)] if base_job_id else []

    if array_task_id_set and array_task_id is not None:
        return [(f"{int(array_job_id)}_{int(array_task_id)}", True)]

    if array_task_string:
        expanded = _parse_array_task_string(array_task_string)
        if expanded:
            return [(f"{int(array_job_id)}_{n}", True) for n in expanded]
        # Unparseable collapsed range: keep the row visible, but its id can't
        # satisfy the cancel action's job-id regex, so withhold cancel rather
        # than offer an action that will always fail.
        if base_job_id:
            return [(f"{int(array_job_id)}_[{clean(array_task_string)}]", False)]
        return []

    if base_job_id:
        return [(str(base_job_id), True)]
    return []


def parse_squeue_json(text: str, now: float) -> list[dict[str, Any]]:
    try:
        data = json.loads(text)
    except ValueError as exc:
        raise ProviderError(f"squeue: invalid JSON output: {exc}") from exc
    if not isinstance(data, dict):
        raise ProviderError("squeue: unexpected JSON shape (not an object)")
    jobs = data.get("jobs")
    if not isinstance(jobs, list):
        raise ProviderError("squeue: JSON missing 'jobs' array")

    rows: list[dict[str, Any]] = []
    for job in jobs:
        if not isinstance(job, dict):
            continue
        try:
            node_count, _ = _slurm_number(job.get("node_count", {}))
            stdout_path = job.get("standard_output") or ""
            stderr_path = job.get("standard_error") or ""
            workdir = job.get("current_working_directory") or ""
            base_row = {
                "name": clean(job.get("name", "")),
                "state": _job_state_code(job),
                "partition": clean(job.get("partition", "")),
                "qos": clean(job.get("qos", "")),
                "nodes": str(node_count) if node_count is not None else "",
                "timeleft": _time_left_str(job, now),
                "timelimit": _time_limit_str(job),
                "start": _start_str(job),
                "reason": clean(job.get("state_reason", "")),
                "workdir": workdir,
                "stdout": stdout_path,
                "stderr": stderr_path,
            }
            for job_id, cancelable in _job_row_ids(job):
                if not job_id:
                    continue
                row = dict(base_row)
                row["id"] = job_id
                row["cancelable"] = cancelable
                rows.append(row)
                if len(rows) >= 5000:  # defensive cap; login-node job counts never approach this
                    return rows
        except (TypeError, ValueError, KeyError, AttributeError, OverflowError):
            continue  # one malformed job entry must not drop the whole poll
    return rows


def fetch_squeue_rows(now: float) -> list[dict[str, Any]]:
    user = os.environ.get("USER") or getpass.getuser()
    cmd = ["squeue", "-u", user, "--json"]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=8)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ProviderError(f"squeue: {exc}") from exc
    if proc.returncode != 0:
        raise ProviderError(f"squeue exit {proc.returncode}: {proc.stderr.strip()[:200]}")
    return parse_squeue_json(proc.stdout, now)


def sacct_lookup(job_id: str) -> tuple[str, str] | None:
    """Exactly the command the design doc specifies. Runs only on a vanish
    transition. Unlike the squeue/sacct-history listing paths, this is safe
    to leave in pipe/text form: State and ExitCode are always "TOKEN" and
    "N:N" respectively and can never contain "|" the way a job name or
    WorkDir can, so the HIGH 1 field-shift bug does not apply here."""
    cmd = ["sacct", "-j", job_id, "--format=State,ExitCode", "-n", "-P"]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=8)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        fields = line.split("|")
        state = fields[0].strip()
        exitcode = fields[1].strip() if len(fields) > 1 else ""
        if not state:
            continue
        return (state, exitcode)
    return None


def build_active_row(row: dict[str, Any], dlabel: str, ncell: str, running: bool) -> dict[str, Any]:
    time_cell = clean(row["timeleft"] if running else row["timelimit"])
    row_vars: dict[str, str] = {"dir": row["workdir"]}
    actions: list[str] = []
    if row.get("cancelable", True):
        actions.append("cancel")
    if running:
        stdout_path = row["stdout"]
        # standard_output is an unresolved template (slurm-%j.out, or a
        # %x/%j-containing path from -o) until a job starts; only expose tail
        # once squeue has resolved it to a real absolute path.
        if stdout_path and "%" not in stdout_path and stdout_path.startswith("/"):
            row_vars["log"] = stdout_path
            actions.append("tail")
    return {
        "id": row["id"],
        "cells": [clean(dlabel), ncell, time_cell],
        "style": "normal",
        "vars": row_vars,
        "actions": actions,
    }


def job_sort_key(item: tuple[str, dict[str, Any]]) -> tuple[int, str]:
    job_id = item[0]
    match = re.match(r"^(\d+)", job_id)
    return (int(match.group(1)) if match else (1 << 62), job_id)


def build_summary(running_count: int, queued_count: int, done_map: dict[str, dict[str, Any]]) -> str:
    parts = []
    if running_count:
        parts.append(f"{running_count}R")
    if queued_count:
        parts.append(f"{queued_count}Q")
    style_counts: dict[str, int] = {}
    for entry in done_map.values():
        style = entry.get("style")
        style_counts[style] = style_counts.get(style, 0) + 1
    for style, glyph in SUMMARY_GLYPH_ORDER:
        n = style_counts.get(style, 0)
        if n:
            parts.append(f"{n}{glyph}")
    return "  ".join(parts)


def compute_live_payload(
    rows: list[dict[str, Any]],
    prior_state: dict[str, Any],
    now: float,
    lookup: Callable[[str], tuple[str, str] | None],
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Pure(-ish) core of live mode: squeue rows + prior state -> payload + new state.

    `lookup` is injected so tests can exercise the linger transition without
    calling sacct. `prior_state` is expected to already be validated (see
    validate_state / MEDIUM 6); the checks below are defense in depth, not
    the primary sanitizer.
    """
    prior_state = prior_state if isinstance(prior_state, dict) else {}
    prior_seen = prior_state.get("seen_ids")
    prior_seen = prior_seen if isinstance(prior_seen, dict) else {}
    prior_done = prior_state.get("done")
    prior_done = prior_done if isinstance(prior_done, dict) else {}

    current: dict[str, dict[str, str]] = {}
    running_rows: list[tuple[str, dict[str, Any]]] = []
    queued_rows: list[tuple[str, dict[str, Any]]] = []

    for row in rows:
        job_id = row.get("id", "")
        if not job_id or job_id in current:
            continue  # duplicate/blank id: first wins, matching protocol dedup rule
        dlabel = dir_label(row["workdir"])
        ncell = nodes_cell(row["nodes"])
        current[job_id] = {"dir_label": dlabel, "nodes": ncell}
        state = row.get("state", "")
        if state == "R":
            running_rows.append((job_id, build_active_row(row, dlabel, ncell, running=True)))
        elif state == "PD":
            queued_rows.append((job_id, build_active_row(row, dlabel, ncell, running=False)))
        # Other squeue states (e.g. CG completing) are kept in `current` so a
        # brief transitional state doesn't look like a vanish, but the spec
        # only defines Running/Queued groups so they render nowhere yet.

    # Carry forward not-yet-expired done entries from the previous poll.
    done_map: dict[str, dict[str, Any]] = {}
    for job_id, entry in prior_done.items():
        if not isinstance(entry, dict):
            continue
        try:
            first_seen = float(entry.get("first_seen"))
        except (TypeError, ValueError):
            continue
        if not math.isfinite(first_seen):
            continue
        if now - first_seen >= DONE_TTL_SECONDS:
            continue
        style = entry.get("style")
        if not isinstance(style, str) or style not in STYLE_NAMES or not entry.get("glyph"):
            continue
        done_map[job_id] = {**entry, "first_seen": first_seen}
    # A job that reappears in squeue (requeue) is no longer "done".
    for job_id in list(done_map):
        if job_id in current:
            del done_map[job_id]

    vanished = set(prior_seen) - set(current)
    for job_id in vanished:
        if job_id in done_map:
            continue
        result = lookup(job_id)
        if not result:
            continue
        mapped = map_state(result[0])
        if not mapped:
            continue
        style, glyph = mapped
        prior_info = prior_seen.get(job_id)
        prior_info = prior_info if isinstance(prior_info, dict) else {}
        done_map[job_id] = {
            "style": style,
            "glyph": glyph,
            "dir_label": prior_info.get("dir_label", ""),
            "nodes": prior_info.get("nodes", ""),
            "first_seen": now,
        }

    groups: list[dict[str, Any]] = []
    if running_rows:
        running_rows.sort(key=job_sort_key)
        groups.append({"id": "running", "label": "Running", "rows": [r for _, r in running_rows]})
    if queued_rows:
        queued_rows.sort(key=job_sort_key)
        groups.append({"id": "queued", "label": "Queued", "rows": [r for _, r in queued_rows]})

    notify: list[dict[str, Any]] = []
    if done_map:
        done_items = sorted(done_map.items(), key=lambda kv: kv[1].get("first_seen", 0), reverse=True)
        done_rows = []
        for job_id, entry in done_items:
            style = entry.get("style", "muted")
            done_rows.append({
                "id": job_id,
                "cells": [entry.get("dir_label", ""), entry.get("nodes", ""), entry.get("glyph", "")],
                "style": style,
                "vars": {},
                "actions": [],
            })
            # Re-emitted every poll while the entry lingers; herdr dedupes by
            # notify[].id so this does not re-toast. LOW 8: the notify LEVEL
            # is not the same enum as the row STYLE -- "muted" is a valid
            # style but not a valid notify level, so it must be mapped
            # (e.g. CANCELLED -> style "muted", notify level "info").
            notify.append({
                "id": f"done-{job_id}",
                "level": NOTIFY_LEVEL_FOR_STYLE.get(style, "info"),
                "text": clean(f"{job_id} ({entry.get('dir_label', '')}) finished"),
            })
        groups.append({"id": "done", "label": "Done", "rows": done_rows})

    groups = cap_rows(groups, MAX_ROWS)
    summary = build_summary(len(running_rows), len(queued_rows), done_map)

    payload = {
        "version": PROTOCOL_VERSION,
        "title": TITLE,
        "summary": summary,
        "groups": groups,
        "notify": notify,
    }
    new_state = {"seen_ids": current, "done": done_map}
    return payload, new_state


# --- history mode (also HIGH 1: sacct --json, not --parsable2 text) --------
#
# `sacct --json` (no -X) took ~6.9s for 1092 jobs on this account -- too
# close to/over the 5s poller timeout once job steps are included. Adding
# -X (exclude steps, which this path already wanted) drops that to ~0.5s for
# the same 1092 jobs, comfortably inside the timeout, so we use JSON here too
# rather than falling back to --parsable2 (whose WorkDir field has the exact
# same "|" field-shift risk as squeue's text format).

def parse_sacct_history(text: str) -> list[dict[str, Any]]:
    try:
        data = json.loads(text)
    except ValueError as exc:
        raise ProviderError(f"sacct: invalid JSON output: {exc}") from exc
    if not isinstance(data, dict):
        raise ProviderError("sacct: unexpected JSON shape (not an object)")
    jobs = data.get("jobs")
    if not isinstance(jobs, list):
        raise ProviderError("sacct: JSON missing 'jobs' array")

    entries: list[dict[str, Any]] = []
    for job in jobs:
        if not isinstance(job, dict):
            continue
        try:
            job_id = job.get("job_id")
            if not job_id:
                continue
            job_id_str = str(job_id)
            if "." in job_id_str:
                continue  # a job step id; structurally shouldn't occur under -X
            state_field = job.get("state")
            states = state_field.get("current") if isinstance(state_field, dict) else None
            token = states[0] if isinstance(states, list) and states else ""
            if not isinstance(token, str) or token not in TERMINAL_SACCT_STATES:
                continue
            time_field = job.get("time")
            end = time_field.get("end", 0) if isinstance(time_field, dict) else 0
            if not isinstance(end, (int, float)) or not math.isfinite(end):
                end = 0
            entries.append({
                "id": job_id_str,
                "state": token,
                "workdir": job.get("working_directory", "") or "",
                "nodes": str(job.get("allocation_nodes", "") or ""),
                "end": end,
            })
        except (TypeError, ValueError, KeyError, AttributeError):
            continue
        if len(entries) >= 5000:
            break
    return entries


def fetch_sacct_history() -> list[dict[str, Any]]:
    user = os.environ.get("USER") or getpass.getuser()
    cmd = ["sacct", "-u", user, "-X", "-S", "now-30days", "--json"]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=8)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ProviderError(f"sacct: {exc}") from exc
    if proc.returncode != 0:
        raise ProviderError(f"sacct exit {proc.returncode}: {proc.stderr.strip()[:200]}")
    return parse_sacct_history(proc.stdout)


def compute_history_payload(entries: list[dict[str, Any]], limit: int = 10) -> dict[str, Any]:
    ordered = sorted(entries, key=lambda e: e.get("end", 0) or 0, reverse=True)[:limit]
    rows = []
    for entry in ordered:
        mapped = map_state(entry["state"])
        if mapped is None:
            # State outside the spec's four mapped values (NODE_FAIL, etc.):
            # show it rather than silently dropping the job from history.
            token = entry["state"]
            mapped = ("muted", token[:2].upper() if token else "??")
        style, glyph = mapped
        dlabel = dir_label(entry.get("workdir", ""))
        rows.append({
            "id": entry["id"],
            "cells": [clean(dlabel), nodes_cell(entry.get("nodes", "")), clean(glyph)],
            "style": style,
            "vars": {},
            "actions": [],
        })
    groups = []
    if rows:
        groups.append({"id": "history", "label": "History", "rows": rows})
    groups = cap_rows(groups, MAX_ROWS)
    return {
        "version": PROTOCOL_VERSION,
        "title": TITLE,
        "summary": f"{len(rows)} recent" if rows else "",
        "groups": groups,
        "notify": [],
    }


# --- payload finalization: self-enforced caps (MEDIUM 3) -------------------
#
# The Rust consumer rejects the ENTIRE payload above 256 KiB total or 1 KiB
# per string. A measured 2000-row payload was 378 KB, well over budget. We
# truncate every string to <=1024 UTF-8 bytes (on a char boundary) and then,
# if still oversized, drop rows -- Done first, then Queued, keeping Running
# as long as possible -- until the actual serialized payload fits.

def truncate_utf8(value: str, max_bytes: int) -> str:
    encoded = value.encode("utf-8")
    if len(encoded) <= max_bytes:
        return value
    encoded = encoded[:max_bytes]
    while encoded:
        try:
            return encoded.decode("utf-8")
        except UnicodeDecodeError:
            encoded = encoded[:-1]  # back off until we land on a char boundary
    return ""


def _truncate_strings(obj: Any) -> Any:
    if isinstance(obj, str):
        return truncate_utf8(obj, STRING_BUDGET_BYTES)
    if isinstance(obj, list):
        return [_truncate_strings(v) for v in obj]
    if isinstance(obj, dict):
        return {k: _truncate_strings(v) for k, v in obj.items()}
    return obj


def _dumps(payload: dict[str, Any]) -> str:
    return json.dumps(payload, ensure_ascii=False, separators=(",", ":"))


def _byte_len(s: str) -> int:
    return len(s.encode("utf-8"))


def _row_cost(row: Any) -> int:
    return _byte_len(_dumps(row)) + 1  # +1 for the separating comma


_GROUP_DROP_PRIORITY = {"done": 0, "queued": 1}  # lower drops first; running/history default last


def _enforce_size_cap(payload: dict[str, Any], max_bytes: int = MAX_PAYLOAD_BYTES) -> dict[str, Any]:
    if _byte_len(_dumps(payload)) <= max_bytes:
        return payload

    groups = [dict(g, rows=list(g.get("rows", []))) for g in payload.get("groups", [])]
    order = sorted(range(len(groups)), key=lambda i: _GROUP_DROP_PRIORITY.get(groups[i].get("id"), 2))

    # Coarse pass: use per-row serialized size to estimate how many rows to
    # drop, in priority order, without re-serializing the whole payload for
    # every single row removed.
    excess = _byte_len(_dumps(payload)) - max_bytes
    for i in order:
        rows = groups[i]["rows"]
        while rows and excess > 0:
            excess -= _row_cost(rows[-1])
            rows.pop()
        if excess <= 0:
            break

    result = dict(payload)
    result["groups"] = [g for g in groups if g["rows"]]

    # Fine correction pass: the estimate above can drift slightly (nesting,
    # separators); keep trimming the lowest-priority-to-keep group with rows
    # left until the real serialized size actually fits, bounded so this can
    # never spin forever.
    guard = 0
    while _byte_len(_dumps(result)) > max_bytes and guard < 4096:
        dropped = False
        for gid in ("done", "queued"):
            for g in result["groups"]:
                if g.get("id") == gid and g["rows"]:
                    g["rows"].pop()
                    dropped = True
                    break
            if dropped:
                break
        if not dropped:
            for g in result["groups"]:
                if g["rows"]:
                    g["rows"].pop()
                    dropped = True
                    break
        if not dropped:
            break
        result["groups"] = [g for g in result["groups"] if g["rows"]]
        guard += 1
    return result


def finalize_payload(payload: dict[str, Any]) -> dict[str, Any]:
    """The single choke point every payload passes through before it is
    printed: row-count cap, per-string byte cap, then total-size cap."""
    payload = dict(payload, groups=cap_rows(payload.get("groups", []), MAX_ROWS))
    payload = _truncate_strings(payload)
    payload = _enforce_size_cap(payload, MAX_PAYLOAD_BYTES)
    return payload


# --- state file: private per-node, per-user directory ----------------------
#
# Node-local (under the OS temp dir, typically /tmp, not $HOME which is
# shared across Perlmutter login nodes) and per-user (uid in the name), so
# concurrent herdr instances from different users never collide. The
# directory is created 0700 and its ownership/mode/symlink-ness are verified
# on every use rather than trusted, so a pre-planted symlink or a
# world-writable leftover can't redirect our writes (classic /tmp attack).
# If verification fails we fall back to a private, unlinked-to-anything
# mkdtemp() for this process only: state won't persist, which just means the
# next poll sees "no previous run" -- a safe degrade, not a crash.

def _verify_private_dir(path: str, uid: int) -> bool:
    try:
        st = os.lstat(path)
    except OSError:
        return False
    if not stat.S_ISDIR(st.st_mode):
        return False
    if st.st_uid != uid:
        return False
    if stat.S_IMODE(st.st_mode) != 0o700:
        try:
            os.chmod(path, 0o700)
        except OSError:
            return False
        try:
            st = os.lstat(path)
        except OSError:
            return False
        if stat.S_IMODE(st.st_mode) != 0o700 or not stat.S_ISDIR(st.st_mode) or st.st_uid != uid:
            return False
    return True


def state_dir() -> str | None:
    uid = os.getuid()
    base = tempfile.gettempdir()
    path = os.path.join(base, f"herdr-jobs-{uid}")
    try:
        os.mkdir(path, 0o700)
    except FileExistsError:
        pass
    except OSError:
        path = None
    if path is not None and _verify_private_dir(path, uid):
        return path
    try:
        return tempfile.mkdtemp(prefix=f"herdr-jobs-{uid}-")
    except OSError:
        return None


def _read_state_file(path: str) -> dict[str, Any]:
    try:
        with open(path, "r") as f:
            data = json.load(f)
    except (OSError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def _write_state_file(directory: str, final_path: str, data: dict[str, Any]) -> None:
    tmp_path = None
    try:
        fd, tmp_path = tempfile.mkstemp(prefix="state.", suffix=".tmp", dir=directory)
        with os.fdopen(fd, "w") as tmpf:
            json.dump(data, tmpf)
        os.replace(tmp_path, final_path)
        tmp_path = None
    except OSError:
        pass
    finally:
        if tmp_path is not None:
            try:
                os.remove(tmp_path)
            except OSError:
                pass


def read_state(directory: str | None) -> dict[str, Any]:
    """Standalone one-shot read, each call taking its own lock. NOT what
    run_live uses (see run_locked_transaction, MEDIUM 5) -- kept for tooling
    and the selftest round-trip check. If the lock can't be acquired we fail
    closed (empty state) rather than reading unlocked."""
    if not directory:
        return {}
    lock_path = os.path.join(directory, "state.lock")
    state_path = os.path.join(directory, "state.json")
    try:
        lockf = open(lock_path, "a+")
    except OSError:
        return {}
    try:
        try:
            fcntl.flock(lockf.fileno(), fcntl.LOCK_SH)
        except OSError:
            return {}
        try:
            return _read_state_file(state_path)
        finally:
            try:
                fcntl.flock(lockf.fileno(), fcntl.LOCK_UN)
            except OSError:
                pass
    finally:
        lockf.close()


def write_state(directory: str | None, data: dict[str, Any]) -> None:
    """Standalone one-shot write; see read_state's docstring."""
    if not directory:
        return
    lock_path = os.path.join(directory, "state.lock")
    try:
        lockf = open(lock_path, "a+")
    except OSError:
        return
    try:
        try:
            fcntl.flock(lockf.fileno(), fcntl.LOCK_EX)
        except OSError:
            return
        try:
            _write_state_file(directory, os.path.join(directory, "state.json"), data)
        finally:
            try:
                fcntl.flock(lockf.fileno(), fcntl.LOCK_UN)
            except OSError:
                pass
    finally:
        lockf.close()


# --- MEDIUM 6: corrupt state must self-heal, never wedge --------------------
#
# json.loads accepts non-standard "NaN"/"Infinity" tokens by default, and a
# hand-edited or racily-written state file can have string-typed timestamps
# next to numeric ones. Both of those used to reach compute_live_payload
# un-normalized: NaN first_seen never satisfies `now - first_seen >= TTL` (so
# it never expires), and sorting a mix of str/float first_seen values raises
# TypeError. An unhashable `style` (e.g. a JSON list) also used to raise
# TypeError from `style not in STYLE_NAMES`, since `in` on a set hashes its
# operand. validate_state normalizes (or drops) every entry up front so a
# corrupted file self-heals into a clean state on the very next poll instead
# of repeating the same failure forever.

_MAX_STATE_STR_BYTES = 4096
_MAX_STATE_ENTRIES = 5000


def _valid_job_id_key(key: Any) -> str | None:
    if not isinstance(key, str) or not key or len(key) > 128:
        return None
    return key


def _bounded_str(value: Any, max_len: int = _MAX_STATE_STR_BYTES) -> str | None:
    if not isinstance(value, str) or len(value.encode("utf-8", "surrogatepass")) > max_len:
        return None
    return value


def _validate_seen_entry(value: Any) -> dict[str, str] | None:
    if not isinstance(value, dict):
        return None
    dlabel = _bounded_str(value.get("dir_label", ""))
    nodes = _bounded_str(value.get("nodes", ""))
    if dlabel is None or nodes is None:
        return None
    return {"dir_label": dlabel, "nodes": nodes}


def _validate_done_entry(value: Any) -> dict[str, Any] | None:
    if not isinstance(value, dict):
        return None
    style = value.get("style")
    if not isinstance(style, str) or style not in STYLE_NAMES:
        return None
    glyph = _bounded_str(value.get("glyph", ""), max_len=64)
    if not glyph:
        return None
    dlabel = _bounded_str(value.get("dir_label", ""))
    nodes = _bounded_str(value.get("nodes", ""))
    if dlabel is None or nodes is None:
        return None
    try:
        first_seen = float(value.get("first_seen"))
    except (TypeError, ValueError):
        return None
    if not math.isfinite(first_seen):
        return None
    return {"style": style, "glyph": glyph, "dir_label": dlabel, "nodes": nodes, "first_seen": first_seen}


def validate_state(raw: Any) -> dict[str, Any]:
    """Return a well-formed {"seen_ids": {...}, "done": {...}} no matter how
    malformed `raw` is -- invalid entries are dropped, never raised."""
    if not isinstance(raw, dict):
        return {"seen_ids": {}, "done": {}}

    seen_raw = raw.get("seen_ids")
    seen: dict[str, Any] = {}
    if isinstance(seen_raw, dict):
        for key, value in list(seen_raw.items())[:_MAX_STATE_ENTRIES]:
            job_id = _valid_job_id_key(key)
            entry = _validate_seen_entry(value)
            if job_id is not None and entry is not None:
                seen[job_id] = entry

    done_raw = raw.get("done")
    done: dict[str, Any] = {}
    if isinstance(done_raw, dict):
        for key, value in list(done_raw.items())[:_MAX_STATE_ENTRIES]:
            job_id = _valid_job_id_key(key)
            entry = _validate_done_entry(value)
            if job_id is not None and entry is not None:
                done[job_id] = entry

    return {"seen_ids": seen, "done": done}


def run_locked_transaction(
    directory: str | None,
    transition: Callable[[dict[str, Any]], tuple[dict[str, Any], dict[str, Any]]],
) -> dict[str, Any]:
    """Run `transition(prior_state) -> (payload, new_state)` holding ONE
    exclusive lock across the whole read -> transition -> write sequence
    (MEDIUM 5). Two concurrent instances can no longer both read the same
    state, compute independently, and overwrite each other. If the lock
    can't be acquired within a short deadline, persistence is skipped
    entirely for this poll -- we never proceed unlocked -- and `transition`
    runs against an empty prior state so a valid payload is still produced
    (linger continuity is lost for just this one poll, which is the same
    "safe degrade, not a crash" trade-off already made elsewhere here)."""
    if not directory:
        payload, _new_state = transition({})
        return payload

    lock_path = os.path.join(directory, "state.lock")
    try:
        lockf = open(lock_path, "a+")
    except OSError:
        payload, _new_state = transition({})
        return payload

    try:
        locked = False
        deadline = time.monotonic() + 2.0
        while time.monotonic() < deadline:
            try:
                fcntl.flock(lockf.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                locked = True
                break
            except OSError:
                time.sleep(0.05)
        if not locked:
            payload, _new_state = transition({})
            return payload
        try:
            state_path = os.path.join(directory, "state.json")
            raw_state = _read_state_file(state_path)
            payload, new_state = transition(raw_state)
            _write_state_file(directory, state_path, new_state)
            return payload
        finally:
            try:
                fcntl.flock(lockf.fileno(), fcntl.LOCK_UN)
            except OSError:
                pass
    finally:
        lockf.close()


# --- entry points -----------------------------------------------------------

def run_live(now: float) -> tuple[dict[str, Any], bool]:
    """Returns (payload, ok). ok=False on any failure -- caller exits
    non-zero so herdr retains+stales prior rows instead of clearing them
    (HIGH 2)."""
    try:
        rows = fetch_squeue_rows(now)
    except ProviderError as exc:
        return error_payload(str(exc)), False

    directory = state_dir()

    def transition(raw_state: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
        prior_state = validate_state(raw_state)
        return compute_live_payload(rows, prior_state, now, sacct_lookup)

    payload = run_locked_transaction(directory, transition)
    return payload, True


def run_history(_now: float) -> tuple[dict[str, Any], bool]:
    try:
        entries = fetch_sacct_history()
    except ProviderError as exc:
        return error_payload(str(exc)), False
    return compute_history_payload(entries), True


def emit_payload(payload: dict[str, Any]) -> None:
    try:
        print(_dumps(finalize_payload(payload)))
    except Exception:
        print(
            '{"version":1,"title":"JOBS","summary":"","groups":[],'
            '"notify":[{"id":"error-encode","level":"fail","text":"internal encode error"}]}'
        )


# --- selftest ---------------------------------------------------------------

def _sf(number: int, infinite: bool = False, set_: bool = True) -> dict[str, Any]:
    """Build a Slurm JSON {set,infinite,number} field for fixtures."""
    return {"set": set_, "infinite": infinite, "number": number}


_UNSET = {"set": False, "infinite": False, "number": 0}

FIXTURE_NOW = 1000.0

FIXTURE_SQUEUE_JOBS: list[dict[str, Any]] = [
    {
        "job_id": 55241874, "name": "myjob", "job_state": ["RUNNING"],
        "partition": "regular", "qos": "debug",
        "node_count": _sf(4), "time_limit": _sf(1440),
        "start_time": _sf(int(FIXTURE_NOW) - 5000),
        "end_time": _sf(int(FIXTURE_NOW) + 81375),
        "state_reason": "None",
        "current_working_directory": "/pscratch/sd/j/jdgeorga/ued",
        "standard_output": "/pscratch/sd/j/jdgeorga/ued/slurm-55241874.out",
        "standard_error": "/pscratch/sd/j/jdgeorga/ued/slurm-55241874.err",
        "array_job_id": _sf(0), "array_task_id": dict(_UNSET), "array_task_string": "",
    },
    {
        "job_id": 55241900, "name": "myjob2", "job_state": ["PENDING"],
        "partition": "regular", "qos": "debug",
        "node_count": _sf(2), "time_limit": _sf(1440),
        "start_time": dict(_UNSET), "end_time": dict(_UNSET),
        "state_reason": "Priority",
        "current_working_directory": "/pscratch/sd/j/jdgeorga/002",
        "standard_output": "slurm-%j.out",
        "standard_error": "slurm-%j.err",
        "array_job_id": _sf(0), "array_task_id": dict(_UNSET), "array_task_string": "",
    },
    {
        "job_id": 55241950, "name": "longjob", "job_state": ["RUNNING"],
        "partition": "regular", "qos": "debug",
        "node_count": _sf(8), "time_limit": _sf(0, infinite=True),
        "start_time": _sf(int(FIXTURE_NOW) - 300000), "end_time": dict(_UNSET),
        "state_reason": "None",
        "current_working_directory": "/pscratch/sd/j/jdgeorga/runs/17",
        "standard_output": "/pscratch/sd/j/jdgeorga/runs/17/slurm-55241950.out",
        "standard_error": "/pscratch/sd/j/jdgeorga/runs/17/slurm-55241950.err",
        "array_job_id": _sf(0), "array_task_id": dict(_UNSET), "array_task_string": "",
    },
]

FIXTURE_SQUEUE_JSON = json.dumps({"jobs": FIXTURE_SQUEUE_JOBS})

FIXTURE_SACCT_JOBS: list[dict[str, Any]] = [
    {"job_id": 55240003, "state": {"current": ["CANCELLED"]}, "time": {"end": 4000},
     "working_directory": "/pscratch/sd/j/jdgeorga/runs/9", "allocation_nodes": 1},
    {"job_id": 55240001, "state": {"current": ["COMPLETED"]}, "time": {"end": 3000},
     "working_directory": "/pscratch/sd/j/jdgeorga/ued", "allocation_nodes": 4},
    {"job_id": 55240002, "state": {"current": ["FAILED"]}, "time": {"end": 2000},
     "working_directory": "/pscratch/sd/j/jdgeorga/002", "allocation_nodes": 2},
    {"job_id": 55240006, "state": {"current": ["OUT_OF_MEMORY"]}, "time": {"end": 1000},
     "working_directory": "/pscratch/sd/j/jdgeorga/oom", "allocation_nodes": 1},
    {"job_id": 55240005, "state": {"current": ["RUNNING"]}, "time": {"end": 0},
     "working_directory": "/pscratch/sd/j/jdgeorga/ued", "allocation_nodes": 4},  # non-terminal: dropped
]
FIXTURE_SACCT_JSON = json.dumps({"jobs": FIXTURE_SACCT_JOBS})


def _check(label: str, condition: bool, failures: list[str]) -> None:
    if not condition:
        failures.append(label)


def run_selftest() -> int:
    failures: list[str] = []

    # --- dir_label ---
    _check("dir_label/simple", dir_label("/pscratch/sd/j/jdgeorga/ued") == "ued", failures)
    _check("dir_label/numeric-leaf", dir_label("/pscratch/sd/j/jdgeorga/002") == "jdgeorga/002", failures)
    _check("dir_label/generic-leaf", dir_label("/a/b/runs") == "b/runs", failures)
    _check("dir_label/root", dir_label("/") == "/", failures)
    _check("dir_label/empty", dir_label("") == "", failures)

    # --- clean() control-char stripping ---
    _check("clean/strips-control-chars", clean("a\nb\x00c\x7f") == "abc", failures)

    # --- format_duration ---
    _check("duration/under-24h", format_duration(81375) == "22:36:15", failures)
    _check("duration/past-24h", format_duration(86400) == "1-00:00:00", failures)
    _check("duration/clamps-negative", format_duration(-5) == "0:00:00", failures)

    # --- squeue JSON parsing ---
    rows = parse_squeue_json(FIXTURE_SQUEUE_JSON, now=FIXTURE_NOW)
    _check("squeue/row-count", len(rows) == 3, failures)
    _check("squeue/bad-json-raises", _raises(ProviderError, parse_squeue_json, "not json", FIXTURE_NOW), failures)
    _check("squeue/missing-jobs-key-raises",
           _raises(ProviderError, parse_squeue_json, json.dumps({"nope": []}), FIXTURE_NOW), failures)
    _check("squeue/non-dict-job-skipped", len(parse_squeue_json(json.dumps({"jobs": ["oops"]}), FIXTURE_NOW)) == 0,
           failures)

    # --- HIGH 1 regression: names that would break pipe-delimited parsing ---
    pipe_job = dict(FIXTURE_SQUEUE_JOBS[0], job_id=55242001, name="scan|v2")
    pipe_rows = parse_squeue_json(json.dumps({"jobs": [pipe_job]}), now=FIXTURE_NOW)
    _check("squeue/name-with-pipe-not-dropped", len(pipe_rows) == 1 and pipe_rows[0]["id"] == "55242001", failures)

    newline_job = dict(FIXTURE_SQUEUE_JOBS[0], job_id=55242002, name="scan\nv2")
    newline_rows = parse_squeue_json(json.dumps({"jobs": [newline_job]}), now=FIXTURE_NOW)
    _check("squeue/name-with-newline-not-dropped",
           len(newline_rows) == 1 and newline_rows[0]["id"] == "55242002", failures)
    _check("squeue/name-with-newline-cleaned", "\n" not in newline_rows[0]["name"], failures)

    long_workdir = "/pscratch/" + ("a" * 2048) + "/final"
    long_wd_job = dict(FIXTURE_SQUEUE_JOBS[0], job_id=55242003, current_working_directory=long_workdir)
    long_wd_rows = parse_squeue_json(json.dumps({"jobs": [long_wd_job]}), now=FIXTURE_NOW)
    _check("squeue/2kb-workdir-not-dropped", len(long_wd_rows) == 1, failures)
    _check("squeue/2kb-workdir-dir-label-ok", dir_label(long_wd_rows[0]["workdir"]) == "final", failures)

    # --- MEDIUM 4: array job expansion ---
    array_expanded_task = dict(
        FIXTURE_SQUEUE_JOBS[0], job_id=70003, name="arrjob",
        array_job_id=_sf(7000), array_task_id=_sf(3), array_task_string="",
    )
    array_rows = parse_squeue_json(json.dumps({"jobs": [array_expanded_task]}), now=FIXTURE_NOW)
    _check("array/expanded-task-id", array_rows and array_rows[0]["id"] == "7000_3", failures)
    _check("array/expanded-task-cancelable", array_rows and array_rows[0]["cancelable"], failures)

    array_collapsed = dict(
        FIXTURE_SQUEUE_JOBS[1], job_id=8000, name="arrpending", job_state=["PENDING"],
        array_job_id=_sf(8000), array_task_id=dict(_UNSET), array_task_string="5-7",
    )
    collapsed_rows = parse_squeue_json(json.dumps({"jobs": [array_collapsed]}), now=FIXTURE_NOW)
    collapsed_ids = sorted(r["id"] for r in collapsed_rows)
    _check("array/collapsed-expands-to-3", collapsed_ids == ["8000_5", "8000_6", "8000_7"], failures)
    _check("array/collapsed-all-cancelable", all(r["cancelable"] for r in collapsed_rows), failures)
    _check("array/collapsed-ids-match-cancel-regex",
           all(re.match(r"^[0-9]+(_[0-9]+)?(\+[0-9]+)?$", r["id"]) for r in collapsed_rows), failures)

    array_unparseable = dict(
        FIXTURE_SQUEUE_JOBS[1], job_id=9000, name="arrweird", job_state=["PENDING"],
        array_job_id=_sf(9000), array_task_id=dict(_UNSET), array_task_string="weird[garbage]",
    )
    weird_rows = parse_squeue_json(json.dumps({"jobs": [array_unparseable]}), now=FIXTURE_NOW)
    _check("array/unparseable-kept-visible", len(weird_rows) == 1, failures)
    _check("array/unparseable-not-cancelable", weird_rows and not weird_rows[0]["cancelable"], failures)
    weird_payload, _ = compute_live_payload(weird_rows, {}, now=FIXTURE_NOW, lookup=lambda _j: None)
    weird_group = next(g for g in weird_payload["groups"] if g["id"] == "queued")
    _check("array/unparseable-row-has-no-cancel-action", "cancel" not in weird_group["rows"][0]["actions"], failures)

    # --- live payload: initial poll (no prior state) ---
    payload, state1 = compute_live_payload(rows, {}, now=FIXTURE_NOW, lookup=lambda _jid: None)
    groups_by_id = {g["id"]: g for g in payload["groups"]}
    _check("live/has-running", "running" in groups_by_id, failures)
    _check("live/has-queued", "queued" in groups_by_id, failures)
    _check("live/no-done-yet", "done" not in groups_by_id, failures)
    _check("live/running-count", len(groups_by_id.get("running", {}).get("rows", [])) == 2, failures)
    running_row = next(r for r in groups_by_id["running"]["rows"] if r["id"] == "55241874")
    _check("live/running-cells", running_row["cells"] == ["ued", "4N", "22:36:15"], failures)
    _check("live/running-actions", running_row["actions"] == ["cancel", "tail"], failures)
    _check("live/running-log-var", running_row["vars"].get("log", "").endswith("55241874.out"), failures)
    queued_row = groups_by_id["queued"]["rows"][0]
    _check("live/queued-cells-time", queued_row["cells"][2] == "1-00:00:00", failures)
    _check("live/queued-no-tail", queued_row["actions"] == ["cancel"], failures)
    _check("live/summary", payload["summary"] == "2R  1Q", failures)
    _check("live/state-seen-ids", set(state1["seen_ids"]) == {"55241874", "55241900", "55241950"}, failures)

    # --- linger: job 55241874 vanishes, sacct reports COMPLETED ---
    def fake_lookup_completed(job_id: str) -> tuple[str, str] | None:
        if job_id == "55241874":
            return ("COMPLETED", "0:0")
        return None

    remaining_rows = [r for r in rows if r["id"] != "55241874"]
    payload2, state2 = compute_live_payload(remaining_rows, state1, now=1005.0, lookup=fake_lookup_completed)
    groups2 = {g["id"]: g for g in payload2["groups"]}
    _check("linger/done-group-present", "done" in groups2, failures)
    done_row = groups2["done"]["rows"][0]
    _check("linger/done-id", done_row["id"] == "55241874", failures)
    _check("linger/done-cells", done_row["cells"] == ["ued", "4N", "✓"], failures)
    _check("linger/done-style", done_row["style"] == "ok", failures)
    notify_ids = {n["id"] for n in payload2["notify"]}
    _check("linger/notify-id", "done-55241874" in notify_ids, failures)
    _check("linger/summary-has-glyph", "1✓" in payload2["summary"], failures)
    notify_completed = next(n for n in payload2["notify"] if n["id"] == "done-55241874")
    _check("linger/notify-level-valid", notify_completed["level"] in NOTIFY_LEVELS, failures)

    # --- LOW 8: cancelled -> style "muted", notify level "info" (not "muted") ---
    def fake_lookup_cancelled(job_id: str) -> tuple[str, str] | None:
        if job_id == "55241874":
            return ("CANCELLED by 12345", "0:0")
        return None

    payload_cancel, _ = compute_live_payload(remaining_rows, state1, now=1005.0, lookup=fake_lookup_cancelled)
    cancel_row = next(r for r in payload_cancel["groups"] if r["id"] == "done")["rows"][0]
    _check("cancel/row-style-is-muted", cancel_row["style"] == "muted", failures)
    cancel_notify = next(n for n in payload_cancel["notify"] if n["id"] == "done-55241874")
    _check("cancel/notify-level-is-info-not-muted", cancel_notify["level"] == "info", failures)
    _check("cancel/notify-level-valid", cancel_notify["level"] in NOTIFY_LEVELS, failures)

    # --- linger persists across a steady poll without calling sacct again ---
    calls = {"n": 0}

    def counting_lookup(_jid: str) -> tuple[str, str] | None:
        calls["n"] += 1
        return None

    payload3, state3 = compute_live_payload(remaining_rows, state2, now=1010.0, lookup=counting_lookup)
    _check("linger/no-sacct-on-steady-poll", calls["n"] == 0, failures)
    _check("linger/still-present-before-ttl", "done" in {g["id"] for g in payload3["groups"]}, failures)

    # --- linger ages out after 10 minutes ---
    payload4, _state4 = compute_live_payload(remaining_rows, state2, now=1005.0 + 601.0, lookup=counting_lookup)
    _check("linger/aged-out", "done" not in {g["id"] for g in payload4["groups"]}, failures)

    # --- unmapped sacct state on a vanish: dropped, not guessed ---
    def fake_lookup_unmapped(job_id: str) -> tuple[str, str] | None:
        return ("NODE_FAIL", "1:0")

    payload5, _state5 = compute_live_payload(remaining_rows, state1, now=1005.0, lookup=fake_lookup_unmapped)
    _check("linger/unmapped-state-dropped", "done" not in {g["id"] for g in payload5["groups"]}, failures)

    # --- map_state ---
    _check("map_state/completed", map_state("COMPLETED") == ("ok", "✓"), failures)
    _check("map_state/cancelled-by", map_state("CANCELLED by 12345") == ("muted", "CA"), failures)
    _check("map_state/unknown", map_state("BOOT_FAIL") is None, failures)
    _check("map_state/empty", map_state("") is None, failures)

    # --- history (JSON) ---
    hist_entries = parse_sacct_history(FIXTURE_SACCT_JSON)
    _check("history/steps-structurally-absent", all("." not in e["id"] for e in hist_entries), failures)
    _check("history/non-terminal-dropped", all(e["id"] != "55240005" for e in hist_entries), failures)
    _check("history/count", len(hist_entries) == 4, failures)
    _check("history/bad-json-raises", _raises(ProviderError, parse_sacct_history, "not json"), failures)
    hist_payload = compute_history_payload(hist_entries, limit=10)
    hist_rows = hist_payload["groups"][0]["rows"] if hist_payload["groups"] else []
    _check("history/row-count", len(hist_rows) == 4, failures)
    _check("history/sorted-desc", hist_rows[0]["id"] == "55240003", failures)  # latest End
    fallback_row = next(r for r in hist_rows if r["id"] == "55240006")
    _check("history/fallback-style", fallback_row["style"] == "muted", failures)
    _check("history/fallback-glyph", fallback_row["cells"][2] == "OU", failures)
    hist_payload_limited = compute_history_payload(hist_entries, limit=1)
    _check("history/limit", len(hist_payload_limited["groups"][0]["rows"]) == 1, failures)

    # --- state file round trip + graceful corruption handling ---
    tmp_state_dir = tempfile.mkdtemp(prefix="herdr-jobs-selftest-")
    try:
        os.chmod(tmp_state_dir, 0o700)
        write_state(tmp_state_dir, {"seen_ids": {"1": {"dir_label": "x", "nodes": "1N"}}, "done": {}})
        loaded = read_state(tmp_state_dir)
        _check("state/roundtrip", loaded.get("seen_ids", {}).get("1", {}).get("dir_label") == "x", failures)
        with open(os.path.join(tmp_state_dir, "state.json"), "w") as f:
            f.write("{not json")
        _check("state/corrupt-degrades", read_state(tmp_state_dir) == {}, failures)
        _check("state/missing-dir-degrades", read_state("/nonexistent/herdr-jobs-selftest") == {}, failures)
    finally:
        for name in os.listdir(tmp_state_dir):
            try:
                os.remove(os.path.join(tmp_state_dir, name))
            except OSError:
                pass
        try:
            os.rmdir(tmp_state_dir)
        except OSError:
            pass

    # --- MEDIUM 6: corrupt state self-heals instead of wedging ---
    _check("validate_state/non-dict-input", validate_state("garbage") == {"seen_ids": {}, "done": {}}, failures)
    _check("validate_state/non-dict-input-list", validate_state([1, 2, 3]) == {"seen_ids": {}, "done": {}}, failures)

    unhashable_style_raw = json.loads('{"done":{"1":{"first_seen":1,"style":[],"glyph":"x"}}}')
    validated_unhashable = validate_state(unhashable_style_raw)
    _check("validate_state/unhashable-style-dropped-not-raised", validated_unhashable["done"] == {}, failures)

    nan_raw = json.loads(
        '{"done":{"1":{"first_seen":NaN,"style":"ok","glyph":"X","dir_label":"d","nodes":"1N"}}}'
    )
    _check("validate_state/nan-first-seen-dropped", validate_state(nan_raw)["done"] == {}, failures)

    inf_raw = json.loads(
        '{"done":{"1":{"first_seen":Infinity,"style":"ok","glyph":"X","dir_label":"d","nodes":"1N"}}}'
    )
    _check("validate_state/infinite-first-seen-dropped", validate_state(inf_raw)["done"] == {}, failures)

    mixed_raw = {
        "done": {
            "1": {"first_seen": "1000", "style": "ok", "glyph": "X", "dir_label": "d1", "nodes": "1N"},
            "2": {"first_seen": 1005.0, "style": "fail", "glyph": "Y", "dir_label": "d2", "nodes": "2N"},
        }
    }
    validated_mixed = validate_state(mixed_raw)
    _check("validate_state/mixed-types-coerced-to-float",
           all(isinstance(e["first_seen"], float) for e in validated_mixed["done"].values()), failures)
    _check("validate_state/mixed-types-sortable-without-raising",
           not _raises(TypeError, sorted, validated_mixed["done"].items(),
                       key=lambda kv: kv[1].get("first_seen", 0)),
           failures)

    oversized_str_raw = {"seen_ids": {"1": {"dir_label": "x" * 100000, "nodes": "1N"}}}
    _check("validate_state/oversized-string-dropped", validate_state(oversized_str_raw)["seen_ids"] == {}, failures)

    # A validated corrupt state must feed compute_live_payload without raising.
    _check(
        "validate_state/feeds-compute-live-payload-safely",
        not _raises(
            Exception, compute_live_payload, rows, validate_state(unhashable_style_raw), FIXTURE_NOW,
            lambda _j: None,
        ),
        failures,
    )

    # --- row cap ---
    many_groups = [{"id": "g", "label": "G", "rows": [{"id": str(i)} for i in range(5)]}]
    _check("cap/truncates", len(cap_rows(many_groups, 3)[0]["rows"]) == 3, failures)

    # --- MEDIUM 3: truncate_utf8 respects char boundaries ---
    multibyte = "é" * 600  # 2 bytes each = 1200 bytes, over the 1024 budget
    truncated = truncate_utf8(multibyte, 1024)
    _check("truncate_utf8/valid-utf8", _roundtrips_utf8(truncated), failures)
    _check("truncate_utf8/under-budget", len(truncated.encode("utf-8")) <= 1024, failures)
    _check("truncate_utf8/short-string-unchanged", truncate_utf8("short", 1024) == "short", failures)

    # --- MEDIUM 3: oversized payload is shrunk under the consumer's cap,
    # dropping Done then Queued while keeping Running. Running/Queued use
    # realistically small rows (so 100 of them together are trivially under
    # budget); Done alone is made deliberately huge so the size cap can only
    # be satisfied by dropping Done rows, not by touching Running/Queued.
    def _small_row(i: int) -> dict[str, Any]:
        return {
            "id": str(i),
            "cells": ["proj", "4N", "1:00:00"],
            "style": "normal",
            "vars": {"dir": "/pscratch/sd/j/user/proj"},
            "actions": ["cancel", "tail"],
        }

    def _big_row(i: int, dlabel_len: int = 1200) -> dict[str, Any]:
        return {
            "id": str(i),
            "cells": ["d" * dlabel_len, "4N", "1:00:00"],
            "style": "normal",
            "vars": {"dir": "/x" * 800, "log": "/y" * 800},
            "actions": ["cancel", "tail"],
        }

    oversized_payload = {
        "version": 1, "title": "JOBS", "summary": "50R  50Q  3000 done",
        "groups": [
            {"id": "running", "label": "Running", "rows": [_small_row(i) for i in range(50)]},
            {"id": "queued", "label": "Queued", "rows": [_small_row(1000 + i) for i in range(50)]},
            {"id": "done", "label": "Done", "rows": [_big_row(2000 + i) for i in range(3000)]},
        ],
        "notify": [],
    }
    finalized = finalize_payload(oversized_payload)
    serialized = _dumps(finalized)
    _check("oversized/fits-256kib", _byte_len(serialized) <= 256 * 1024, failures)
    finalized_groups = {g["id"]: g for g in finalized["groups"]}
    _check("oversized/running-fully-preserved", len(finalized_groups.get("running", {}).get("rows", [])) == 50,
           failures)
    _check("oversized/queued-fully-preserved", len(finalized_groups.get("queued", {}).get("rows", [])) == 50,
           failures)
    _check("oversized/done-dropped-first", len(finalized_groups.get("done", {}).get("rows", [])) < 3000, failures)
    all_strings_ok = True
    for group in finalized["groups"]:
        for row in group["rows"]:
            for cell in row.get("cells", []):
                if len(cell.encode("utf-8")) > 1024:
                    all_strings_ok = False
            for v in row.get("vars", {}).values():
                if len(v.encode("utf-8")) > 1024:
                    all_strings_ok = False
    _check("oversized/all-strings-under-1kib", all_strings_ok, failures)

    # --- HIGH 2: failure paths still emit exactly one valid payload, and
    # ok=False propagates so main() can exit non-zero ---
    # Selftest must never touch the real per-user state directory (that could
    # be the live herdr session's own state) -- patch state_dir() to a
    # private tmpdir for the duration of these run_live() calls.
    orig_fetch_squeue = globals()["fetch_squeue_rows"]
    orig_fetch_sacct = globals()["fetch_sacct_history"]
    orig_state_dir = globals()["state_dir"]
    selftest_state_dir = tempfile.mkdtemp(prefix="herdr-jobs-selftest-runlive-")
    os.chmod(selftest_state_dir, 0o700)
    globals()["state_dir"] = lambda: selftest_state_dir
    try:
        def boom_squeue(_now: float) -> list[dict[str, Any]]:
            raise ProviderError("squeue: [Errno 2] No such file or directory")

        globals()["fetch_squeue_rows"] = boom_squeue
        fail_payload, fail_ok = run_live(FIXTURE_NOW)
        _check("failure/live-ok-is-false", fail_ok is False, failures)
        _check("failure/live-payload-is-valid-json", _roundtrips_json(fail_payload), failures)
        _check("failure/live-payload-has-fail-notify",
               any(n.get("level") == "fail" for n in fail_payload.get("notify", [])), failures)

        def boom_sacct() -> list[dict[str, Any]]:
            raise ProviderError("sacct: [Errno 2] No such file or directory")

        globals()["fetch_sacct_history"] = boom_sacct
        fail_hist_payload, fail_hist_ok = run_history(FIXTURE_NOW)
        _check("failure/history-ok-is-false", fail_hist_ok is False, failures)
        _check("failure/history-payload-is-valid-json", _roundtrips_json(fail_hist_payload), failures)

        # --- a genuinely empty (not failed) poll still reports ok=True ---
        globals()["fetch_squeue_rows"] = lambda _now: []
        empty_payload, empty_ok = run_live(FIXTURE_NOW)
        _check("empty-poll/ok-is-true", empty_ok is True, failures)
        _check("empty-poll/no-groups", empty_payload["groups"] == [], failures)
    finally:
        globals()["fetch_squeue_rows"] = orig_fetch_squeue
        globals()["fetch_sacct_history"] = orig_fetch_sacct
        globals()["state_dir"] = orig_state_dir
        for name in os.listdir(selftest_state_dir):
            try:
                os.remove(os.path.join(selftest_state_dir, name))
            except OSError:
                pass
        try:
            os.rmdir(selftest_state_dir)
        except OSError:
            pass

    if failures:
        print(f"SELFTEST FAILED ({len(failures)}): {', '.join(failures)}", file=sys.stderr)
        return 1
    print("SELFTEST OK", file=sys.stderr)
    return 0


def _raises(exc_type: type[BaseException], fn: Callable[..., Any], *args: Any, **kwargs: Any) -> bool:
    try:
        fn(*args, **kwargs)
    except exc_type:
        return True
    except Exception:
        return False
    return False


def _roundtrips_utf8(s: str) -> bool:
    try:
        s.encode("utf-8").decode("utf-8")
        return True
    except UnicodeError:
        return False


def _roundtrips_json(payload: dict[str, Any]) -> bool:
    try:
        json.loads(_dumps(finalize_payload(payload)))
        return True
    except (TypeError, ValueError):
        return False


# --- CLI ---------------------------------------------------------------

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--mode", choices=["live", "history"])
    parser.add_argument("--selftest", action="store_true")
    try:
        args, _unknown = parser.parse_known_args(argv)
    except SystemExit:
        emit_payload(error_payload("invalid arguments"))
        return 1

    if args.selftest:
        return run_selftest()

    now = time.time()
    ok = True
    try:
        if args.mode == "live":
            payload, ok = run_live(now)
        elif args.mode == "history":
            payload, ok = run_history(now)
        else:
            payload = error_payload("missing or invalid --mode (expected live|history)")
            ok = False
    except Exception as exc:  # last-resort guard: never let a traceback reach stdout
        payload = error_payload(f"internal error: {exc}")
        ok = False

    emit_payload(payload)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
