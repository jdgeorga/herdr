# Herdr SLURM jobs sidebar — design

**Date:** 2026-08-06
**Status:** approved, ready for implementation planning
**Base:** herdr v0.8.0 (`69a07fd`), Apache-2.0, github.com/herdrdev/herdr
**Target platform:** NERSC Perlmutter login nodes (Linux, SLURM 25.11.6)

## Problem

Herdr's sidebar shows agents and spaces. On an HPC login node the other thing
worth watching continuously is the batch queue. Today that means a separate
`watch squeue` pane, which costs a pane and carries no connection to the
workspaces the jobs came from.

We want a third sidebar section listing the user's SLURM jobs, grouped into
Running / Queued / recently-Done, refreshing every ~10 seconds, with cancel and
tail-log actions.

## Decisions already settled

| Question | Decision |
| --- | --- |
| Job scope | All the user's jobs — `squeue -u $USER`, not just jobs launched from Herdr |
| Mechanism | Generic external-command section in Rust; SLURM logic in a provider script |
| Distribution | Personal fork; upstream only after a maintainer Discussion (see Constraints) |
| Cluster scope | Perlmutter only. No SSH, no multi-site |
| Interaction | Mouse-only in v1. Cancel + tail-log |
| Row density | One line per job |
| Placement | Bottom of sidebar, collapsed to a summary line by default |
| Finished jobs | Linger 10 minutes as Done/Failed, green/red, with a notification |
| History | A `[live|HIST]` toggle swaps the same box to the last 10 jobs |
| Notifications | In-TUI toasts only |
| Toolchain | NERSC `rust/stable` module with redirected `RUSTUP_HOME`/`CARGO_HOME` |
| Install | Replace `~/.local/bin/herdr`, disable the update check |

## Architecture

Two components, split along the axis of what changes often.

### 1. Generic list section (Rust, in the fork)

A third sidebar section that runs a configured command on an interval, parses
JSON from its stdout, renders grouped rows, and dispatches configured actions.
It contains no SLURM knowledge — no mention of squeue, job IDs, or node counts.

### 2. SLURM provider (Python, in `~/.config/herdr/scripts/`)

Owns everything SLURM-specific: the squeue invocation and its field layout, the
10-minute linger buffer for finished jobs, the sacct exit-code lookup, history
mode, and the decision of when to emit a notification.

Interpreter: bare `python3` on NERSC login nodes is **3.6.15**. The provider is
invoked as `/usr/bin/python3.11` explicitly. Do not rely on `python3` resolving
to anything modern, and do not assume the interpreter is the same one used by
other tooling on the node. Alternatively the script may be written 3.6-clean —
no f-string `=`, no dataclasses, no walrus — but pinning 3.11 is preferred since
the path is stable and the provider is ours.

The split matters because every part the user will keep adjusting — grouping,
linger duration, which fields show, what counts as a failure — lives in the
script. Adjusting the display costs a 10-second poll, not a rebuild of 266
crates.

## Provider protocol

The command is invoked fresh on each poll and writes a single JSON object to
stdout. Non-zero exit or unparseable output surfaces as an error row in the
section and leaves the previous rows in place.

```json
{
  "version": 1,
  "title": "JOBS",
  "summary": "2R  1Q  1✓",
  "groups": [
    {
      "id": "running",
      "label": "Running",
      "rows": [
        {
          "id": "55241874",
          "cells": ["ued", "4N", "1:23:45"],
          "style": "normal",
          "vars": {
            "log": "/pscratch/sd/j/jdgeorga/ued/slurm-55241874.out",
            "dir": "/pscratch/sd/j/jdgeorga/ued"
          },
          "actions": ["cancel", "tail"]
        }
      ]
    }
  ],
  "notify": [{ "level": "ok", "text": "55241874 (ued) finished" }]
}
```

Field notes:

- `id` — stable row identity. Substituted into action commands as `{id}`.
- `cells` — positional, matched against the configured `columns`. Excess cells
  are dropped; missing cells render blank.
- `style` — a *name*, not a color. Resolved against `[ui.sidebar.list.styles]`
  so rows follow the active theme instead of hardcoding hex in Python.
- `vars` — arbitrary string map, available to action templates as `{name}`.
- `actions` — IDs of configured actions enabled for this row. An action not
  listed renders disabled. This is how tail-log is suppressed on pending jobs.
- `notify` — zero or more toasts to raise this poll. The provider decides when;
  Rust just displays them. `level` is one of `ok`, `warn`, `fail`, `info`.

`summary` renders on the collapsed header line, so the box is useful without
being expanded.

