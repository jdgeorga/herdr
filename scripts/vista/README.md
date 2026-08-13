# Building and running herdr on TACC Vista

A herdr server that lives on your `idev` job's compute node for the life of the allocation,
plus `herdr-attach` to jump back into it from any login node, any number of times, including
after the ssh connection drops.

This is the Vista counterpart of `scripts/frontera/README.md`. **Read the "What Vista does not
need" section before adding anything to this directory.** Most of Frontera's machinery exists
to work around one dead syscall on a 2014 kernel, and Vista does not have that problem.

Everything below was measured on Vista on 2026-08-13 from compute node `i614-013`, job `909983`
(`idv18291`), partition `gg`. Where a fact was NOT measured, it says so.

## The problem in one paragraph

`vista.tacc.utexas.edu` round-robins across login nodes, so a herdr server on "the login node"
is a coin flip away from where your session actually is. The compute node in an `idev`
allocation is reachable from every login node for the life of the job, which makes the login
node pure transit. But `$HOME` on Vista is **NFS shared by every node**, so herdr's default
socket location is visible from hosts where the server does not exist — and herdr treats a
visible-but-refusing socket as stale and deletes it. Moving only the socket to node-local
`/tmp` fixes that. Nothing else needs to move.

## What Vista actually is

| | measured value | command |
|---|---|---|
| kernel | `5.14.0-611.16.1.el9_7.aarch64+64k` | `uname -r` |
| distro | Rocky Linux 9.7 (Blue Onyx) | `/etc/os-release` |
| glibc | 2.34 | `ldd --version` |
| bash | `5.1.8(1)-release` | `bash --version` |
| arch | `aarch64` (NVIDIA Grace), 144 CPUs | `arch`, `SLURM_CPUS_ON_NODE` |
| cgroups | v2 (`cgroup2fs`) | `stat -fc %T /sys/fs/cgroup` |
| `$HOME` | `/home1/08526/jdgeorga`, **NFS** `192.168.16.21:/vista/home1` | `stat -f -c %T` |
| `$WORK` | `/work/08526/jdgeorga/vista`, **Lustre** | `stat -f -c %T` |
| `$SCRATCH` | `/scratch/08526/jdgeorga`, **NFS**, 90% full | `df -h` |
| `$TMPDIR` | `/tmp`, **node-local xfs** on `/dev/mapper/rootvg01-lv_tmp`, 286G | see below |
| login nodes | `login1` (129.114.16.11), `login2` (129.114.16.9) — **two only** | `getent hosts` |
| `curl` TLS | works (7.76.1 / OpenSSL 3.5.1) | `curl -sI https://github.com` |
| system python3 | 3.9.25 — **below the 3.11 gate** | `python3 --version` |
| zig | none on `PATH`, no Lmod module | `module spider zig` |
| rust | none | `command -v rustc` |
| XALT | 3.1 loaded, `ld` is its bash wrapper | `type ld` |

Quotas — note the **file** counts, not just bytes:

```
$HOME    (quota -s)      11169M / 23842M space      137k / 500k FILES
$WORK    (lfs quota)     309G   / 1T     space      245539 / 3000000 files
```

`$HOME` has ~12.6G and ~363k inodes free. That is not comfortable for a rustup toolchain plus
a release target dir, so the build tree goes on `$WORK`. Same conclusion as Frontera, different
reason: Frontera's `/home1` is Lustre near a 200k inode cap; Vista's is NFS with a 500k file
cap and a tight space quota.

### ulimits

Both measured, the login-node column on `login2`:

| | compute node `i614-013` | login node `login2` |
|---|---|---|
| `ulimit -u` | **16384** | **100** (soft *and* hard) |
| `ulimit -v` | **unlimited** | **8388608 KB = 8 GiB** (soft *and* hard) |
| `ulimit -n` | 256000 | 16384 |
| cores | 144 | 144 |

Frontera's hard rule — *never build or run anything thread-heavy on a login node* — holds here
and is if anything tighter: Frontera allows 300, Vista allows **100**, and the limit is a hard
one so it cannot be raised. It counts tasks (threads), not processes: `login2` was sitting at
**51 threads across only 16 processes**, i.e. half the budget gone at idle. A single `claude` is
~40 threads. The compute node is where work belongs — `ulimit -u` there is 16384, and the
release build peaked around 560 threads without trouble.

### `nproc` lies here

`nproc` reports **1** on the compute node. That is not an affinity restriction:

```
nproc                 -> 1
nproc --all           -> 144
taskset -pc $$        -> current affinity list: 0-143
OMP_NUM_THREADS       -> 1        # set by TACC's default environment
```

GNU `nproc` honours `OMP_NUM_THREADS`; cargo uses `sched_getaffinity`, so it correctly used all
144 cores (observed: 560 threads across the build). **Do not pass `-j $(nproc)`** to anything
here — you would serialise it.

## First-time setup

No CI, no release asset, no provenance file, no browser step. Compare with Frontera's
five-step preamble.

