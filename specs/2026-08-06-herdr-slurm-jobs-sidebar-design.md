# Herdr SLURM jobs sidebar — design

**Date:** 2026-08-06 (rev 2, after adversarial review)
**Status:** approved, ready for implementation
**Base:** herdr v0.8.0 (`69a07fd`), Apache-2.0, github.com/herdrdev/herdr
**Target:** NERSC Perlmutter login nodes (Linux, SLURM 25.11.6)

## Problem

Herdr's sidebar shows agents and spaces. On an HPC login node the other thing
worth watching continuously is the batch queue. Today that means a separate
`watch squeue` pane, which costs a pane and carries no connection to the
workspaces the jobs came from.

Add a third sidebar section listing the user's SLURM jobs, grouped Running /
Queued / recently-Done, refreshing every ~10 seconds, with cancel and tail-log
actions.

## Revision note

Rev 1 was reviewed against the source by GPT-5.6 Sol and rejected as
implementation-ready. Its substantive findings are incorporated here. The
corrections that changed the design, not just the prose:

- **The "carve inside `render_sidebar`" plan was wrong.** `compute_view`
  (`src/ui.rs:246`) normalizes scroll against the *full* sidebar rect, and
  `ViewState` stores `workspace_card_areas` (`src/app/state.rs:803`) consumed by
  both rendering and hit-testing. Carving in one place only would silently
  desync geometry. Replaced with a single computed `SidebarLayout`.
- **The sidebar toggle already lives at the bottom** of the sidebar
  (`expanded_sidebar_toggle_rect`, `src/ui/sidebar.rs:1552`), colliding with
  bottom-placed Jobs. Now explicitly reserved.
- **`Mode::ConfirmRemoveWorktree` is a unit variant**; its payload lives in a
  separate `WorktreeRemoveState` (`src/app/state.rs:663`). Rev 1 described it as
  payload-carrying.
- **`resolved_token_spans` is not a generic truncation helper** — it is bound to
  sidebar `ResolvedTokenKind` semantics. Use `truncate_end` (`src/ui/text.rs:3`)
  and compute columns independently.
- **`App::run` is in `src/app/mod.rs:903`**, not `src/app/runtime.rs`, and the
  two loops share deadline logic rather than duplicating it.
- **tokio has no `process` feature** (`Cargo.toml:41`). Use `std::process`.
- **`AppState.toast` is a single slot** (`src/app/state.rs:1484`) with three
  `ToastKind` variants. Provider notifications need a queue.
- **`~` is not expanded under argv execution.** Rev 1's example config was
  unrunnable.
- Rev 1's claim that a bad config section silently resets unrelated settings is
  outdated for live reload (`src/config/io.rs:218`).

Sol also recommended cutting linger, history, notifications and provider
styling from v1. Those are kept, because in this architecture they are almost
entirely *provider-side*: the linger buffer, the sacct exit lookup and the
history query are Python, and the Rust cost is a style-name lookup table, a
bounded notify queue, and a mode string. The review's estimate of their cost
assumed they were Rust features. Its correctness findings are adopted in full;
its scope recommendation is not.

## Decisions

| Question | Decision |
| --- | --- |
| Job scope | All the user's jobs — `squeue -u $USER` |
| Mechanism | Generic external-command section in Rust; SLURM logic in a provider script |
| Distribution | Personal fork; upstream only after a maintainer Discussion |
| Cluster | Perlmutter only |
| Interaction | Mouse-only in v1. Left click selects, right click opens a context menu |
| Row density | One line per job |
| Placement | Bottom of sidebar, above the toggle row, collapsed by default |
| Finished jobs | Linger 10 minutes as Done/Failed, green/red, with a notification |
| History | A header toggle swaps the box to the last 10 jobs |
| Notifications | In-TUI toasts only |
| Headless | Polling pauses when no TUI client is attached |

## Architecture

Two components, split along the axis of what changes often.

**Generic list section (Rust).** Runs a configured command on an interval,
parses JSON, renders grouped rows, dispatches configured actions. Contains no
SLURM knowledge.

**SLURM provider (Python).** Owns the squeue invocation, the 10-minute linger
buffer, the sacct exit-code lookup, history mode, and when to notify.

Interpreter: bare `python3` on NERSC login nodes is **3.6.15**. Invoke
`/usr/bin/python3.11` (verified 3.11.15) by absolute path.