### Substitution vocabulary

Action `command` and `confirm` templates accept:

| Token | Resolves to |
| --- | --- |
| `{id}` | the row's `id` |
| `{cell0}`, `{cell1}`, … | the row's `cells` by index |
| `{<name>}` | the matching key in the row's `vars` |
| `{mode}` | the section's current mode (in `command` only) |

An unresolved token is a hard error: the action is refused and a toast explains
which token was missing, rather than executing a command with a literal `{log}`
in its argv.

## Configuration

```toml
[ui.sidebar.list]
enabled = true
placement = "bottom"          # "top" | "bottom"
collapsed = true              # initial state; runtime changes persist
refresh_seconds = 10
max_visible_rows = 12
command = ["/usr/bin/python3.11", "~/.config/herdr/scripts/herdr-jobs.py", "--mode", "{mode}"]
modes = ["live", "history"]   # first is default; header toggle cycles
timeout_seconds = 5

columns = [
  { width = "fill", align = "left"  },   # directory, truncates with …
  { width = 3,      align = "right" },   # 4N / 16N
  { width = 7,      align = "right" },   # 1:23:45
]

[ui.sidebar.list.styles]
ok     = "#b8bb26"
fail   = "#fb4934"
warn   = "#fabd2f"
muted  = "#928374"
normal = "#ebdbb2"

[[ui.sidebar.list.actions]]
id = "cancel"
label = "cancel"
command = ["scancel", "{id}"]
confirm = "Cancel job {id} ({cell0})?"
target = "background"         # default when omitted

[[ui.sidebar.list.actions]]
id = "tail"
label = "tail"
command = ["tail", "-f", "{log}"]
target = "overlay"            # "overlay" | "background"
cwd = "{dir}"
```

Action targets in v1: `overlay` runs the command in a full-screen overlay via
`spawn_overlay_argv_command`; `background` runs it detached and reports the
result as a toast (this is what `cancel` uses). Opening a persistent split pane
is deferred — it needs pane-tree plumbing that the overlay path avoids.

Exactly one list section in v1. A map of named sections is a natural later
extension but is not built now.

## Rendering and layout

At the default sidebar width of 26 columns (`default_sidebar_width: 26`,
`src/app/state.rs:1866`; bounds 18–36, drag-resizable) about 24 columns are
usable. The layout is a fill column for the directory plus two fixed
right-aligned columns:

```
┌────────────────────────┐
│ SPACES                 │
│ ▸ phd_research         │
├────────────────────────┤
│ AGENTS                 │
│⠹ ✳ SETUP CONFIG · t6   │
│  working · herdr-jobs  │
├────────────────────────┤
│ ▸ JOBS   2R  1Q  1✓    │   ← collapsed default
└────────────────────────┘
```

Expanded:

```
│ ▼ JOBS      [live|HIST]│
│ ▼ Running (2)          │
│ ued          4N 1:23:45│
│ xct_xct     16N 0:41:02│
│ ▼ Queued (1)           │
│ scf_moire    8N 6:00:00│
│ ▼ Done (1)             │
│ tmd_conv     2N      ✓ │
```

History mode, same box:

```
│ ▼ JOBS      [live|HIST]│
│ tmd_conv     2N   ✓ 12m│
│ xct_xct     16N   ✗ 2h │
│ relax_bp     1N  TO 5h │
```

### The layout carve

The existing split functions are hardcoded two-way:

- `sidebar_section_heights(total_h, split_ratio) -> (u16, u16)` — `src/ui/sidebar.rs:42`
- `expanded_sidebar_sections(area, split_ratio) -> (Rect, Rect)` — `src/ui/sidebar.rs:59`
- `collapsed_sidebar_sections(area) -> (Rect, Option<u16>, Rect)` — `src/ui/sidebar.rs:728`

Rather than generalizing these to N sections — which would touch every call
site that destructures their return tuples — `render_sidebar` carves the list
section's rect off the bottom of the content area **first**, then passes the
shrunk remainder into the existing functions with their signatures unchanged.
The input dispatcher in `src/app/input/sidebar.rs` derives its hit-test rect
from the same helper so the two cannot drift.

Note the collapsed path returns a **3-tuple**, not a pair, and needs its own
carve; do not assume the two paths are symmetric.

Reuse rather than reimplement:

- `resolved_token_spans` (`src/ui/sidebar.rs:1003`) for cell truncation. It is
  currently private to the module; widen to `pub(crate)` or keep the list
  renderer in the same module.
- `render_scrollbar` (`src/ui/scrollbar.rs:135`) for overflow past
  `max_visible_rows`.