```bash
# 1. clone the fork and take the Vista branch
git clone https://github.com/jdgeorga/herdr.git ~/herdr
cd ~/herdr && git checkout herdr-slurm-vista

# 2. toolchains onto $WORK (aarch64; nothing built for Frontera is reusable)
PFX=/work/08526/jdgeorga/vista/herdr-build
mkdir -p "$PFX"/{rustup,cargo,zig}
export RUSTUP_HOME="$PFX/rustup" CARGO_HOME="$PFX/cargo"
curl -sSfL -o "$PFX/rustup-init" \
  https://static.rust-lang.org/rustup/dist/aarch64-unknown-linux-gnu/rustup-init
chmod +x "$PFX/rustup-init"
"$PFX/rustup-init" -y --no-modify-path --default-toolchain 1.96.1 \
  --profile minimal --component clippy,rustfmt        # version from rust-toolchain.toml

curl -sSfL -o "$PFX/zig/zig.tar.xz" \
  https://ziglang.org/download/0.15.2/zig-aarch64-linux-0.15.2.tar.xz
tar -C "$PFX/zig" -xf "$PFX/zig/zig.tar.xz"           # 0.15.2 from build.zig.zon

# 3. a python >= 3.11 that runs standalone (see "The sidebar provider")
mkdir -p "$PFX/python-dl" && cd "$PFX/python-dl"
curl -sSfL -o py.tar.gz 'https://github.com/astral-sh/python-build-standalone/releases/download/20260807/cpython-3.12.13%2B20260807-aarch64-unknown-linux-gnu-install_only.tar.gz'
tar xf py.tar.gz && rm -rf "$PFX/python" && mv python "$PFX/python"

# 4. PROVE ZIG RUNS before starting a 600-crate build. 30 seconds, saves an hour.
"$PFX/zig/zig-aarch64-linux-0.15.2/zig" version     # -> 0.15.2
d=$(mktemp -d) && cd "$d" && "$PFX/zig/zig-aarch64-linux-0.15.2/zig" init \
  && "$PFX/zig/zig-aarch64-linux-0.15.2/zig" build && echo ZIG OK && cd / && rm -rf "$d"

# 5. site config, then install
mkdir -p ~/.config/herdr-slurm
ln -sf ~/herdr/scripts/vista/site.env   ~/.config/herdr-slurm/site.env
ln -sf ~/herdr/scripts/vista/bashrc.sh  ~/.config/herdr-slurm/bashrc.sh
cd ~/herdr
./site/install.sh --dry-run --link-herdr    # read this before installing
./site/install.sh --link-herdr
ln -sfn ~/herdr/scripts/vista/herdr-attach.sh ~/.local/bin/herdr-attach
herdr site
```

`--link-herdr` **is not optional.** It defaults to off, and without it `~/.local/bin/herdr`
keeps pointing at the raw binary, which reads `~/.config/herdr/config.toml` instead of the
`~/.config/herdr-slurm/config.toml` the installer links. Only `site/bin/herdr-slurm` exports
`HERDR_CONFIG_PATH`. Confirm against the **running server**, not `herdr --help` (that path
execs before the export):

```bash
tr '\0' '\n' < /proc/$(pgrep -u $USER -x herdr | tail -1)/environ | grep HERDR_CONFIG_PATH
# HERDR_CONFIG_PATH=/home1/08526/jdgeorga/.config/herdr-slurm/config.toml
```

Add one line to `~/.bashrc` (the file it sources holds the socket wrapper and the
`XDG_CONFIG_HOME` repair guard):

```bash
[ -r ~/.config/herdr-slurm/bashrc.sh ] && . ~/.config/herdr-slurm/bashrc.sh
```

## Building

On the **compute node**, never a login node.

```bash
cd ~/herdr
source scripts/vista/env.sh          # -> vista env: rust=1.96.1 zig=0.15.2 target_dir=...
cargo build --release --locked
```

Measured: **2m26s wall, 8m03s user, exit 0** on `i614-013`. Output
`/work/08526/jdgeorga/vista/herdr-build/target/release/herdr`, 22111128 bytes, `ELF 64-bit LSB
pie executable, ARM aarch64`, `herdr 0.8.0`. `ldd` shows no unresolved libraries.

Real zig built the vendored `libghostty-vt` natively — `vendor/libghostty-vt/zig-out/lib/libghostty-vt.a`,
15714140 bytes. That single fact is why this directory is small.

`build.rs` already maps `aarch64-unknown-linux-gnu` to the zig target `aarch64-linux-gnu`
(`build.rs::zig_target`), so no upstream file needed touching.

## Running it

Start the server **from inside the job shell** — this matters, see the cold-start section.

```bash
herdr                       # on the compute node, inside the idev shell
```

Then from any login node, as often as you like:

```bash
herdr-attach                # resolves the job's node, ssh's there, execs herdr
herdr-attach -n             # dry run: show the node and the exact remote command
herdr-attach -j 909983      # pick a specific job
```

Job auto-pick order is: the only RUNNING job, else the one named `idv*`, else list candidates
and exit. **Vista's `idev` does name jobs `idv*`** — measured `idv18291` and `idv78113` — so the
Frontera heuristic transfers unchanged.