## Layout — one source of truth

This is the central correctness requirement. All sidebar geometry is computed
**once**, in `compute_view` (`src/ui.rs`), into a `SidebarLayout` stored on
`ViewState`. Rendering and hit-testing both read it. Neither recomputes.

```rust
pub struct SidebarLayout {
    pub jobs: Rect,                 // empty when hidden
    pub jobs_collapsed: bool,
    pub spaces: Rect,
    pub agents: Rect,
    pub section_divider_y: Option<u16>,
    pub toggle: Rect,
    pub jobs_rows: Vec<JobRowHit>,  // per visible row: rect + row id + group id
    pub jobs_scrollbar: Option<Rect>,
    pub jobs_header_hits: JobsHeaderHits, // chevron rect, mode-toggle rect
}
```

Allocation order, top to bottom, and the reason it must be this order:

1. Reserve the rightmost column (the sidebar's vertical border), as
   `expanded_sidebar_sections` already does (`src/ui/sidebar.rs:60`).
2. Reserve the bottom toggle row.
3. Allocate Jobs from the bottom of what remains.
4. Apply the existing Spaces/Agents `split_ratio` to the **remainder**.

The divider rect and the drag-to-ratio conversion must both be computed against
that same remainder. `set_sidebar_section_split` currently converts a dragged
row into a ratio using the full sidebar height (`src/app/input/sidebar.rs:290`);
if rendering applies the ratio post-carve while dragging computes it pre-carve,
the divider jumps. Carving *after* applying the ratio is also wrong — it steals
rows from Agents and breaks the existing three-row minimum.

### Degradation policy

Jobs never starves the existing sections. Given usable height `H` (after border
and toggle), with Spaces and Agents each requiring 3 rows:

| Available | Jobs gets |
| --- | --- |
| `H - 6 >= wanted` | `wanted` = min(content rows, `max_visible_rows`) |
| `1 <= H - 6 < wanted` | `H - 6`, with a scrollbar |
| `H - 6 < 1` | one row, collapsed header only |
| `H < 7` | hidden entirely; layout falls through to today's two-way behaviour |

`max_visible_rows` counts **all** rows the section draws — section header, group
headers, and job rows alike — so the section's total height is predictable from
config alone.

Collapsed mode is asymmetric and needs its own path.
`collapsed_sidebar_sections` returns `(Rect, Option<u16>, Rect)`
(`src/ui/sidebar.rs:728`), ignores `split_ratio`, uses a fixed half split, and
drops Agents entirely below seven rows. Do not assume symmetry with the expanded
path; write and test the two carves separately.

### Call sites to update

Every consumer of the old two-way geometry moves to `SidebarLayout`:

- `src/ui.rs:248` — view computation and agent scroll clamping
- `src/ui/sidebar.rs:439` — workspace-list geometry
- `src/ui/sidebar.rs:658` — `compute_workspace_list_areas`
- `src/ui/sidebar.rs:775` — collapsed rendering
- `src/ui/sidebar.rs:996` — sidebar rendering
- `src/app/input/sidebar.rs:22` — agent-panel hit geometry
- `src/app/input/sidebar.rs:275` — divider hit test
- `src/app/input/sidebar.rs:324`, `:341` — collapsed Spaces/Agents hit testing
- `src/app/input/sidebar.rs:473` — agent-sort click target
- `src/app/actions.rs:1562` — ensure-agent-visible scrolling

Tests calling the old helpers directly (`src/ui/sidebar.rs:1665`, `:1716`,
`:1888`, `:1918`, `:2165`, `:2232`, `:2266`, `:2294`, `:2335`, `:2385`, `:2507`;
`src/ui.rs:1071`; `src/app/input/sidebar.rs:855`, `:901`, `:1080`, `:1125`) keep
passing unchanged when Jobs is disabled — that is the regression bar. The
existing two-way helpers stay, and `SidebarLayout` calls them on the remainder.

## Mouse contract

Three mouse paths currently assume every sidebar click belongs to Spaces or
Agents and must each intercept Jobs first:

- `src/app/input/mouse.rs:519` — click routing
- `src/app/input/mouse.rs:967` — wheel routing
- `src/app/input/mouse.rs:1019` — right click as workspace click

| Gesture | Target | Result |
| --- | --- | --- |
| Left click | section header chevron | collapse / expand, persisted |
| Left click | mode toggle in header | cycle `live` ↔ `history`, immediate re-poll |
| Left click | group header chevron | collapse / expand that group |
| Left click | job row | select it (selection is visual only) |
| Right click | job row | open context menu |
| Wheel | anywhere in jobs rect | scroll jobs |

Actions live in a **context menu**, not inline buttons — 24 columns has no room
for them. Extend the existing `ContextMenuKind` (`src/app/state.rs:1230`), whose
`items()` returns a static slice per variant, with:

```rust
ContextMenuKind::Job { row_id, can_tail }
```

returning `["Cancel job", "Tail log", "Copy job ID"]`, with "Tail log" omitted
when `can_tail` is false. This reuses the whole existing menu render, keyboard,
and mouse stack rather than inventing hit targets inside a 24-column row.

Selection is identified by row **id**, not index, so it survives a poll that
reorders rows. A selection whose id vanishes is cleared.

Scrolling needs more than `render_scrollbar` (`src/ui/scrollbar.rs:135`), which
only draws. Scroll metrics, track and thumb rects, click and drag handling all
follow the pattern Spaces and Agents each implement separately
(`src/app/input/sidebar.rs:26`, `:105`).

## Polling

The poller is **server-side**. `ClientState` is a blit decoder with no
application state; `HeadlessServer` owns the single `app::App`
(`src/server/headless.rs:4709`).

`start_git_status_refresh_if_due` (`src/app/git_refresh.rs:36`) is the
scheduling template: deadline check, in-flight flag set before spawn, blocking
work on a detached thread, result delivered by internal event
(`src/app/git_refresh.rs:67`), deadline suppressed while in flight (`:95`).
It is **not** a subprocess template — it provides no timeout, no child handle,
no cancellation, no output limits.

Both scheduled-task handlers need the trigger, because they are duplicated:
`src/app/runtime.rs:282` (monolithic) and `src/server/headless.rs:4327`
(headless, the default launch path). The *deadline builder* is shared
(`src/app/runtime.rs:560`) and needs one change, not two.

Headless git refresh is gated on an attached app client
(`src/server/headless.rs:4397`). Jobs polling follows the same gate: **no TUI
client attached, no polling.** This is a login node, and nobody sees a toast
raised into an unattached session. Nothing is lost — the provider owns the
linger state on disk, so it reconstructs what changed on the next poll after
reattach.

### Subprocess requirements

Use `std::process::Command`; tokio's `process` feature is not enabled and this
design does not add it.

- **Single-flight.** One poll outstanding at a time. A poll that overruns the
  interval stretches the cadence rather than overlapping.
- **Timeout** of `timeout_seconds` (default 5), strictly less than the 10s
  cadence.
- **Process group.** Spawn with `process_group(0)` and kill the whole group.
  Killing only Python orphans `squeue`/`sacct`.
- **Reap.** Always `wait()` after kill.
- **Bounded output.** Cap stdout and stderr (256 KiB each) before parsing.
- **Drain both streams concurrently.** Reading stdout while stderr fills its
  pipe deadlocks.
- **Generation counter.** A result is discarded if the mode or the configured
  command changed since it was launched.
- **Always deliver completion.** Spawn failure, read failure, parse failure,
  timeout and worker panic all send a completion event. An in-flight flag that
  can stick means polling silently dies forever.
- **Shutdown cancellation.** The generic child tracker only calls `try_wait`
  (`src/app/runtime.rs:12`) and does not kill children on exit. The poller
  registers its own shutdown hook.

### Result state

Retaining the previous rows on failure is not enough on its own; the UI must say
so. State carries `last_success`, `last_attempt`, `last_error`, `is_stale`. A
*valid empty* result clears rows. Empty stdout, malformed JSON, non-zero exit,
timeout, schema mismatch and oversized output all retain prior rows and mark
them stale, with the last-success time shown in the header.

Internal events default to render-impact (`src/app/api.rs:61`) and both loops
mark rendering needed (`src/app/mod.rs:1121`, `src/server/headless.rs:785`), so
the handler reports impact correctly rather than calling render-dirty and
client-notify by hand.

## Provider protocol

Fixed, versioned schema. Rejected the alternative of a column-mapping DSL:
herdr needs stable identity, bounded columns, validation and predictable action
values, and a pass-through protocol pushes that complexity into config while
making adversarial output harder to contain.

```json
{
  "version": 1,
  "title": "JOBS",
  "summary": "2R  1Q  1✓",
  "groups": [
    { "id": "running", "label": "Running", "rows": [
      { "id": "55241874",
        "cells": ["ued", "4N", "1:23:45"],
        "style": "normal",
        "vars": { "log": "/pscratch/sd/j/jdgeorga/ued/slurm-55241874.out",
                  "dir": "/pscratch/sd/j/jdgeorga/ued" },
        "actions": ["cancel", "tail"] } ] } ],
  "notify": [ { "id": "done-55241874", "level": "ok", "text": "55241874 (ued) finished" } ]
}
```

Rules, all of which the parser enforces:

- `version` required; a mismatch is an error, not a warning.
- Unknown object fields are ignored; unknown enum values (`style`, `level`) fall
  back to `normal`/`info` with a diagnostic.
- Row `id` is globally unique across groups. Duplicates: first wins, rest
  dropped with a diagnostic. Same for group and notify ids.
- One invalid row is dropped; the rest of the payload is still applied.
- `style` is a name resolved against config, never a color.
- `notify[].id` is required and used for deduplication — a provider that
  re-reports the same event on the next poll must not re-toast.
- Caps: 200 groups, 2000 rows, 16 cells/row, 32 vars/row, 1 KiB per string,
  256 KiB total. Exceeding any cap fails the payload as oversized.
- All strings are stripped of control characters and escape sequences before
  they reach the renderer. A job name is untrusted input.
- `summary` renders on the collapsed header line.

### Substitution

| Token | Resolves to |
| --- | --- |
| `{id}` | the row's `id` |
| `{cell0}`, `{cell1}`, … | the row's `cells` by index |
| `{<name>}` | the matching key in the row's `vars` |
| `{mode}` | the section's current mode (`command` only) |

`{{` and `}}` are literal braces. `vars` may not shadow `id`, `cellN` or `mode`.
An unresolved token refuses the action and toasts which token was missing,
rather than executing a literal `{log}`.

## Actions

`Mode::ConfirmListAction` is a unit variant, matching how every other
confirmation in this codebase works. The payload lives in a separate state
struct, mirroring `WorktreeRemoveState`:

```rust
pub struct ListActionConfirmState {
    pub action_id: String,
    pub label: String,
    pub argv: Vec<String>,      // fully resolved
    pub cwd: Option<PathBuf>,   // fully resolved
    pub prompt: String,
    pub generation: u64,
    pub in_progress: bool,
    pub error: Option<String>,
}
```

argv and cwd are resolved **when the menu item is chosen** and frozen. Never
re-resolve after confirmation: the row may be gone, its id reused, or the config
reloaded. Re-resolving is a time-of-check/time-of-use bug.

Confirmation is **Enter to accept, Escape to cancel**, matching the existing
modals — not y/n.

**Tail-log** uses `spawn_overlay_argv_command`
(`src/app/input/navigate.rs:1045`), which takes `argv`, `cwd`, `extra_env` and
temp-file guards. It creates a real PTY-backed pane and splits the pane tree;
the caller must integrate the returned `NewPane` as the scrollback caller does
(`src/app/input/navigate.rs:949`). Closing the pane terminates `tail` through
the pane runtime's HUP/TERM/KILL sequence (`src/pane.rs:1229`).

### Security

argv execution avoids shell metacharacter injection, but the boundary that
matters is untrusted *provider output* interpolated into trusted *user config*.
Controls:

- No substitution in `argv[0]`. The executable is config-only.
- `--` before substituted positionals: `["scancel", "--", "{id}"]`,
  `["tail", "-f", "--", "{log}"]`. Without it a row id of `-A` is a flag.
- Per-action validation: `{id}` for `cancel` must match `^[0-9]+(_[0-9]+)?(\+[0-9]+)?$`
  (plain, array and heterogeneous job ids). Not one generic regex for everything.
- `{log}` and `{dir}` must be absolute, canonicalized, and existing. `tail` will
  happily display any file the user can read.
- Never invoke a shell.
- Paths in config are absolute or `~`-expanded explicitly. Argv execution does
  no tilde expansion (herdr expands `~` only in coded paths such as
  `src/worktree.rs:56`), so rev 1's `"~/.config/..."` example was unrunnable.

## Configuration

```toml
[ui.sidebar.list]
enabled = true
placement = "bottom"
collapsed = true
refresh_seconds = 10
timeout_seconds = 5
max_visible_rows = 12
command = ["/usr/bin/python3.11", "~/.config/herdr/scripts/herdr-jobs.py", "--mode", "{mode}"]
modes = ["live", "history"]

columns = [
  { width = "fill", align = "left"  },
  { width = 3,      align = "right" },
  { width = 7,      align = "right" },
]

[ui.sidebar.list.styles]
ok = "#b8bb26"; fail = "#fb4934"; warn = "#fabd2f"
muted = "#928374"; normal = "#ebdbb2"

[[ui.sidebar.list.actions]]
id = "cancel"; label = "Cancel job"
command = ["scancel", "--", "{id}"]
confirm = "Cancel job {id} ({cell0})?"
validate = { id = "^[0-9]+(_[0-9]+)?(\\+[0-9]+)?$" }

[[ui.sidebar.list.actions]]
id = "tail"; label = "Tail log"
command = ["tail", "-f", "--", "{log}"]
target = "overlay"
cwd = "{dir}"
```

`config.toml` paths are `~`-expanded by the config loader before argv
construction. Exactly one list section in v1.

Cell rendering uses `truncate_end` (`src/ui/text.rs:3`) against
independently-computed column rects. `resolved_token_spans`
(`src/ui/sidebar.rs:1003`) is **not** reused — it is bound to sidebar
`ResolvedTokenKind` semantics with fixed-width special cases and round-robin
width distribution, and exposing it would couple the Jobs protocol to the
Agents/Spaces token system.

## Notifications

`AppState.toast` is a single `Option<ToastNotification>` slot
(`src/app/state.rs:1484`) and `ToastKind` has three variants, none general.
Provider notifications go through a bounded FIFO (cap 8, oldest dropped) drained
into that slot as it frees. Deduplication is by `notify[].id` against a
last-seen set, so a provider that keeps reporting an event does not re-toast.
Levels map onto the existing kinds; a new kind is added only if none fits.

Desktop notifications are not used: herdr's system path shells out to
`notify-send` gated on `$DISPLAY`/`$WAYLAND_DISPLAY`, a silent no-op over SSH.

## Persistence

Session snapshots already persist sidebar width, split ratio and collapsed
Spaces state (`src/persist/snapshot.rs:14`, `:251`). Add `jobs_collapsed` and
`jobs_mode`, both `#[serde(default)]` so existing snapshots load unchanged.
Collapse state persisting is the point — the section is collapsed by default, so
expanding must be a one-time act.

## SLURM provider behaviour

```
squeue -u $USER -h -o '%i|%j|%t|%P|%q|%D|%M|%L|%l|%S|%r|%Z|%o|%e'
```

Measured ~35 ms. `%i` id, `%j` name, `%t` state, `%P` partition, `%q` QOS,
`%D` nodes, `%M` elapsed, `%L` time left, `%l` limit, `%S` start, `%r` reason,
`%Z` workdir, `%o`/`%e` stdout/stderr paths.

Grouping by `%t`: `R` → Running (show `%L`), `PD` → Queued (show `%l`).

Time fields are pre-formatted strings. They gain a `D-` prefix past 24 hours and
can be `UNLIMITED` or `N/A`. Do not assume `HH:MM:SS`.

The directory label is the last one or two components of `%Z`; no `~`-collapse
helper exists in the codebase for subpaths, so the provider does it.

`%o`/`%e` are unresolved templates (`slurm-%j.out`) for pending jobs, so `tail`
is omitted from `actions` on pending rows and the menu item is hidden.

### Linger, history, notifications

Each poll's ids are compared against the previous poll's. Ids that disappeared
get one `sacct -j <id> --format=State,ExitCode -n -P` lookup and enter a Done
group: `COMPLETED` → `ok` ✓, `FAILED` → `fail` ✗, `TIMEOUT` → `warn` TO,
`CANCELLED` → `muted` CA, each with a `notify` entry keyed `done-<id>`. They age
out at 10 minutes. sacct runs only on transitions, never on the steady poll.

History mode returns the last 10 finished jobs from sacct in the same row
schema, so the renderer needs no special case.

State file: a private per-user directory (`0700`), not a predictable shared
`/tmp` path — otherwise concurrent herdr instances race and the filename is
symlink-attackable. Written atomically (temp file plus rename) under an flock.
Node-local, since `$HOME` is shared across NERSC login nodes and this is
per-node runtime state.

## Testing

- `parse_provider_output` — pure function over a JSON string. Valid payloads,
  version mismatch, unknown fields, duplicate ids, one bad row among good ones,
  every cap boundary, control characters, empty-but-valid.
- Substitution — each token kind, `{{` escaping, unresolved token refusal,
  `vars` shadowing refusal, argv[0] substitution refusal.
- Action validation — job-id regex accepts plain/array/het ids and rejects `-A`;
  non-absolute and non-existent `{log}` rejected.
- `SidebarLayout` — expanded and collapsed carves independently; every row of
  the degradation table; the toggle row always survives; tiny heights (compare
  `expanded_sidebar_sections_handle_tiny_heights`, `src/ui/sidebar.rs:2506`).
- **Render/hit-test agreement** — for a generated set of sidebar sizes, every
  rect the renderer draws a row into is the rect the hit-tester maps that row
  from. This is the regression test for the class of bug rev 1 would have
  shipped.
- Divider drag — dragging to row N yields a ratio that renders the divider back
  at row N, with Jobs allocated.
- Regression — with `enabled = false`, all pre-existing sidebar tests pass
  unchanged.
- Poller — timeout kills the process group; a panicking worker still clears
  in-flight; a stale-generation result is discarded; oversized output marks
  stale without clearing rows.
- Provider script — squeue parsing and linger expiry against recorded fixtures.

## Build

Two prerequisites, both resolved; `build-env.sh` at the repo root must be
sourced for every build.

1. No system rustc/cargo. `module load rust/stable` resolves to 1.96.1, matching
   `rust-toolchain.toml`, but the module's `RUSTUP_HOME` is read-only shared
   software and errors on use. Redirect `RUSTUP_HOME`, `CARGO_HOME` and
   `CARGO_TARGET_DIR` to pscratch; `$HOME` is at 73% of 40 GiB.
2. **`build.rs` requires zig.** herdr vendors libghostty-vt and `build.rs:63-80`
   shells out to `zig build`, panicking with a bare `NotFound` if absent.
   `vendor/libghostty-vt/build.zig.zon` requires >= 0.15.2 and there is no NERSC
   module. Pinned 0.15.2 to `$PSCRATCH/tools`, with `ZIG_GLOBAL_CACHE_DIR` and
   `ZIG_LOCAL_CACHE_DIR` redirected off `$HOME`. Pinned rather than tracking
   latest because zig breaks builds across minor versions.

Verified: stock v0.8.0 builds clean in 2m26s cold, producing `herdr 0.8.0`.

## Constraints

**Upstreaming is gated.** `CONTRIBUTING.md` auto-closes unsolicited
implementation PRs; only `APPROVED_CONTRIBUTORS` accounts may submit, after a
maintainer-approved Discussion. Plan the fork as permanent. If upstreaming is
attempted later, the generic list section is the submittable artifact; the SLURM
provider is not.

**`herdr update` overwrites by path.** It atomically replaces whatever sits at
`env::current_exe()`. Replacing `~/.local/bin/herdr` requires disabling the
update check, and the stock binary must be preserved as `herdr-stock` first — it
is the only fallback if a rebase breaks.

**Rebase surface.** Concentrated in `src/ui/sidebar.rs`,
`src/app/input/sidebar.rs`, `src/app/input/mouse.rs`, `src/ui.rs`,
`src/config/sidebar.rs` and `src/server/headless.rs`. Keep new code in new files
where possible; the `SidebarLayout` refactor is the unavoidable exception.

## Out of scope for v1

- Keyboard navigation of the Jobs list. No focus concept exists for a sidebar
  list not backed by a pane; Spaces and Agents both piggyback on real focus.
  The context menu does have keyboard support once open.
- Multiple named list sections.
- Draggable Jobs height (fixed cap plus scrollbar instead).
- Top placement.
- Any cluster other than Perlmutter.
- Job submission.