- The workspace-group chevron/collapse code (`src/ui/sidebar.rs:1280-1379`) for
  the section header and per-group headers.

## Polling

The poller is **server-side**. `ClientState` (`src/client/mod.rs`) is a blit
decoder and host-terminal quirk state machine with no application state;
`HeadlessServer` owns the single `app::App` (`src/server/headless.rs:4709`,
`:4816`, `:4962`). Job rows are server-owned, TUI-only-consumed state — the same
tier as the existing git-status cache — so they extend the internal `AppEvent`
channel and add no public API surface.

`src/app/jobs_refresh.rs` mirrors `src/app/git_refresh.rs`
(`start_git_status_refresh_if_due`, `git_refresh.rs:36`): a `next_list_poll:
Option<Instant>` deadline plus an in-flight guard, spawning a thread that runs
the configured command and sends `AppEvent::ListSectionPolled` on completion.

**Both event loops must be wired.** `App::run` (`src/app/runtime.rs`) and
`HeadlessServer::run` (`src/server/headless.rs`) are independently implemented,
each with its own deadline builder and scheduled-task handler. Headless is the
default launch path; the monolithic `App` loop is used only in `--no-session`
and test mode. Wiring only one ships a timer that never fires in normal use.

Mutating state does not itself trigger a repaint. The event handler in
`src/app/api.rs` must call `render_dirty.request_generic()` and
`render_notify.notify_one()` explicitly.

## Actions

**Cancel** reuses the existing confirmation machinery. Each confirmation type in
this codebase is its own `Mode` variant plus state plus render function — there
is no generic confirm-with-payload. Add `Mode::ConfirmListAction` alongside
`Mode::ConfirmRemoveWorktree` (`src/app/state.rs:831`, dispatched at
`src/ui.rs:456`, handled at `src/app/mod.rs:1823`), carrying the action ID and
row ID. Confirmation is a single-key y/n, matching `CONFIRM_CLOSE_ACTIONS`;
typed confirmation is reserved for genuinely destructive operations and scancel
does not clear that bar.

On accept, the command runs in a thread via the non-interactive process helper,
then sets `next_list_poll = Some(Instant::now())` for an immediate refresh and
raises an in-TUI toast with the result.

**Tail-log** reuses `spawn_overlay_argv_command`
(`src/app/input/navigate.rs:1045`) — the same path `$EDITOR`-on-scrollback
already uses — with `cwd` set to the row's `dir` var. No new pane machinery.

**Mode toggle** clicking `[live|HIST]` in the header cycles `modes` and triggers
an immediate re-poll with the new `{mode}` substitution.

## SLURM provider behaviour

Live mode:

```
squeue -u $USER -h -o '%i|%j|%t|%P|%q|%D|%M|%L|%l|%S|%r|%Z|%o|%e'
```

Measured at ~35 ms on Perlmutter, which is comfortable at a 10-second cadence.
Field mapping: `%i` job ID, `%j` name, `%t` state, `%P` partition, `%q` QOS,
`%D` node count, `%M` elapsed, `%L` time left, `%l` time limit, `%S` start time,
`%r` pending reason, `%Z` work directory, `%o`/`%e` stdout/stderr paths.

Grouping is by `%t`: `R` → Running (display `%L`), `PD` → Queued (display `%l`).

Time fields are pre-formatted strings, not seconds. They gain a leading `D-`
once past 24 hours and can be the literal `UNLIMITED` or `N/A`. The parser must
handle all three rather than assuming `HH:MM:SS`.

The directory label is the last one or two components of `%Z`. No `~`-collapse
helper for subpaths exists in the codebase — only exact-`$HOME` match and
basename extraction — so the provider does this itself.

### Linger and notifications

Each poll's job IDs are compared against the previous poll's, persisted in
`${TMPDIR:-/tmp}/herdr-jobs-$USER.json`. Node-local by design: `$HOME` is shared
across NERSC login nodes and this is per-node runtime state.

IDs that disappeared get one `sacct -j <id> --format=State,ExitCode -n -P`
lookup and move into a Done group with a style and glyph by final state —
`COMPLETED` → green ✓, `FAILED` → red ✗, `TIMEOUT` → `TO`, `CANCELLED` → `CA` —
plus a `notify` entry. They age out after 10 minutes.

sacct runs only on transitions and in history mode, never on the steady-state
poll. It is roughly 5× the cost of squeue and expands into per-step rows.

History mode returns the last 10 finished jobs from sacct with the same row
schema, so the renderer needs no special case.

## Testing