### Killing stray ssh processes on a login node can cancel your job

This one is destructive and was learned the hard way: a broad
`for p in $(pgrep -u $USER -x ssh); do kill -9 $p; done` on `login2` **cancelled job 909983.**

`idev` holds the allocation open from the login node and keeps an ssh into the compute node:

```
$ ps -o args= -p <pid>
ssh -Y -A -o StrictHostKeyChecking no i614-051
```

Kill that and the job dies with it — `sacct` showed `CANCELLED`, and the compute node then
refused connections with `Access denied: user jdgeorga has no active jobs on this node`. On a
compute node `pgrep -x ssh` matches only your own test connections, so the same habit looks
safe there and is not safe on a login node.

When cleaning up an attach, kill the PID you captured at launch, and print what it is before
killing it:

```bash
# capture at launch
herdr-attach & MY=$!
# ... later, verify before killing
ps -o args= -p "$MY"
kill "$MY"
```

Nothing durable is lost if it happens — `session.json`, the binary and the toolchains are all
on shared storage — but the live panes and the server go, and a new job on a different node gets
a fresh per-host session name.

### You cannot start a job-aware server from a login node

The server must be created from **inside the job shell**, and there is no login-node shortcut.
`ssh` into the compute node gives 0 `SLURM_*` vars, and joining the running job with
`srun --jobid=<id> --overlap` does not work either — Vista's `job_submit` plugin rejects it as a
fresh submission, demanding `-p`, then `-N`, then `-t` in turn:

```
--> Submission error: all jobs must have a queue name specified with "-p"
--> Submission error: please define total node count with the "-N" option
--> Submission error: all jobs must have a maximum runlimit defined with "-t"
```

Do not keep feeding it arguments to get past that — satisfying the plugin risks **allocating a
second job** rather than joining the existing one. Type `herdr` in the idev shell instead.

### Why herdr's state must not live on `/home1`

This is the destructive one, and it applies to Vista for the same reason as Frontera. The
mechanism is a **shared filesystem**, not Lustre specifically: Frontera's `/home1` is Lustre,
Vista's is NFS, and both are visible from every login and compute node.

herdr's data dir defaults to `~/.config/herdr`, so a socket there is visible from hosts where
the server process does not exist. Connecting returns `ECONNREFUSED`, and
`src/ipc.rs::prepare_socket_path()` classifies `ConnectionRefused | NotFound | TimedOut` as a
stale socket: **it deletes the file and starts a second server.** The original keeps running
with your panes inside it, now permanently unreachable. `session.json`, both logs and
`.plugins.lock` would also be written by two servers at once.

The fix is to relocate **only the socket**:

```bash
HERDR_SOCKET_PATH="${TMPDIR:-/tmp}/herdr-slurm-$USER-$(hostname -s)/herdr.sock"
```

That path is not arbitrary — it matches `site/bin/herdr-slurm:113` exactly. Keep it
**byte-identical** across the launcher, the `herdr()` function in `scripts/vista/bashrc.sh`,
and anything else you add: `herdr ls` discovers sessions by this path, so divergence *hides
sessions* rather than erroring. Verified identical on Vista:

```
launcher (herdr-slurm:113): /tmp/herdr-slurm-jdgeorga-i614-013
bashrc   (bashrc.sh):       /tmp/herdr-slurm-jdgeorga-i614-013
```

**Never relocate the data dir with `XDG_CONFIG_HOME`.** It is not herdr-specific, and herdr
exports its whole environment into every pane, so every process in every pane then looks for
its config inside herdr's state dir. On Frontera this made `gh auth status` report "not logged
into any GitHub hosts" with `~/.config/gh/hosts.yml` intact. `scripts/vista/bashrc.sh` carries
the repair guard that unsets an inherited value pointing into a herdr dir.

Two more:

- **Avoid the `--session` FLAG**; the `HERDR_SESSION` env var is fine. `src/session.rs` sets
  `EXPLICIT_SESSION_REQUESTED` only for the flag, and `active_api_socket_path()` checks that
  before the environment, so the flag drags the socket back onto shared storage. The
  launcher's `HERDR_SESSION=slurm-<host>` is safe and gives per-host durable state.
- **Prefer one server with several workspaces** over several servers.

### `$TMPDIR` on a Vista compute node is genuinely node-local

The entire design rests on this, so it was checked rather than assumed:

```
$ df -h /tmp
/dev/mapper/rootvg01-lv_tmp  286G  2.1G  284G   1% /tmp

$ stat -f /tmp
Type: xfs   Inodes: Total: 149704704

$ readlink -f /tmp
/tmp                                   # NOT a symlink into /home1 or /work

$ findmnt -no SOURCE,FSTYPE,TARGET /tmp
/dev/mapper/rootvg01-lv_tmp xfs /tmp   # its own block device

$ stat -c %d /tmp $HOME /work
64769   46   2903758926              # distinct devices; home and scratch share dev 46
```

`/tmp` is a dedicated LVM logical volume with its own xfs filesystem, not a bind or symlink
into shared storage. `$TMPDIR` is `/tmp` on the compute node.

