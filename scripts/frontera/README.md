# Building and running herdr on TACC Frontera

Frontera-specific by intent. Nothing here is meant to work on another cluster.

Full reasoning: [`specs/2026-08-11-herdr-slurm-frontera-design.md`](../../specs/2026-08-11-herdr-slurm-frontera-design.md).

## The problem in one paragraph

`build.rs` shells out to `zig build` for the vendored `libghostty-vt`. Zig 0.15.x calls
`statx(2)`, added in Linux 4.11. Frontera is CentOS 7 on kernel **3.10**, so every zig
invocation — even `zig init` — dies with `error: unable to load package manifest ...
Unexpected`. `LD_PRELOAD` cannot shim it (static binary, raw syscalls) and a container cannot
help (Apptainer shares the host kernel). The Rust half is completely fine.

So: build the Zig archive in CI, and give `build.rs` something that verifies and stages it.
`build.rs` already reads a `ZIG` environment variable, so this needs **zero changes to any
upstream-owned file**.

## First-time setup

```bash
# 0. one-time, needs a browser: enable Actions on the fork
#    https://github.com/jdgeorga/herdr/actions
gh workflow list --repo jdgeorga/herdr      # must list workflows, not nothing

# 1. push the branch; CI builds libghostty-vt.a and publishes it as a release asset
git push origin herdr-slurm-frontera

# 2. record what CI produced (deliberate, reviewed commit -- never automate this)
gh release download frontera-libghostty-vt --repo jdgeorga/herdr \
   --pattern provenance.json --dir /tmp
cp /tmp/provenance.json scripts/frontera/provenance.json
git commit -am 'chore(frontera): refresh libghostty-vt provenance'

# 3. stage it on /work2
scripts/frontera/fetch-artifact.sh

# 4. prove it links BEFORE spending an hour on a 600-crate build
scripts/frontera/link-probe.sh
```

## Building

```bash
sbatch scripts/frontera/build.slurm       # normal queue, 3h, runs link-probe first
```

or interactively on a compute node:

```bash
source scripts/frontera/env.sh
cargo build --release --locked -j 24
```

Never on a login node: `ulimit -u` there is 300 and counts **threads**, so parallel rustc
trips it and reports misleading errors (`WouldBlock`, `EAGAIN`).

Put the binary on `PATH` once, as a symlink, so rebuilds need no reinstall step:

```bash
ln -sf /work2/08526/jdgeorga/frontera/herdr-build/target/release/herdr ~/.local/bin/herdr
```

## Running it

herdr is itself a multiplexer, so it **replaces** tmux here rather than running inside it.
Its server persists on the compute node for the life of the allocation.

```bash
scripts/frontera/herdr-attach.sh          # from any login node; resolves your job's node
```

That is the herdr counterpart to `idev-attach`, which does the same thing for tmux. Same job
auto-selection (the only RUNNING job, else the `idv*` one), same `-n/--dry-run` and `-j JOBID`
flags. Symlink it if you want it on `PATH`:

```bash
ln -sf ~/herdr/scripts/frontera/herdr-attach.sh ~/.local/bin/herdr-attach
```

Inherited caveat, same as `idev-attach`: a server started cold over ssh gets a bare login
environment — 0 `SLURM_*` vars, versus 41 in one started from inside the job step — so its
panes cannot run `ibrun`/`srun`. The script warns and continues. For job-aware panes, start
herdr from inside the job shell.

### Why herdr's state must not live on /home1

This is the sharpest Frontera-specific hazard in the whole setup, and it is destructive.

herdr's data dir defaults to `~/.config/herdr`, and `/home1` is Lustre — shared across every
login and compute node. So the server's **socket file is visible from hosts where the server
process does not exist.** Connecting to such a socket returns `ECONNREFUSED`, and
`src/ipc.rs::prepare_socket_path()` classifies that as stale:

```rust
Err(err) if stale_socket_connect_error(err.kind()) => {}   // ConnectionRefused | NotFound | TimedOut
...
fs::remove_file(path)
```

**It deletes the file and starts a second server.** Your original server keeps running with
your panes inside it, now permanently unreachable. Confirmed on 2026-08-11 by creating a unix
socket on `/home1` from a compute node: `login1` and `login2` both saw the file and both got
`errno=111 ECONNREFUSED`. It is the same trap that bites `tailscaled` here.

Beyond the socket, `session.json`, both logs, and `.plugins.lock` would also be written by two
servers at once.

The fix is to relocate **only the socket**:

```bash
HERDR_SOCKET_PATH=/tmp/herdr-$USER-default.sock herdr
```