- `parse_provider_output` — a pure function over a JSON string. Covers valid
  payloads, unknown fields, missing groups, wrong version, and malformed JSON.
  No cluster required.
- Config defaults, mirroring the existing
  `defaults_match_the_compact_agent_and_existing_space_layouts` test.
- The layout carve: given a sidebar rect and a jobs height, the remainder passed
  to `expanded_sidebar_sections` matches expectation, and the carve degrades
  sanely at tiny heights (compare `expanded_sidebar_sections_handle_tiny_heights`,
  `src/ui/sidebar.rs:2506`).
- Hit-testing: a click at a given row maps to the expected row ID, and the
  render rect and hit-test rect agree.
- Provider script: squeue-output parsing and linger-expiry logic against
  recorded fixtures.

## Constraints and risks

**Upstreaming is gated.** `CONTRIBUTING.md` auto-closes unsolicited
implementation PRs; only accounts on an `APPROVED_CONTRIBUTORS` list may submit
them, and a change this size needs a maintainer-approved Discussion first. Plan
the fork as permanent. If upstreaming is later attempted, the generic list
section is the submittable artifact — the SLURM provider is not.

**Rebase surface.** The patch concentrates in `src/ui/sidebar.rs`,
`src/config/sidebar.rs`, `src/app/mod.rs`, and `src/server/headless.rs` — among
the most frequently changed files in the project. Keep the carve helper and the
list renderer in new files where possible to reduce conflict area.

**`herdr update` overwrites by path.** It atomically replaces whatever sits at
`env::current_exe()`. Replacing `~/.local/bin/herdr` requires disabling the
update check, and the stock binary should be preserved as `herdr-stock` first —
it is the only fallback if a rebase breaks mid-week.

**Toolchain.** Two prerequisites, both resolved; see `build-env.sh` at the repo
root, which every build must source.

1. No rustc/cargo on this machine. `module load rust/stable` is rustup-based and
   resolves to 1.96.1, matching `rust-toolchain.toml`, but the module's own
   `RUSTUP_HOME` points at read-only shared software and errors on use.
   Redirect `RUSTUP_HOME`, `CARGO_HOME`, and `CARGO_TARGET_DIR` to pscratch.
   `$HOME` is at 73% of a 40 GiB quota and must not hold build artifacts.
2. **`build.rs` requires zig.** herdr vendors libghostty-vt and `build.rs:63-80`
   shells out to `zig build -Demit-lib-vt`, panicking with a bare `NotFound` if
   zig is absent. `vendor/libghostty-vt/build.zig.zon` declares
   `minimum_zig_version = "0.15.2"`. There is no NERSC zig module. Installed
   0.15.2 from the official tarball to `$PSCRATCH/tools/zig-0.15.2` and exported
   `ZIG` plus `ZIG_GLOBAL_CACHE_DIR`/`ZIG_LOCAL_CACHE_DIR` (which otherwise
   default under `$HOME`). Pinned to 0.15.2 rather than 0.16/0.17 because zig
   breaks builds across minor versions.

`build.rs` also honours `LIBGHOSTTY_VT_ZIG_SYSTEM_DIR` for a prebuilt
libghostty-vt, which would avoid zig entirely — not pursued, since a pinned zig
tarball is simpler than sourcing a matching prebuilt library.

**System notifications do not work here.** Herdr's system toast path shells out
to `notify-send` gated on `$DISPLAY`/`$WAYLAND_DISPLAY`, a silent no-op on an
SSH login-node session. All notifications use the in-TUI delivery path.

**Config errors are coarse.** No struct in the `SidebarConfig` family uses
`deny_unknown_fields`, and a deserialize error anywhere in `config.toml` falls
back to a full default config. A malformed `[ui.sidebar.list]` table silently
resets unrelated settings. Pre-existing, but a new section widens the exposure.

**Tail-log on pending jobs.** `%o`/`%e` return unresolved templates such as
`slurm-%j.out` before a job starts. The provider omits `tail` from `actions` for
pending rows, rendering it disabled. Resolving them would need
`scontrol show job -d <id>`, deferred.

## Explicitly out of scope for v1

- Keyboard navigation. No focus concept exists for a sidebar list not backed by
  a pane — Agents and Spaces selection both piggyback on real pane/workspace
  focus. Inventing a focus zone is the highest-variance part of the estimate and
  is deferred to v2.
- Multiple named list sections.
- Any cluster other than Perlmutter.
- Job submission from the sidebar.

## Effort

Roughly 900–1400 lines of Rust across ~15 files, plus ~250 lines of Python.
Every subsystem has a close template in-tree; the layout carve and the hit-test
alignment are the parts with no precedent to copy.