The live socket directory:

```
$ ls -l /tmp/herdr-slurm-jdgeorga-i614-013/
srw------- 1 jdgeorga G-824957 0 herdr-client.sock
srw------- 1 jdgeorga G-824957 0 herdr.sock
```

And confirmed from the other side — this is the measurement the whole design rests on. Run on
`login2` while a server was live on the compute node:

```
$ ls -l /tmp/herdr-slurm-jdgeorga-i614-013/
ls: cannot access '/tmp/herdr-slurm-jdgeorga-i614-013/': No such file or directory

$ cat /tmp/herdr-node-local-proof.txt          # marker written on the compute node
cat: /tmp/herdr-node-local-proof.txt: No such file or directory

$ findmnt -no SOURCE,FSTYPE,TARGET /tmp        # login2 has its OWN /tmp volume
/dev/mapper/rootvg01-lv_tmp xfs /tmp

$ ls -ld /tmp/herdr-slurm-*                    # and its own host-keyed dir
drwx------ 2 jdgeorga G-824957 6 /tmp/herdr-slurm-jdgeorga-login2
```

The compute node's socket is invisible from the login node, so `prepare_socket_path()` can never
see it, never classify it as stale, and never delete it. The separate
`herdr-slurm-jdgeorga-login2` directory is the `$(hostname -s)` key in the path formula doing
exactly its job.

### The cold-start asymmetry — measured on Vista

A server born from `herdr-attach`'s ssh gets a bare login environment. Vista reproduces
Frontera's numbers exactly:

| server started | `SLURM_*` vars in its environ |
|---|---|
| from inside the job shell | **41** |
| cold over `ssh` | **0** |

With 0, its panes cannot run `ibrun`/`srun`/`mpirun`. So: **create the server once from inside
the job shell, then only ever reattach.** `herdr-attach` warns rather than refuses; the warning
was confirmed to fire when no server is running, and confirmed *not* to fire when one is.

A pane in the job-shell server was verified to see the job:

```
pane_host=i614-013 pane_slurm=41 pane_pid=3315051
ibrun reachable
```

### Persistence — verified, not assumed

Killing the pty that launched herdr (the stand-in for the ssh dying):

```
before:  3315004 herdr  <- client, parent = pty harness
         3315028 herdr server, parent = 3315004
after:   3315028 herdr server, PPID = 1        # reparented to init
         pgrep -u $USER -x herdr | wc -l  ->  1
         sockets intact, herdr status -> running
```

Note on that count: it is **1 when no TUI client is attached** and 2 while one is, because the
client is also named `herdr`. The invariant that matters is one *server*:
`pgrep -u $USER -f 'herdr server$' | wc -l` stayed at 1 through every test.

Reattach was then done **twice** over real ssh, against server PID 3315028 both times, and both
times came back to the same pane with the same scrollback — a marker string
`VISTA-HERDR-PROOF-909983-A1B2C3` rendered on screen in all three attaches, with the live jobs
row ticking down (`11:31` → `11:28` → `11:27`).

That first round was run from the compute node itself. To force the ssh branch there
(`herdr-attach` runs locally when `hostname -s` already equals the target node) a `hostname`
shim returning `login9` was put on `PATH`; everything else was genuine.

**Then it was redone from a real login node**, which is the case that actually matters. From
`login2`, against job 910283 on `i614-051` with a server started from inside the idev shell:

```
herdr-attach -n     -> job 910283 (idv78113): i614-051, 11:58:25 left
                       would run: ssh -t i614-051 '<script as the command argument>'
attach #1           -> marker LOGIN2-REATTACH-910283-174000 on screen,
                       pane reporting host=i614-051 slurm=41 job=910283,
                       live jobs row, NO cold-start warning
kill my ssh         -> server survives
attach #2           -> same panes, same scrollback, same marker
```

The server was provably the *same process* across both attaches, not a restart:

```
pid=1896931  starttime_ticks=905807513  lstart=Thu Aug 13 17:39:41 2026   (attach #1)
pid=1896931  starttime_ticks=905807513                                    (attach #2)
server count: 1
```

And the load-bearing hazard is disproved from the far side. From `login2`:

```
$ ls -l /tmp/herdr-slurm-jdgeorga-i614-051/
ls: cannot access ...: No such file or directory
```

The compute node's socket is **not visible from the login node**, so the cross-host
stale-socket deletion cannot happen. `login2` has its own separate
`/tmp/herdr-slurm-jdgeorga-login2/`, which is the host-keying in the path formula working as
intended.

### State across job endings

Live processes cannot survive the allocation ending; SLURM kills them. **Layout and `cwd`
can.** `~/.config/herdr/sessions/slurm-<host>/session.json` is on NFS `$HOME`, already durable.

Verified by stopping the server and restarting it from an unrelated directory:

```
session.json recorded:  "cwd": "/scratch/08526/jdgeorga/herdr-cwd-probe"   (x7 panes)
server relaunched from: /tmp
result:                 7 panes at /scratch/08526/jdgeorga/herdr-cwd-probe
```