`herdr-attach.sh` does this, and a `herdr()` function in `~/.bashrc` does it for bare
invocations — which matters, because `herdr-attach` only protects the paths that go through it,
and a bare `herdr` would otherwise still use the `/home1` socket. The client socket is derived
from the API socket, so one variable covers both.

**Never relocate the data dir with `XDG_CONFIG_HOME`.** This was tried and it backfired badly.
The variable is not herdr-specific, and **herdr exports its whole environment into every pane** —
so a server started with `XDG_CONFIG_HOME` hands that value to every process in every pane.
`gh`, `gcloud`, `yazi`, and `matplotlib` then look for their config inside herdr's state dir and
find nothing. On 2026-08-12 this made `gh auth status` report "not logged into any GitHub hosts"
in a live session, with `~/.config/gh/hosts.yml` perfectly intact. The socket variable is
targeted and safe; `XDG_CONFIG_HOME` is not.

`~/.bashrc` also carries a repair for this: if an inherited `XDG_CONFIG_HOME` points at a herdr
state dir, it unsets it. That fixes existing panes without restarting a server and killing live
work.

Two more things worth knowing:

- **Do not pass `--session`.** `active_api_socket_path()` checks `explicit_session_requested()`
  **before** consulting the environment, so naming a session discards `HERDR_SOCKET_PATH` and
  puts the socket back on Lustre.
- **Prefer one server with several workspaces** over several servers. herdr already has
  workspaces and tabs. Multiple servers share one `~/.config/herdr/session.json`, so the last
  one to save wins the layout — an annoyance, not corruption, but avoidable.

### State across job endings

Live processes cannot survive the allocation ending; SLURM kills them. **Layout can.**

`~/.config/herdr` is on `/home1`, so it is already durable — no relocation needed. `session.json`
records each pane's `cwd` and its `agent_session` id, and herdr restores from it at startup with
`[session] resume_agents_on_restore` (default `true`), which resumes Claude sessions by id.

Verified 2026-08-12: killed the server, deleted both sockets, restarted — herdr logged
`persist.restore outcome="ok"` and came back with the pane's `cwd` intact. So after your job
ends, starting herdr on the next node brings back the workspace/tab/pane layout and puts each
shell back in its directory.

## The SLURM sidebar

`scripts/herdr-jobs.py` was written for NERSC but runs on Frontera **unmodified** — verified
against Slurm 23.11 in both `live` and `history` modes, and `--selftest` passes. It only needs
the right interpreter, which is what `herdr-jobs-frontera.sh` provides.

```bash
cp scripts/frontera/config.frontera.toml ~/.config/herdr/config.toml
scripts/frontera/herdr-jobs-frontera.sh --mode live      # should print one JSON object
```

Two Frontera facts drive the wrapper:

- **Python.** The provider needs >= 3.7 (`from __future__ import annotations`).
  `/usr/bin/python3` is 3.6.8 and cannot parse it. `python3/3.9.2` works.
- **libssp.** Juliaup ships its own `libssp.so.0`, `.bashrc` puts it first on
  `LD_LIBRARY_PATH`, and it lacks `__vsnprintf_chk@LIBSSP_1.0` — so every Intel-built python
  dies with a relocation error. gcc 8.3.0's `lib64` has the working copy and must come first.

## Files

| File | Role |
|---|---|
| `zig-shim.sh` | Stands in for `zig build`. **Verifies, then exits 0.** Never a no-op. |
| `env.sh` | Toolchain paths, `ZIG`, XALT bypass, linker pin, libssp fix. |
| `fetch-artifact.sh` | Downloads the CI archive to `/work2`, rejects it if provenance disagrees. |
| `link-probe.sh` | 30-second go/no-go gate. Run before any full build. |
| `provenance.json` | The committed expectation the shim checks against. |
| `herdr-attach.sh` | Attach to herdr on the job's compute node, with a node-local socket. |
| `herdr-jobs-frontera.sh` | Interpreter wrapper for the SLURM sidebar. |
| `config.frontera.toml` | Sidebar config pointing at the wrapper. |
| `build.slurm` | sbatch wrapper (`normal` queue, not `development`). |

## Updating and rebasing

This branch is designed to be rebased onto upstream indefinitely. Its entire diff is new files
under `scripts/frontera/`, one workflow, and one spec — **no upstream-owned file is modified** —
so a rebase can only conflict on a file this branch created.

### Routine rebase

```bash
git fetch upstream
git rebase upstream/master          # or: git rebase origin/feat/slurm-jobs-sidebar
source scripts/frontera/env.sh
cargo build --release --locked -j 24
```

If the rebase pulled in a new `vendor/libghostty-vt`, that build stops immediately with a
digest mismatch. That is the system working; see the next section.