So restore applies the **recorded** `cwd`, not the launching shell's. Layout came back intact
every time: 7 panes, identical pane ids, 6 tabs with their labels (`Visual Layout`,
`Color Config`, `Sys3`, `Cleanup crew`). `[session] resume_agents_on_restore` defaults to
`true` (`src/config/model.rs:279`).

Also measured: `herdr server stop` **does** persist `session.json` (mtime advanced 16:52 →
17:01); `kill -9` does not, so state stays at the last clean save.

**One unexplained observation, recorded rather than smoothed over.** On one cold-over-ssh
restart, `session.json` held `/scratch/08526/jdgeorga` for all 7 panes but they came back at
`/home1/08526/jdgeorga`, and the next clean shutdown then persisted `/home1`. A controlled
retest (above) showed `cwd` restore working correctly, and `/scratch` was verified reachable
and writable from a fresh ssh session and is a static NFS mount, not an automount — so the
obvious explanations are ruled out and the cause is not established. Treat `cwd` after a
*cold* attach as worth a glance.

## The `site/` layer on Vista

Everything cluster-specific funnels through `site/lib/herdr-site.sh`, which resolves each value
by: environment → `~/.config/herdr-slurm/site.env` → runtime probe → default. Vista needs
**three** of Frontera's four overrides. `herdr site` on Vista, with no warnings and nothing
`unresolved`:

```
VALUE                  RESOLVED                                             FROM
REPO                   /home1/08526/jdgeorga/herdr                          probe: library location
BIN_DIR                /home1/08526/jdgeorga/herdr/site/bin                 probe: library location
FORK_BIN               /work/.../herdr-build/target/release/herdr           site.env
STOCK_BIN              /home1/08526/jdgeorga/.local/bin/herdr-bin           default: ~/.local/bin/herdr-bin
LOGIN_PREFIX           login                                                site.env
LOGIN_PATTERN          ^login[0-9]+$                                        site.env
PYTHON                 /work/.../herdr-build/python/bin/python3             site.env
CGROUP_MODE            v2                                                   probe: cgroup.controllers present
CGROUP_DIR             /sys/fs/cgroup/user.slice/user-878254.slice           probe: cgroup v2 layout
```

`LOGIN_PREFIX` and `LOGIN_PATTERN` must **both** be set: the pattern is probed independently, so
overriding the prefix alone leaves a stale pattern and does nothing. The probe strips
`hostname -s` at the first digit, which from `i614-013` yields prefix `i` and pattern
`^i[0-9]+$` — rejecting every real login node. Vista has exactly two, `login1` and `login2`;
`login3`/`login4` do not resolve. `vista.tacc.utexas.edu` is neither of them — it round-robins
129.114.63.161 and .162 — but `hostname -s` after login is `login1`/`login2`, which the pattern
matches.

### The multi-host fan-out commands do not work on Vista at all

`herdr ls` and `herdr health` with no arguments discover hosts and drop anything not matching
`^login[0-9]+$`. From `i614-013` that discovers **zero** hosts, so both print an empty table.
This is correct behaviour given the pin, not a bug, but it is surprising.

There is a second, more fundamental reason the multi-host forms do not work, and it would bite
even with a permissive pattern: `herdr_site_fanout()` uses `ssh -o BatchMode=yes`, and **Vista
requires MFA for login nodes.** Measured in both directions:

```
i614-013 -> login1/login2 :  Permission denied (keyboard-interactive)
login2   -> login1        :  Permission denied (keyboard-interactive)
login2   -> i614-051      :  works (this is the direction herdr-attach needs)
```

So `BatchMode=yes` can never reach a login node **from anywhere**, not just from a compute node:
every login host lands in `HERDR_SITE_UNAVAILABLE` and the command exits nonzero. Treat
`herdr ls`, `herdr health`, `herdr cleanup` and `herdr reap` as **`--local`-only on Vista,
everywhere.** Do not patch `site/` for this. Use `--local`:

```
$ herdr ls --local
i614-013  slurm-i614-013  slurm  20  /tmp/herdr-slurm-jdgeorga-i614-013/herdr.sock
```

### The two bash 4.2 fixes in `site/lib/herdr-site.sh` stay

Lines 80 and 251 guard `"${arr[@]}"` on a possibly-empty array with
`(( ${#arr[@]} > 0 ))`. Vista is bash 5.1.8 where those are harmless no-ops, but Frontera is
bash 4.2.46 where `set -u` plus `"${empty_array[@]}"` is an unbound-variable error that aborts
every `site/bin` command at startup. **Do not "simplify" them away** — that breaks Frontera.

### `herdr health` reads real numbers here — but its TASKLIMIT lies on a login node

Frontera is cgroup v1 with an unreadable per-user slice, so `herdr health` returns zeroed
columns — worse than no check, and why Frontera needs `frontera-limits.sh`. Vista is cgroup v2
on both node types and the probe's v2 branch resolves a real, readable directory, so **no
`vista-limits.sh` is needed and none is provided.** Measured directly:

```
/sys/fs/cgroup/user.slice/user-878254.slice/memory.current  6670843904
                                            pids.current    28
                                            pids.max        160525

$ herdr health --local        # compute node i614-013
SUMMARY  i614-013  10568794112  max  31  160525  0  -

$ herdr health --local        # login2
SUMMARY  login2    1946222592   max  53  160525  0  -
```

Memory and task *counts* are genuine in both. **The TASKLIMIT column is not trustworthy on a
login node.** It reports the cgroup's `pids.max` of 160525, but the binding constraint there is
`ulimit -u = 100` — hard. So the row above reads "53 of 160525" when the truth is **53 of 100**,
which is worse than no number at all: it says you have room when you are half out.

This is Frontera's failure mode reappearing for a different reason — not an unreadable cgroup,
but a cgroup whose limit is not the real ceiling. `herdr_site_cgroup_read()` has no way to know
that; the ulimit is invisible to it. On the compute node the two agree well enough to ignore
(`pids.max` 160525 vs `ulimit -u` 16384, and real usage is orders below both). On a login node,
read `ulimit -u` and `ps -u $USER -L --no-headers | wc -l` yourself instead — which is what the
thread-quota guard in `~/.bashrc` already does.

### The sidebar provider

```
$ ~/.config/herdr/scripts/herdr-jobs --mode live
{"version":1,"title":"JOBS","summary":"1R · 1N 0.4nh","groups":[{"id":"running",
"label":"Running","rows":[{"id":"909983","cells":["jdgeorga","1N","11:36:10"],
"style":"normal","vars":{"dir":"...","log":".../idv18291.o909983"},
"actions":["cancel","tail"]}]}],"notify":[]}
```

Exactly one JSON object, one live row, and `--selftest` prints `SELFTEST OK`. Vista runs
**slurm 23.11.11** — the same major as Frontera, and not the 25.11 the provider was written
against; it needed no changes. Do not edit `scripts/herdr-jobs.py`, which is shared.

Rendered in a real 50x200 session:

```
▾ JOBS               [live|history]
▾ Running
jdgeorga                 1N 11:31:…
```

`HERDR_SITE_PYTHON` points **straight at an interpreter** — there is no wrapper script here and
there should not be one. Frontera needs `python3-frontera.sh` because its module python needs a
lib dir, the Intel runtime, and a libssp ordering fix. Vista's situation is different but not
zero: the `python3/3.11.8` Lmod module's binary is
`/opt/apps/gcc14/cuda12/python3/3.11.8/bin/python3.11` and it **does not run standalone** —

```
$ env -i PATH=/usr/bin:/bin /opt/apps/gcc14/cuda12/python3/3.11.8/bin/python3.11 --version
error while loading shared libraries: libpython3.11.so.1.0: cannot open shared object file
```

so naming it in `site.env` yields exit 127 inside the provider. A relocatable
python-build-standalone 3.12.13 on `$WORK` avoids both a wrapper and any Lmod dependency, and
was verified under the exact flags `herdr-jobs` uses (`-I -S`) with a scrubbed environment.
`-S` disables `site`, so anything pip-installed into that interpreter cannot reach the
provider.

**Two gotchas when testing the sidebar headlessly**, both hit during this port:

1. A script-spawned pty inherits a 0x0 winsize when stdin is not a tty and **herdr then draws
   nothing at all**, which looks exactly like a hung server. Set the size explicitly
   (`TIOCSWINSZ`, or `stty rows/cols`).
2. Collapsed renders no rows and looks exactly like a broken provider.

On (2), a correction to what the design spec claims.
`specs/2026-08-11-herdr-slurm-frontera-design.md:254-255` says "the shipped config now sets
`collapsed = false`". **It does not.** On this branch:

```
site/config/config.toml:117          collapsed = true
site/config/session-template.json:175  "jobs_collapsed": true
```

and the persisted session value **wins over the config value** (`src/app/mod.rs:964` applies
`snapshot.jobs_collapsed` when present). So a fresh install seeded from the template comes up
collapsed regardless of `config.toml`. Both files are shared with Frontera and were left
untouched; instead the **seeded per-user copy** was edited, which `site/install.sh` copies
rather than symlinks precisely because it is user data:

```bash
# ~/.config/herdr-slurm/session-template.json  (yours, not tracked)
"jobs_collapsed": false
```

### `herdr-attach` is a symlink to the Frontera script, deliberately

`scripts/vista/herdr-attach.sh -> ../frontera/herdr-attach.sh`

It is transport only — resolve the node, ssh, exec `herdr` — and it needed **no** Vista changes.
Socket, session and config all belong to `site/bin/herdr-slurm` on the far side, so the two can
never disagree. A symlink rather than a copy means the transport cannot drift between clusters.
Tested unchanged on Vista: job resolution, the ssh branch, reattach twice, and the cold-start
warning all behaved correctly.

Its hard-won details, all still load-bearing:

- The remote script is passed as the **ssh COMMAND argument, never on stdin**. With `ssh -t`,
  stdin is the terminal the TUI needs.