### Refreshing the prebuilt archive

Needed whenever `vendor/libghostty-vt/**` changes. Four steps, and the shim's error message
prints them:

```bash
# 1. push; CI rebuilds the archive and publishes it to the fork release
git push origin herdr-slurm-frontera

# 2. record what CI produced -- a deliberate, reviewed commit, never automated
gh release download frontera-libghostty-vt --repo jdgeorga/herdr \
   --pattern provenance.json --dir /tmp
cp /tmp/provenance.json scripts/frontera/provenance.json
git diff scripts/frontera/provenance.json      # read it before committing
git commit -am 'chore(frontera): refresh libghostty-vt provenance'

# 3. stage it on /work2 (refuses if the two provenance copies disagree)
scripts/frontera/fetch-artifact.sh

# 4. prove it links before a full build
scripts/frontera/link-probe.sh
```

### What CI does and does not rebuild

The archive is **not bit-reproducible** — it embeds runner paths, so two builds of identical
source differ byte for byte. If every push republished, the `archive_sha256` committed here
would be invalidated constantly and `fetch-artifact.sh` would fail for no real reason.

So the workflow keys publication on the **vendored source**, not the commit: it compares the
published provenance's `vendor_tree_sha256` against the current tree and skips the build and
upload entirely when they match. Zig is not even installed on that path. A push that does not
touch `vendor/libghostty-vt/**` finishes in about 6 seconds.

Consequence worth knowing: **you cannot force a rebuild by pushing again.** To genuinely
replace the artifact, delete the release asset (or the whole `frontera-libghostty-vt` release)
and push, or use `workflow_dispatch` after removing the published `provenance.json`.

### Keeping the two digests in sync

`zig-shim.sh` and the workflow compute the vendored-source digest independently. They must
agree exactly, and two details make that fragile — if you touch either copy, preserve both:

- `LC_ALL` must be **exported**, not used as a command prefix. `LC_ALL=C find … | sort` applies
  the locale only to `find`, leaving `sort` under the ambient locale; the two forms produce
  different digests on this tree.
- Generated directories must be **pruned** (`zig-out`, `.zig-cache`). The shim stages the
  archive into `vendor/libghostty-vt/zig-out/`, which would otherwise be inside the hashed
  tree and change the digest on the very next build.

### Upstream hygiene

Never push branches or tags to the `upstream` remote, and never open a PR or issue against
`herdrdev/herdr`. `git remote -v` has `upstream` configured, so a mistyped push target is a
live hazard. `CONTRIBUTING.md` auto-closes unsolicited PRs. Everything here belongs on the
`jdgeorga/herdr` fork.

### If `build.rs` stops honouring `ZIG`

The whole mechanism rests on `build.rs` reading `env::var("ZIG")` and only asserting the child
exits 0. If upstream changes that, the shim stops being consulted and nothing announces it. The
regression test below is the detector: if it stops failing, the guard is gone.

## When a build suddenly refuses to start

```
frontera zig-shim: FATAL: vendored libghostty-vt has changed since the archive was built.
```

Working as designed. You rebased onto an upstream that re-vendored `libghostty-vt`, so the
pinned archive no longer matches the source. Redo steps 1–3 above.

**Do not "fix" this by relaxing the check.** `src/ghostty/bindings.rs` is committed bindgen
output with 64 `#[repr(C)]` types and *zero* layout tests. A stale archive exports identical
symbol names with different struct layouts, so it links cleanly, boots, and reads garbage.
Every other failure mode in this setup is loud; this is the only silent one, and that digest
is the only thing standing in front of it.

Its regression test — which must keep failing:

```bash
printf '\n// staleness probe\n' >> vendor/libghostty-vt/src/lib_vt.zig
cargo build --release          # MUST fail with a provenance mismatch
git checkout -- vendor/libghostty-vt/src/lib_vt.zig
```

Note it must be a **content** change. `touch` alone does not trip the gate — the digest hashes
file contents, not mtimes — which is the desired behaviour but makes `touch` useless as a test.
(Verified both ways on 2026-08-11.)

If that ever *succeeds*, the guard has gone fail-open (most likely `build.rs` stopped honouring
`ZIG`) and the archive is no longer being checked at all.

## Not supported here

`just check` / `just test`. Their `windows-lint` leg cross-compiles to
`x86_64-pc-windows-msvc`, which needs a real zig regardless of this shim. The shim
deliberately **refuses** any target other than `x86_64-unknown-linux-gnu` rather than handing
a linux archive to a windows build and letting you believe the leg passed. Frontera is scoped
to `cargo build --release`.