- `set --` is emitted **unconditionally** to clear positional parameters leaked by `~/.bashrc`.
  Vista's `~/.bashrc` does **not** have Frontera's `set umask 027` bug — measured `argc=0` over
  ssh — so this is inert here, but it must stay: it is what makes the script immune to whatever
  a future rc file leaves lying around. The `[ "$#" -gt 0 ] &&` guard also matters, because
  `printf '%q '` with zero arguments prints `''` and would hand herdr one empty argument.
- An **absolute path**, not `ssh -t node bash -lc`. Vista's `~/.bashrc` has no tailscaled
  autostart, so the specific noise Frontera avoids does not occur here, but sourcing a full
  login shell to launch a TUI is still the wrong default.

**Two known stale strings**, both in error branches a working install never reaches. Recorded
so they are not a surprise; judged not worth forking 150 lines of identical transport.

`herdr-attach.sh:102` — shown only when no herdr binary exists at `~/.local/bin/herdr`
(`exit 127`):

```
build it:  cd ~/herdr && source scripts/frontera/env.sh && cargo build --release
```

On Vista that should read `scripts/vista/env.sh`.

`herdr-attach.sh:54` — shown only when you have no RUNNING job:

```
idev -N 1 -n 56 -p normal -t 12:00:00
```

Both values are wrong for Vista. There is **no `normal` partition** here, and nodes have 144
cores, not 56. Vista's partitions and their real wall limits (partition `MaxTime` is
`infinite` on all of them, so the ceiling is the QOS `MaxWall` — read it from `sacctmgr`,
never from `sinfo`'s TIMELIMIT column):

| partition | QOS | MaxWall |
|---|---|---|
| `gh-dev` (**default**) | `qdevelopment` | **02:00:00** |
| `gh` | `qgh` | 2-00:00:00 |
| `gg` | `qgg` | 2-00:00:00 |
| `gb` | `qgb` | 12:00:00 |

The default partition caps at two hours, which is short for an allocation meant to host a herdr
server all day, so **name a partition explicitly**. The job this port was verified in was
`-p gg` with `QOS=qdefault` and `TimeLimit=12:00:00`.

## What Vista does not need, and why

This is the point of the port. Each of these exists on Frontera solely because of a specific
defect Vista does not have. **Do not re-add them.**

| Frontera machinery | why it exists there | why Vista does not need it |
|---|---|---|
| `zig-shim.sh` | kernel 3.10 has no `statx(2)`; Zig 0.15.x calls it, so `zig build` cannot run at all | kernel 5.14. `zig version` → 0.15.2, `zig init && zig build` → exit 0, `libghostty-vt.a` built natively |
| prebuilt `.a` release asset | same | same |
| `provenance.json` | pins the digest of that asset | no asset |
| `fetch-artifact.sh` | stages the asset onto `/work2` | no asset |
| `link-probe.sh` | proves the asset links before a 600-crate build | source build is the probe |
| `.github/workflows/frontera-libghostty-vt.yml` | builds the asset in CI | no asset |
| `python3-frontera.sh` | module python needs its lib dir, Intel runtime, and a libssp order fix | standalone python 3.12.13, named directly in `site.env` |
| `frontera-limits.sh` | cgroup v1 + unreadable slice makes `herdr health` report zeros | cgroup v2, readable, real numbers |
| `RUSTFLAGS=-C link-arg=-lrt` | glibc 2.17 keeps `shm_open` in librt | glibc 2.34 absorbed it. Verified twice: a C `shm_open` test links with no `-lrt`, and `readelf -d` on the finished herdr shows NEEDED = `libgcc_s.so.1`, `libm.so.6`, `libc.so.6` and **no `librt.so.1`** |
| `CC=/opt/apps/gcc/8.3.0/bin/gcc` pin | bare `cc` is GCC 4.8.5 with binutils 2.27 | system gcc is 11.5.0; the build linked clean unpinned |
| libssp `LD_LIBRARY_PATH` fix | Juliaup ships a broken `libssp.so.0` first on the path | not present in Vista's `~/.bashrc` |
| `XALT_EXECUTABLE_TRACKING=no` | XALT's `ld` wrapper rewrites link argv and double-links | see below — kept OFF, with evidence |
| `build.slurm` | batch build because login nodes cannot take it | built interactively on the compute node in 2m26s |

Nothing prebuilt for Frontera is reusable anyway: Vista is **aarch64**, Frontera x86_64.

### On XALT specifically

XALT *is* in the link path on Vista, exactly as on Frontera:

```
COMPILER_PATH            /opt/apps/xalt/xalt/bin
LD_PRELOAD               /opt/apps/xalt/xalt/lib64/libxalt_init.so
XALT_EXECUTABLE_TRACKING yes
gcc -print-prog-name=ld  /opt/apps/xalt/xalt/bin/ld     (a bash script)
```

So the bypass was *not* dropped on the assumption that XALT is absent. It was dropped because
the interference was measured and turned out to be benign here. XALT **does** still touch the
link — a test binary built with tracking on differs from one built with
`XALT_EXECUTABLE_TRACKING=no` (72656 vs 71488 bytes, first difference at byte 25), which is its
watermark object. What it does **not** do on Vista is append extra shared-library dependencies:

```
$ readelf -d with_xalt | grep NEEDED     ->  [libc.so.6]
$ readelf -d no_xalt   | grep NEEDED     ->  [libc.so.6]      # no -ldcgm, no -luuid
```

and the full `cargo build --release --locked` then succeeded with XALT untouched. Rust links are
static-archive-heavy and order-sensitive, so if you ever
see an inscrutable undefined-reference at the final link, add
`export XALT_EXECUTABLE_TRACKING=no` (and `unset LD_PRELOAD COMPILER_PATH`) to
`scripts/vista/env.sh` and document that it became necessary.

## What I could not verify

Stated plainly rather than inherited from the Frontera README:

- **Whether `herdr-attach` works from `login1`.** It was verified end to end from `login2`
  (twice, with the disconnect in between). `login1` was not used as a source host, but nothing in
  the path is login-node-specific and both match `^login[0-9]+$`.
- **The allocation actually ending.** Job 909983 had ~11h left. Persistence was tested by
  killing the server and deleting the sockets, which is the same code path but not the same
  event.
- **Multi-node allocations.** This job was `-N 1`. `herdr-attach` takes the first host from
  `scontrol show hostnames` and reports `[+N more]`; untested with N > 1.
- **The cgroup layout under a plain `sbatch` step.** Everything above was measured in an
  interactive `idev` session, where `/proc/self/cgroup` is
  `0::/user.slice/user-878254.slice/session-61376.scope` and the probe's
  `user.slice/user-<uid>.slice` guess is right. Under a batch step the processes may live in a
  `slurmstepd` scope instead, in which case `herdr_site_cgroup_read()` returns its neutral
  fallback (`0` / `max`) **silently** and `herdr health` would read "fine" at the ceiling — the
  same failure shape as Frontera, arrived at differently. If you start using `sbatch`, re-run
  the cgroup reads there and pin `HERDR_SITE_CGROUP_DIR` in `site.env` if needed rather than
  patching `site/`.
Unlike Frontera, where the system `curl` fails all TLS and `wget` is mandatory, **both work on
Vista from both node types** — `curl 7.76.1` with OpenSSL 3.5.1 and `wget 1.21.1` each reached
`https://github.com` from `i614-013` and from `login2`. So no `wget`-only workaround is needed
anywhere, and dependency fetches on the compute node are fine.

## Fragile spots

- The one unexplained `cwd`-after-cold-attach observation above.
- `HERDR_SITE_PYTHON`, `HERDR_SITE_FORK_BIN` and `HERDR_BUILD_PREFIX` are absolute paths
  containing `08526` and `jdgeorga`. Another user must edit `site.env` and `env.sh`.
- The python-build-standalone URL pins release tag `20260807`. That tag will age out of
  "latest" but the download URL remains valid.
- `$SCRATCH` is 90% full and TACC purges it. `session-template.json` seeds pane `cwd` from
  `${SCRATCH:-$HOME}`, so seeded panes point into a purgeable directory.
- **`herdr sync` will try to build with Perlmutter's environment.** `scripts/herdr-sync.sh:134`
  requires `$repo/build-env.sh` and line 137 sources it. That file *does* exist, but it is
  NERSC-only: it does `module load rust/stable` and points `RUSTUP_HOME`, `CARGO_HOME`,
  `CARGO_TARGET_DIR` and `ZIG` at `$PSCRATCH`, which is unset on Vista. Use:

  ```bash
  herdr sync --no-build      # then: source scripts/vista/env.sh && cargo build --release --locked
  ```

  The same file is why `site/bin/herdr-slurm:85` and `site/install.sh:200` print
  `source ./build-env.sh` as their build hint — wrong for Vista, but both are shared with
  Frontera, so they were left alone.

## Files

| file | what it is |
|---|---|
| `env.sh` | build environment: `$WORK` prefix, real `ZIG`, zig cache dirs. Source before `cargo build`. |
| `site.env` | the three `HERDR_SITE_*` overrides Vista needs. Symlink to `~/.config/herdr-slurm/site.env`. |
| `bashrc.sh` | `herdr()` socket wrapper + `XDG_CONFIG_HOME` repair guard. Symlink to `~/.config/herdr-slurm/bashrc.sh`, sourced from `~/.bashrc`. |
| `herdr-attach.sh` | symlink to `../frontera/herdr-attach.sh`; needed no changes. |
| `README.md` | this file. |

There is deliberately no `build.slurm`, no `zig-shim.sh`, no `provenance.json`, no
`fetch-artifact.sh`, no `link-probe.sh`, no `python3-vista.sh`, and no `vista-limits.sh`.

## Not supported here

- Building on a login node.
- `herdr update` — blocked on the fork; it would replace the binary with upstream's prebuilt
  and drop the jobs sidebar. Use `herdr sync --no-build` followed by a manual
  `source scripts/vista/env.sh && cargo build --release --locked`; plain `herdr sync` would try
  to build with Perlmutter's `build-env.sh` (see "Fragile spots").
- Running the server on a login node.
- Creating the server over `herdr-attach` and then expecting `ibrun` to work in its panes.
